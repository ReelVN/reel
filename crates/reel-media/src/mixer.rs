use crate::maintenance::{
    AudioReclaimService, DeferredManager, DeferredRenderer, Envelope, Job, RetirementSignal,
    TeardownCredit, try_producer,
};
use crate::retirement::PhysicalRelease;
use crate::{AudioError, AudioErrorKind, AudioFormat, PcmFill};
use reel_audio::{
    AudioManager, AudioManagerSettings, Frame, Tween,
    backend::{Backend, Renderer},
    info::Info,
    sound::{
        PlaybackState, Sound, SoundData,
        static_sound::{StaticSoundData, StaticSoundHandle, StaticSoundSettings},
    },
    track::MainTrackBuilder,
};
#[cfg(test)]
use reel_core::wake::WakeSignal;
use reel_core::{
    budget::{BudgetPool, BudgetTicket},
    result::{RejectCode, RejectReason, Rejected},
};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

pub(crate) struct Bridge {
    sample_rate: u32,
    signal: Arc<RetirementSignal>,
    fill: Option<PcmFill>,
    renderer_credit: Option<TeardownCredit>,
}
impl Bridge {
    pub fn has_fill(&self) -> bool {
        self.fill.is_some()
    }
}
impl Backend for Bridge {
    type Settings = (u32, Arc<RetirementSignal>, TeardownCredit);
    type Error = AudioError;
    fn setup(
        (sample_rate, signal, credit): Self::Settings,
        _: usize,
    ) -> Result<(Self, u32), AudioError> {
        Ok((
            Self {
                sample_rate,
                signal,
                fill: None,
                renderer_credit: Some(credit),
            },
            sample_rate,
        ))
    }
    fn start(&mut self, renderer: Renderer) -> Result<(), AudioError> {
        debug_assert_ne!(self.sample_rate, 0);
        let signal = self.signal.clone();
        let mut deferred = DeferredRenderer::new(
            renderer,
            self.renderer_credit
                .take()
                .expect("reserved Renderer credit"),
        );
        // Call a method on the whole Drop shell, avoiding disjoint field capture.
        self.fill = Some(Box::new(move |out| deferred.render(out, &signal)));
        Ok(())
    }
}
#[derive(Clone, Copy, Debug)]
pub struct MixerLimits {
    pub max_sounds: usize,
    pub internal_buffer_frames: usize,
}
impl Default for MixerLimits {
    fn default() -> Self {
        Self {
            max_sounds: 64,
            internal_buffer_frames: 128,
        }
    }
}

