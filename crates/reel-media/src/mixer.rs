use crate::maintenance::{ReclaimNotifier, RetirementSignal};
use crate::{AudioError, AudioErrorKind, AudioFormat, PcmFill};
use kira::{
    AudioManager, AudioManagerSettings, Frame, Tween,
    backend::{Backend, Renderer},
    info::Info,
    sound::{
        PlaybackState, Sound, SoundData,
        static_sound::{StaticSoundData, StaticSoundHandle, StaticSoundSettings},
    },
    track::MainTrackBuilder,
};
use reel_core::{
    budget::{BudgetPool, BudgetTicket},
    result::{RejectCode, RejectReason, Rejected},
    wake::WakeSignal,
};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

struct Bridge {
    sample_rate: u32,
    signal: Arc<RetirementSignal>,
    fill: Option<PcmFill>,
}
impl Backend for Bridge {
    type Settings = (u32, Arc<RetirementSignal>);
    type Error = AudioError;
    fn setup((sample_rate, signal): Self::Settings, _: usize) -> Result<(Self, u32), AudioError> {
        Ok((
            Self {
                sample_rate,
                signal,
                fill: None,
            },
            sample_rate,
        ))
    }
    fn start(&mut self, mut renderer: Renderer) -> Result<(), AudioError> {
        debug_assert_ne!(self.sample_rate, 0);
        let signal = self.signal.clone();
        self.fill = Some(Box::new(move |out| {
            renderer.on_start_processing();
            // finished() marks observation before removal; wake only AFTER the
            // RT publication phase, so an early empty control drain cannot lose it.
            if signal.observed.swap(false, Ordering::AcqRel) {
                signal.pending.store(true, Ordering::Release);
            }
            renderer.process(out, 2);
        }));
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
    budget: Arc<BudgetTicket>,
}
pub type DecodedSoundRequest = (u32, Vec<[f32; 2]>, BudgetTicket);
pub const MAX_DECODED_FRAMES: usize = 8_388_608;
impl DecodedSound {
    pub fn new(
        sample_rate_hz: u32,
        frames: Vec<[f32; 2]>,
        ticket: BudgetTicket,
    ) -> Result<Self, Rejected<DecodedSoundRequest>> {
        let required = (frames.capacity() as u64)
            .checked_add(frames.len() as u64)
            .and_then(|n| n.checked_mul(8));
        if sample_rate_hz == 0
            || frames.is_empty()
            || frames.len() > MAX_DECODED_FRAMES
            || required.is_none_or(|n| n > ticket.bytes())
        {
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
            budget: Arc::new(ticket),
        })
    }
    /// Loop the decoded region using Kira's transport, without duplicating PCM.
    pub fn looping(mut self) -> Self {
        self.data = self.data.loop_region(..);
        self
    }
    pub fn resident_bytes(&self) -> u64 {
        self.budget.bytes()
    }
}
struct BudgetedData {
    data: StaticSoundData,
    budget: Arc<BudgetTicket>,
    signal: Arc<RetirementSignal>,
}
struct BudgetedSound {
    // Rust drops fields in declaration order: native Sound/PCM FIRST, ticket
    // SECOND. The private frame Arc has no externally held Weak references.
    sound: Box<dyn Sound>,
    _budget: Arc<BudgetTicket>,
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
    manager: AudioManager<Bridge>,
    signal: Arc<RetirementSignal>,
    _notifier: ReclaimNotifier,
}
/// A count of completed control-side destructors, not a time-slice guarantee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReclaimProgress {
    pub reclaimed_sounds: usize,
    /// A reached quota requests another maintenance turn even if the queue
    /// happens to be empty. False only observed no currently published garbage;
    /// active sounds or a retirement in flight still own their PCM/tickets.
    pub needs_maintenance: bool,
}
impl KiraMixer {
    /// maintenance_wake is the host's existing control scheduler wake. A host
    /// acknowledges it BEFORE a maintenance tick; it must schedule another tick
    /// when needs_maintenance is true, including while the UI is sleeping.
    pub fn new(
        format: &AudioFormat,
        limits: MixerLimits,
        maintenance_wake: WakeSignal,
    ) -> Result<Self, AudioError> {
        format.validate()?;
        if !(1..=256).contains(&limits.max_sounds)
            || !(1..=4096).contains(&limits.internal_buffer_frames)
        {
            return Err(AudioError::new(
                AudioErrorKind::InvalidRequest,
                "invalid Kira resource/buffer limits",
            ));
        }
        let signal = Arc::new(RetirementSignal::default());
        let notifier = ReclaimNotifier::new(signal.clone(), maintenance_wake)?;
        let manager = AudioManager::<Bridge>::new(AudioManagerSettings {
            backend_settings: (format.sample_rate_hz, signal.clone()),
            capacities: kira::Capacities {
                sub_track_capacity: 0,
                send_track_capacity: 0,
                clock_capacity: 0,
                modulator_capacity: 0,
                listener_capacity: 0,
            },
            main_track_builder: MainTrackBuilder::new().sound_capacity(limits.max_sounds),
            internal_buffer_size: limits.internal_buffer_frames,
        })?;
        Ok(Self {
            manager,
            signal,
            _notifier: notifier,
        })
    }
    /// Linear transfer, at most once. Keep this manager alive while the output owns fill.
    pub fn take_fill(&mut self) -> Option<PcmFill> {
        self.manager.backend_mut().fill.take()
    }
    /// Normal control-tick maintenance. At most min(limit, max_sounds) resource
    /// destructors run, including limit=0 (no progress). A single PCM destructor
    /// is indivisible and its measured worst cost may exceed a time quota.
    /// No new play or ticket is needed; main-track play has no hidden cleanup.
    pub fn reclaim_unused(&mut self, limit: usize) -> ReclaimProgress {
        let (reclaimed_sounds, needs_maintenance) = self.manager.main_track().reclaim_unused(limit);
        if needs_maintenance {
            self.signal.pending.store(true, Ordering::Release);
        }
        ReclaimProgress {
            reclaimed_sounds,
            needs_maintenance,
        }
    }
    /// Reclaim BEFORE requesting a new PCM construction ticket. Inspect progress
    /// even on Busy. Allocate/decode input only after the returned ticket is Ok;
    /// never return tickets merely because playback is stopped.
    pub fn reserve_decode_budget(
        &mut self,
        pool: &BudgetPool,
        bytes: u64,
        reclaim_limit: usize,
    ) -> (ReclaimProgress, Result<BudgetTicket, RejectReason>) {
        let progress = self.reclaim_unused(reclaim_limit);
        (progress, pool.reserve(bytes))
    }
    pub fn play(&mut self, sound: DecodedSound) -> Result<Playback, Rejected<DecodedSound>> {
        // Retain the exact request through Kira's fallible play; clone only immutable refs.
        let data = BudgetedData {
            data: sound.data.clone(),
            budget: sound.budget.clone(),
            signal: self.signal.clone(),
        };
        match self.manager.play(data) {
            Ok(handle) => Ok(Playback { handle }),
            Err(_) => Err(Rejected {
                request: sound,
                reason: RejectReason::new(RejectCode::Busy, "Kira sound capacity exhausted"),
            }),
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
mod retirement_tests {
    use super::*;
    use crate::{
        AudioDeviceConfig, AudioOutput, AudioRequest, ChannelLayout, OfflineOutput, SampleFormat,
    };
    use reel_core::budget::{BudgetCategory, QueueLimits};
    use std::{sync::mpsc, thread, time::Duration};
    fn format() -> AudioFormat {
        let mut output = OfflineOutput::new(
            AudioDeviceConfig {
                sample_rate_hz: 48_000,
                sample_format: SampleFormat::F32,
                layout: ChannelLayout::Stereo,
            },
            WakeSignal::new(|| {}),
        )
        .unwrap();
        output.configure(AudioRequest::default()).unwrap()
    }
    #[test]
    fn single_budget_a_to_b_reclaims_actual_pcm_and_wakes_sleeping_control() {
        for (label, limits) in [
            (
                "one-ticket",
                QueueLimits {
                    max_requests: 1,
                    max_bytes: 16_384,
                },
            ),
            (
                "one-sound-bytes",
                QueueLimits {
                    max_requests: 8,
                    max_bytes: 1_024,
                },
            ),
        ] {
            let pool = BudgetPool::new(BudgetCategory::CpuOutput, limits);
            let (notifications, wakeups) = mpsc::channel();
            let wake = WakeSignal::new(move || {
                notifications.send(thread::current().id()).unwrap();
            });
            let mut mixer = KiraMixer::new(
                &format(),
                MixerLimits {
                    max_sounds: 1,
                    internal_buffer_frames: 64,
                },
                wake.clone(),
            )
            .unwrap();
            let mut fill = mixer.take_fill().unwrap();
            let (progress, ticket) = mixer.reserve_decode_budget(&pool, 1_024, 1);
            assert_eq!(progress.reclaimed_sounds, 0);
            let a = DecodedSound::new(48_000, vec![[0.1, -0.2]; 64], ticket.unwrap()).unwrap();
            let actual_pcm = Arc::downgrade(&a.data.frames);
            let a_handle = mixer.play(a).unwrap();
            let mut pcm = [0.0; 512 * 2];
            fill(&mut pcm);
            assert!(a_handle.is_stopped());
            assert!(
                pcm.chunks_exact(2)
                    .any(|f| (f[0] - 0.1).abs() < 1e-6 && (f[1] + 0.2).abs() < 1e-6)
            );
            // A is Stopped but still in arena; no early budget return.
            assert_eq!(mixer.reclaim_unused(1).reclaimed_sounds, 0);
            assert!(actual_pcm.upgrade().is_some());
            assert_eq!(pool.usage().tickets, 1);
            assert!(pool.reserve(1_024).is_err());
            fill(&mut pcm); // publish retired A, then set notification flag.
            let notifier_thread = wakeups.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_ne!(notifier_thread, thread::current().id());
            assert!(wake.acknowledge()); // Existing host scheduling contract: before drain.
            assert_eq!(pool.usage().tickets, 1);
            // Weak is a liveness observer, not proof of physical deallocation:
            // drop it first so it cannot retain Arc<[Frame]>'s allocation. The
            // standalone allocator probe separately verifies the actual free.
            drop(actual_pcm);
            // This entry performs the actual reclaim BEFORE reserving B's ticket.
            let (progress, b_ticket) = mixer.reserve_decode_budget(&pool, 1_024, 1);
            assert_eq!(progress.reclaimed_sounds, 1);
            assert!(progress.needs_maintenance); // Quota hit does NOT claim empty.
            let b_ticket = b_ticket.unwrap(); // Both single-amount configurations can progress.
            // Even with no subsequent PCM callback, hitting the quota wakes another
            // maintenance turn; the host can sleep on its existing wake source.
            wakeups.recv_timeout(Duration::from_secs(1)).unwrap();
            assert!(wake.acknowledge());
            assert_eq!(
                mixer.reclaim_unused(1),
                ReclaimProgress {
                    reclaimed_sounds: 0,
                    needs_maintenance: false
                }
            );
            let b = DecodedSound::new(48_000, vec![[0.3, -0.4]; 64], b_ticket).unwrap();
            let b_handle = mixer.play(b).unwrap();
            fill(&mut pcm);
            assert!(b_handle.is_stopped());
            assert!(
                pcm.chunks_exact(2)
                    .any(|f| (f[0] - 0.3).abs() < 1e-6 && (f[1] + 0.4).abs() < 1e-6)
            );
            fill(&mut pcm);
            assert_eq!(mixer.reclaim_unused(1).reclaimed_sounds, 1);
            assert_eq!(pool.usage().tickets, 0);
            println!(
                "{label}: A stopped still charged; publish wakes control; reserve_decode_budget reclaimed A's PCM before B reserve; quota wakes next tick; same mixer/fill B=[0.3,-0.4]"
            );
        }
    }
    #[test]
    fn bounded_tick_and_successful_play_do_not_hide_bulk_pcm_destruction() {
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 2,
                max_bytes: 4_096,
            },
        );
        let mut mixer = KiraMixer::new(
            &format(),
            MixerLimits {
                max_sounds: 2,
                internal_buffer_frames: 64,
            },
            WakeSignal::new(|| {}),
        )
        .unwrap();
        let mut fill = mixer.take_fill().unwrap();
        for value in [0.1, 0.2] {
            let (_, ticket) = mixer.reserve_decode_budget(&pool, 1_024, 1);
            mixer
                .play(
                    DecodedSound::new(48_000, vec![[value, -value]; 64], ticket.unwrap()).unwrap(),
                )
                .unwrap();
        }
        let mut pcm = [0.0; 512 * 2];
        fill(&mut pcm);
        fill(&mut pcm); // both retired; no control play needed.
        assert_eq!(pool.usage().tickets, 2);
        assert_eq!(mixer.reclaim_unused(0).reclaimed_sounds, 0);
        assert_eq!(pool.usage().tickets, 2);
        let progress = mixer.reclaim_unused(1);
        assert_eq!(progress.reclaimed_sounds, 1);
        assert!(progress.needs_maintenance);
        assert_eq!(pool.usage().tickets, 1);
        let c =
            DecodedSound::new(48_000, vec![[0.3, -0.4]; 64], pool.reserve(1_024).unwrap()).unwrap();
        mixer.play(c).unwrap();
        assert_eq!(pool.usage().tickets, 2); // remaining retired sound was NOT drained by play.
        assert_eq!(mixer.reclaim_unused(1).reclaimed_sounds, 1);
        assert_eq!(pool.usage().tickets, 1); // only new C remains resident.
        println!(
            "two retired sounds: tick limit 0 drops none; limit 1 drops exactly one; successful play(C) does not reclaim the other"
        );
    }
}