/// Predecoded samples and their residency reservation. Does no file IO.
/// Ticket covers input capacity + converted Frame storage during construction,
/// and remains conservatively charged until Kira's control-side reclamation.
#[derive(Debug)]
pub struct DecodedSound {
    data: StaticSoundData,
    budget: Arc<Residency>,
}
/// Every internal PCM alias owns this same reservation. Declaration order ensures
/// the actual ticket is returned before the final physical-release publication.
#[derive(Debug)]
struct Residency {
    ticket: BudgetTicket,
    release: Option<PhysicalRelease>,
}
pub type DecodedSoundRequest = (u32, Vec<[f32; 2]>, BudgetTicket);
impl DecodedSound {
    pub fn new(
        sample_rate_hz: u32,
        frames: Vec<[f32; 2]>,
        ticket: BudgetTicket,
    ) -> Result<Self, Rejected<DecodedSoundRequest>> {
        let required = (frames.capacity() as u64)
            .checked_add(frames.len() as u64)
            .and_then(|n| n.checked_mul(8));
        if sample_rate_hz == 0 || frames.is_empty() || required.is_none_or(|n| n > ticket.bytes()) {
            return Err(Rejected {
                request: (sample_rate_hz, frames, ticket),
                reason: RejectReason::new(
                    RejectCode::InvalidRequest,
                    "invalid PCM or ticket does not cover input and converted storage",
                ),
            });
        }
        let data = StaticSoundData {
            sample_rate: sample_rate_hz,
            frames: frames
                .into_iter()
                .map(|[left, right]| Frame::new(left, right))
                .collect(),
            settings: StaticSoundSettings::default(),
            slice: None,
        };
        Ok(Self {
            data,
            budget: Arc::new(Residency {
                ticket,
                release: None,
            }),
        })
    }
    /// Loop the decoded region using Kira's transport, without duplicating PCM.
    pub fn looping(mut self) -> Self {
        self.data = self.data.loop_region(..);
        self
    }
    pub fn resident_bytes(&self) -> u64 {
        self.budget.ticket.bytes()
    }
    /// Called before admission while this request has no internal PCM aliases.
    pub(crate) fn track_retirement(&mut self, release: PhysicalRelease) {
        let residency =
            Arc::get_mut(&mut self.budget).expect("unaccepted request uniquely owns its residency");
        assert!(residency.release.is_none(), "request already tracked");
        residency.release = Some(release);
    }
    /// Failed admission has already discarded every temporary Kira alias. The
    /// caller discards its provisional evidence; returned PCM remains unaccepted.
    pub(crate) fn untrack_retirement(&mut self) {
        let residency =
            Arc::get_mut(&mut self.budget).expect("rejected request uniquely owns its residency");
        drop(residency.release.take());
    }
}
struct BudgetedData {
    data: StaticSoundData,
    budget: Arc<Residency>,
    signal: Arc<RetirementSignal>,
}
struct BudgetedSound {
    // Rust drops fields in declaration order: native Sound/PCM FIRST, ticket
    // SECOND. The private frame Arc has no externally held Weak references.
    sound: Box<dyn Sound>,
    _budget: Arc<Residency>,
    signal: Arc<RetirementSignal>,
}
impl Sound for BudgetedSound {
    fn on_start_processing(&mut self) {
        self.sound.on_start_processing();
    }
    fn process(&mut self, out: &mut [Frame], dt: f64, info: &Info) {
        self.sound.process(out, dt, info);
    }
    fn finished(&self) -> bool {
        let finished = self.sound.finished();
        if finished {
            self.signal.observed.store(true, Ordering::Release);
        }
        finished
    }
}
impl SoundData for BudgetedData {
    type Error = ();
    type Handle = StaticSoundHandle;
    fn into_sound(self) -> Result<(Box<dyn Sound>, Self::Handle), ()> {
        let (sound, handle) = self.data.into_sound()?;
        Ok((
            Box::new(BudgetedSound {
                sound,
                _budget: self.budget,
                signal: self.signal,
            }),
            handle,
        ))
    }
}

/// Kira native handles stay private to this adapter. Capacity and all control
/// allocations are established before real-time consumption starts.
pub struct KiraMixer {
    manager: DeferredManager,
    signal: Arc<RetirementSignal>,
}
/// A count of pure ownership transfers. Destruction/budget return is asynchronous.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReclaimProgress {
    pub transferred_sounds: usize,
    /// A reached quota requests another maintenance turn even if the queue
    /// happens to be empty. False only observed no currently published garbage;
    /// active sounds or a retirement in flight still own their PCM/tickets.
    pub needs_maintenance: bool,
}
impl KiraMixer {
    /// Host-owned service outlives mixers. Generation and two teardown credits
    /// are reserved before Kira construction; old resident generations cause Busy.
    pub fn new(
        format: &AudioFormat,
        limits: MixerLimits,
        service: &AudioReclaimService,
    ) -> Result<Self, AudioError> {
        format.validate()?;
        if limits.max_sounds == 0
            || limits.internal_buffer_frames == 0
            || std::alloc::Layout::array::<Frame>(limits.internal_buffer_frames).is_err()
        {
            return Err(AudioError::new(
                AudioErrorKind::InvalidRequest,
                "invalid Kira resource/buffer limits",
            ));
        }
        let (signal, renderer_credit, manager_credit) = service.reserve_generation()?;
        let manager = AudioManager::<Bridge>::new(AudioManagerSettings {
            backend_settings: (format.sample_rate_hz, signal.clone(), renderer_credit),
            capacities: reel_audio::Capacities {
                sub_track_capacity: 0,
                send_track_capacity: 0,
                clock_capacity: 0,
                modulator_capacity: 0,
                listener_capacity: 0,
            },
            main_track_builder: MainTrackBuilder::new().sound_capacity(limits.max_sounds),
            internal_buffer_size: limits.internal_buffer_frames,
        })?;
        let manager = DeferredManager::new(manager, manager_credit);
        #[cfg(test)]
        if service
            .shared
            .abort_construction
            .swap(false, Ordering::AcqRel)
        {
            return Err(AudioError::new(
                AudioErrorKind::DeviceFailure,
                "injected construction abort",
            ));
        }
        Ok(Self { manager, signal })
    }
    /// Linear transfer, at most once. Keep this manager alive while the output owns fill.
    pub fn take_fill(&mut self) -> Option<PcmFill> {
        self.manager
            .manager
            .as_mut()
            .expect("owned manager")
            .backend_mut()
            .fill
            .take()
    }
    /// Transfers at most min(limit, max_sounds) retired Sounds. Destination
    /// credit is reserved before Kira pop; full/contended queue retains ownership.
    /// Quota/full returns needs_maintenance. No physical destructor runs here.
    pub fn reclaim_unused(&mut self, limit: usize) -> ReclaimProgress {
        let signal = self.signal.clone();
        let track = self
            .manager
            .manager
            .as_mut()
            .expect("owned manager")
            .main_track();
        let limit = limit.min(track.sound_capacity());
        let mut transferred_sounds = 0;
        let mut needs_maintenance = true;
        if signal.shared.accepting()
            && let Some(mut producer) = try_producer(&signal.shared)
        {
            while transferred_sounds < limit {
                let Ok(chunk) = producer.write_chunk_uninit(1) else {
                    break;
                };
                let Some(sound) = track.try_take_unused_sound() else {
                    needs_maintenance = false;
                    break;
                };
                chunk.fill_from_iter(std::iter::once(Envelope::normal(
                    Job::Sound(sound),
                    &signal,
                )));
                transferred_sounds += 1;
            }
        }
        if needs_maintenance || transferred_sounds != 0 {
            signal.shared.notify();
        }
        ReclaimProgress {
            transferred_sounds,
            needs_maintenance,
        }
    }
    /// Attempt transfer BEFORE reserving construction memory. Never waits for
    /// budget: Busy is retried after completion wake, including a transfer with
    /// no physical release yet. Allocate/decode only after the ticket is Ok.
    pub fn reserve_decode_budget(
        &mut self,
        pool: &BudgetPool,
        bytes: u64,
        reclaim_limit: usize,
    ) -> (ReclaimProgress, Result<BudgetTicket, RejectReason>) {
        let progress = self.reclaim_unused(reclaim_limit);
        (progress, pool.reserve(bytes))
    }
    /// Rejection returns the unchanged unaccepted request. Success transfers
    /// EVERY internal PCM reference, including the original data, to the worker.
    pub fn play(&mut self, sound: DecodedSound) -> Result<Playback, Rejected<DecodedSound>> {
        let signal = self.signal.clone();
        let reject = |request| Rejected {
            request,
            reason: RejectReason::new(
                RejectCode::Busy,
                "Kira or background ownership capacity exhausted",
            ),
        };
        if !signal.shared.accepting() {
            return Err(reject(sound));
        }
        let Some(mut producer) = try_producer(&signal.shared) else {
            return Err(reject(sound));
        };
        // Actual destination credit BEFORE acceptance. No fallible send follows.
        let Ok(chunk) = producer.write_chunk_uninit(1) else {
            return Err(reject(sound));
        };
        let data = BudgetedData {
            data: sound.data.clone(),
            budget: sound.budget.clone(),
            signal: signal.clone(),
        };
        match self
            .manager
            .manager
            .as_mut()
            .expect("owned manager")
            .play(data)
        {
            Ok(handle) => {
                #[cfg(test)]
                {
                    let hook = signal.shared.after_accept.lock().unwrap().clone();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                chunk.fill_from_iter(std::iter::once(Envelope::normal(
                    Job::AcceptedData(sound),
                    &signal,
                )));
                signal.shared.notify();
                Ok(Playback { handle })
            }
            Err(_) => Err(reject(sound)),
        }
    }
}
/// Control wrapper; state/position describe content consumption, not device presentation.
pub struct Playback {
    handle: StaticSoundHandle,
}
impl Playback {
    pub fn position_seconds(&self) -> f64 {
        self.handle.position()
    }
    pub fn is_stopped(&self) -> bool {
        self.handle.state() == PlaybackState::Stopped
    }
    /// Coalesced Kira command slots; never competes for the bounded play queue.
    pub fn set_volume_db(&mut self, db: f32) -> Result<(), AudioError> {
        if !db.is_finite() {
            return Err(AudioError::new(
                AudioErrorKind::InvalidRequest,
                "volume must be finite",
            ));
        }
        self.handle.set_volume(db, immediate());
        Ok(())
    }
    pub fn stop(&mut self) {
        self.handle.stop(immediate());
    }
}
fn immediate() -> Tween {
    Tween {
        duration: Duration::ZERO,
        ..Tween::default()
    }
}

#[cfg(test)]
mod retirement_tests;
