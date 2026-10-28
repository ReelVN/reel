use crate::maintenance::{
    AudioReclaimService, DeferredManager, DeferredRenderer, Envelope, Job, RetirementSignal,
    TeardownCredit, try_producer,
};
use crate::retirement::PhysicalRelease;
use crate::{AudioError, AudioErrorKind, AudioFormat, AudioGroup, PcmFill};
use reel_audio::{
    AudioManager, AudioManagerSettings, Decibels, Frame, Tween,
    backend::{Backend, Renderer},
    info::Info,
    sound::{
        EndPosition, PlaybackPosition, PlaybackState, Region, Sound, SoundData,
        static_sound::{
            QueuedStaticSound, StaticSoundData, StaticSoundHandle, StaticSoundQueue,
            StaticSoundSettings,
        },
    },
    track::{MainTrackBuilder, TrackBuilder, TrackHandle},
};
#[cfg(test)]
use reel_core::wake::WakeSignal;
use reel_core::{
    audio::{AudioLoopRegion, AudioRate},
    budget::{BudgetPool, BudgetTicket},
    result::{RejectCode, RejectReason, Rejected},
};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
mod stream;
pub use stream::DecodeLimits;

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
    release: std::sync::Mutex<Option<PhysicalRelease>>,
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
                release: std::sync::Mutex::new(None),
            }),
        })
    }
    /// Decode off the realtime thread, reserving PCM growth and checking cancellation.
    #[cfg(feature = "decode")]
    pub fn decode(
        bytes: impl AsRef<[u8]> + Send + Sync + 'static,
        mut ticket: BudgetTicket,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Self, String> {
        let data = StaticSoundData::decode_with(Box::new(std::io::Cursor::new(bytes)), |bytes| {
            if cancelled.load(Ordering::Acquire) {
                return Err(std::io::Error::other("audio decode cancelled"));
            }
            ticket.resize(bytes as u64).map_err(|e| {
                std::io::Error::other(format!(
                    "audio PCM decode needs {bytes} bytes: {}",
                    e.message
                ))
            })
        })
        .map_err(|e| e.to_string())?;
        if data.frames.is_empty() {
            return Err("audio contains no PCM frames".into());
        }
        ticket
            .resize((data.frames.len() * std::mem::size_of::<Frame>()) as u64)
            .map_err(|e| e.message)?;
        Ok(Self {
            data,
            budget: Arc::new(Residency {
                ticket,
                release: std::sync::Mutex::new(None),
            }),
        })
    }
    /// Set initial gain before admission; no first-buffer volume race.
    pub fn with_volume_db(mut self, db: f32) -> Self {
        self.data.settings.volume = Decibels(db.max(Decibels::SILENCE.0)).into();
        self
    }
    pub fn with_pan(mut self, pan: f32) -> Self {
        self.data.settings.panning = reel_audio::Panning(pan).into();
        self
    }
    /// Set bounded content speed before admission without changing PCM ownership.
    pub fn with_playback_rate(mut self, rate: f64) -> Result<Self, AudioError> {
        validate_rate(rate)?;
        self.data.settings.playback_rate = rate.into();
        Ok(self)
    }
    pub fn with_fade_in(mut self, duration: Duration) -> Self {
        self.data.settings.fade_in_tween = (!duration.is_zero()).then(|| tween(duration));
        self
    }
    /// Resolve the initial position, exclusive endpoint and loop after decoding.
    /// The transport uses frame indices and retains the same PCM allocation.
    pub fn with_playback(
        mut self,
        start: Duration,
        end: Option<Duration>,
        loop_region: Option<AudioLoopRegion>,
    ) -> Result<Self, AudioError> {
        let frames = self.data.num_frames();
        let sample_rate = f64::from(self.data.sample_rate);
        let frame = |time: Duration| (time.as_secs_f64() * sample_rate).round() as usize;
        let frames = if let Some(end) = end {
            let end = frame(end);
            if end == 0 || end > frames {
                return Err(AudioError::new(
                    AudioErrorKind::InvalidRequest,
                    "audio end must fit within the source and include at least one frame",
                ));
            }
            // Keep absolute source positions and share the original PCM backing.
            self.data.slice = Some((0, end));
            end
        } else {
            frames
        };
        self.data.settings.loop_region = if let Some(region) = loop_region {
            let from = frame(region.start);
            let to = region.end.map_or(frames, frame);
            if from >= to || to > frames {
                return Err(AudioError::new(
                    AudioErrorKind::InvalidRequest,
                    "audio loop must fit within the playback end and contain at least one frame",
                ));
            }
            Some(Region {
                start: PlaybackPosition::Samples(from),
                end: if region.end.is_none() && end.is_none() {
                    EndPosition::EndOfAudio
                } else {
                    EndPosition::Custom(PlaybackPosition::Samples(to))
                },
            })
        } else {
            None
        };
        self.seek(start);
        Ok(self)
    }
    /// Update a decoded sound before admission without copying or decoding PCM.
    pub fn seek(&mut self, position: Duration) -> Duration {
        let bounds = PlaybackBounds::new(&self.data);
        let frame = bounds.frame(position);
        self.data.settings.start_position = PlaybackPosition::Samples(frame);
        bounds.time(frame)
    }
    /// The resolved content start, including frame quantization and loop wrapping.
    pub fn start_position(&self) -> Duration {
        let seconds = match self.data.settings.start_position {
            PlaybackPosition::Seconds(seconds) => seconds,
            PlaybackPosition::Samples(frame) => frame as f64 / f64::from(self.data.sample_rate),
        };
        Duration::from_secs_f64(seconds)
    }
    pub(crate) fn volume_db(&self) -> f32 {
        match self.data.settings.volume {
            reel_audio::Value::Fixed(value) => value.0,
            _ => unreachable!("decoded sounds use fixed initial volume"),
        }
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
        let mut slot = self.budget.release.lock().unwrap();
        assert!(slot.is_none(), "request already tracked");
        *slot = Some(release);
    }
    /// Failed admission has already discarded every temporary Kira alias. The
    /// caller discards its provisional evidence; returned PCM remains unaccepted.
    pub(crate) fn untrack_retirement(&mut self) {
        drop(self.budget.release.lock().unwrap().take());
    }
}
struct BudgetedData {
    data: StaticSoundData,
    budget: Arc<Residency>,
    signal: Arc<RetirementSignal>,
    capacity: usize,
}
#[cfg(feature = "video")]
struct ObservedData<D>(D, Arc<RetirementSignal>);
#[cfg(feature = "video")]
struct ObservedSound {
    sound: Box<dyn Sound>,
    signal: Arc<RetirementSignal>,
}
#[cfg(feature = "video")]
impl<D: SoundData> SoundData for ObservedData<D> {
    type Error = D::Error;
    type Handle = D::Handle;
    fn into_sound(self) -> Result<(Box<dyn Sound>, D::Handle), D::Error> {
        let (sound, handle) = self.0.into_sound()?;
        Ok((
            Box::new(ObservedSound {
                sound,
                signal: self.1,
            }),
            handle,
        ))
    }
}
#[cfg(feature = "video")]
impl Sound for ObservedSound {
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
struct BudgetedSound {
    // Rust drops fields in declaration order: native Sound/PCM FIRST, ticket
    // SECOND. The private frame Arc has no externally held Weak references.
    sound: Box<dyn Sound>,
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
    type Handle = (StaticSoundHandle, StaticSoundQueue);
    fn into_sound(self) -> Result<(Box<dyn Sound>, Self::Handle), ()> {
        let (sound, handle, queue) = self.data.into_sound_with_queue(self.capacity, self.budget);
        Ok((
            Box::new(BudgetedSound {
                sound,
                signal: self.signal,
            }),
            (handle, queue),
        ))
    }
}

/// Kira native handles stay private to this adapter. Capacity and all control
/// allocations are established before real-time consumption starts.
pub(crate) struct MixerControl {
    pub groups: Vec<TrackHandle>,
    queues: Vec<Option<StaticSoundQueue>>,
    pub manager: AudioManager<Bridge>,
}
pub struct KiraMixer {
    manager: DeferredManager,
    signal: Arc<RetirementSignal>,
    max_sounds: usize,
    stream_cache_bytes: u64,
    reclaim_cursor: usize,
}
/// A count of pure ownership transfers. Destruction/budget return is asynchronous.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReclaimProgress {
    /// Ownership objects: native sounds, consumed queue segments, or endpoints.
    pub transferred_sounds: usize,
    /// A reached quota requests another maintenance turn even if the queue
    /// happens to be empty. False only observed no currently published garbage;
    /// active sounds or a retirement in flight still own their PCM/tickets.
    pub needs_maintenance: bool,
}
impl KiraMixer {
    #[cfg(feature = "video")]
    pub fn can_play_video_audio(&self) -> bool {
        self.signal.shared.accepting()
            && self.manager.manager.as_ref().is_some_and(|control| {
                control
                    .groups
                    .iter()
                    .map(TrackHandle::num_sounds)
                    .sum::<usize>()
                    < self.max_sounds
            })
    }
    /// Stream PCM shares the same output, buses and off-callback reclamation as
    /// static sounds. The stream owner handles transport and terminal delivery.
    #[cfg(feature = "video")]
    pub fn play_video_audio(
        &mut self,
        group: AudioGroup,
        sound: crate::video::StreamSoundData,
    ) -> Result<(), AudioError> {
        let control = self.manager.manager.as_mut().expect("owned manager");
        if !self.signal.shared.accepting()
            || control
                .groups
                .iter()
                .map(TrackHandle::num_sounds)
                .sum::<usize>()
                >= self.max_sounds
        {
            return Err(AudioError::new(
                AudioErrorKind::InvalidRequest,
                "audio mixer capacity exhausted",
            ));
        }
        control.groups[group as usize]
            .play(ObservedData(sound, self.signal.clone()))
            .map_err(|_| {
                AudioError::new(
                    AudioErrorKind::InvalidRequest,
                    "audio mixer capacity exhausted",
                )
            })
    }
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
        // Cache storage survives while a Sound is active, on any group's unused
        // queue, or in the shared normal queue/worker. Queued segments have no
        // voice cache until they replace the current source of that same Sound.
        let stream_cache_bytes = (limits.max_sounds as u64)
            .checked_mul((AudioGroup::ALL.len() + 1) as u64)
            .and_then(|voices| voices.checked_add(service.limits().queued_work as u64))
            .and_then(|voices| voices.checked_add(1))
            .and_then(|voices| {
                voices.checked_mul(reel_audio::sound::static_sound::STREAM_CACHE_BYTES as u64)
            })
            .ok_or_else(|| {
                AudioError::new(
                    AudioErrorKind::InvalidRequest,
                    "voice cache accounting overflow",
                )
            })?;
        let (signal, renderer_credit, manager_credit) = service.reserve_generation()?;
        let manager = AudioManager::<Bridge>::new(AudioManagerSettings {
            backend_settings: (format.sample_rate_hz, signal.clone(), renderer_credit),
            capacities: reel_audio::Capacities {
                sub_track_capacity: AudioGroup::ALL.len(),
                send_track_capacity: 0,
                clock_capacity: 0,
                modulator_capacity: 0,
                listener_capacity: 0,
            },
            main_track_builder: MainTrackBuilder::new().sound_capacity(0),
            internal_buffer_size: limits.internal_buffer_frames,
        })?;
        let mut manager = DeferredManager::new(
            MixerControl {
                groups: Vec::with_capacity(AudioGroup::ALL.len()),
                queues: (0..limits.max_sounds).map(|_| None).collect(),
                manager,
            },
            manager_credit,
        );
        let control = manager.manager.as_mut().unwrap();
        for _ in AudioGroup::ALL {
            let track = control
                .manager
                .add_sub_track(
                    TrackBuilder::new()
                        .sound_capacity(limits.max_sounds)
                        .sub_track_capacity(0),
                )
                .map_err(|_| {
                    AudioError::new(AudioErrorKind::InvalidRequest, "group track capacity")
                })?;
            control.groups.push(track);
        }
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
        Ok(Self {
            manager,
            signal,
            max_sounds: limits.max_sounds,
            stream_cache_bytes,
            reclaim_cursor: 0,
        })
    }
    /// Linear transfer, at most once. Keep this manager alive while the output owns fill.
    pub fn take_fill(&mut self) -> Option<PcmFill> {
        self.manager
            .manager
            .as_mut()
            .expect("owned manager")
            .manager
            .backend_mut()
            .fill
            .take()
    }
    /// Transfers at most min(limit, max_sounds) retired Sounds. Destination
    /// credit is reserved before Kira pop; full/contended queue retains ownership.
    /// Quota/full returns needs_maintenance. No physical destructor runs here.
    pub fn reclaim_unused(&mut self, limit: usize) -> ReclaimProgress {
        let signal = self.signal.clone();
        let control = self.manager.manager.as_mut().expect("owned manager");
        let count = AudioGroup::ALL.len() + control.queues.len();
        let limit = limit.min(self.max_sounds);
        let mut transferred_sounds = 0;
        let mut needs_maintenance = true;
        let mut empty = 0;
        if signal.shared.accepting()
            && let Some(mut producer) = try_producer(&signal.shared)
        {
            while transferred_sounds < limit {
                let Ok(chunk) = producer.write_chunk_uninit(1) else {
                    break;
                };
                let slot = self.reclaim_cursor;
                self.reclaim_cursor = (slot + 1) % count;
                let job = if slot < AudioGroup::ALL.len() {
                    control.groups[slot].try_take_unused_sound().map(Job::Sound)
                } else {
                    let entry = &mut control.queues[slot - AudioGroup::ALL.len()];
                    if entry.as_ref().is_some_and(StaticSoundQueue::is_finished) {
                        entry.take().map(Job::SoundQueue)
                    } else {
                        entry
                            .as_mut()
                            .and_then(StaticSoundQueue::take_retired)
                            .map(Job::QueuedSound)
                    }
                };
                if let Some(job) = job {
                    chunk.fill_from_iter(std::iter::once(Envelope::normal(job, &signal)));
                    transferred_sounds += 1;
                    empty = 0;
                } else {
                    empty += 1;
                    if empty == count {
                        needs_maintenance = false;
                        break;
                    }
                }
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
        self.play_in(AudioGroup::Effects, sound)
    }
    pub fn set_group_volume_db(
        &mut self,
        group: Option<AudioGroup>,
        db: f32,
        duration: Duration,
    ) -> Result<(), AudioError> {
        let db = validate_db(db)?;
        let control = self.manager.manager.as_mut().expect("owned manager");
        match group {
            Some(group) => control.groups[group as usize].set_volume(db, tween(duration)),
            None => control.manager.main_track().set_volume(db, tween(duration)),
        }
        Ok(())
    }
    pub fn play_in(
        &mut self,
        group: AudioGroup,
        sound: DecodedSound,
    ) -> Result<Playback, Rejected<DecodedSound>> {
        self.play_in_mode(group, sound, false)
    }
    pub(crate) fn play_in_mode(
        &mut self,
        group: AudioGroup,
        sound: DecodedSound,
        held: bool,
    ) -> Result<Playback, Rejected<DecodedSound>> {
        let signal = self.signal.clone();
        let reject = |request| Rejected {
            request,
            reason: RejectReason::new(
                RejectCode::Busy,
                "Kira or background ownership capacity exhausted",
            ),
        };
        if !signal.shared.accepting()
            || self
                .manager
                .manager
                .as_ref()
                .unwrap()
                .groups
                .iter()
                .map(TrackHandle::num_sounds)
                .sum::<usize>()
                >= self.max_sounds
        {
            return Err(reject(sound));
        }
        let Some(mut producer) = try_producer(&signal.shared) else {
            return Err(reject(sound));
        };
        // Actual destination credit BEFORE acceptance. No fallible send follows.
        let Ok(chunk) = producer.write_chunk_uninit(1) else {
            return Err(reject(sound));
        };
        let bounds = PlaybackBounds::new(&sound.data);
        let control = self.manager.manager.as_mut().expect("owned manager");
        let Some(queue_slot) = control.queues.iter().position(Option::is_none).or_else(|| {
            control
                .queues
                .iter()
                .position(|queue| queue.as_ref().is_some_and(StaticSoundQueue::is_finished))
        }) else {
            return Err(reject(sound));
        };
        let data = BudgetedData {
            data: sound.data.clone(),
            budget: sound.budget.clone(),
            signal: signal.clone(),
            capacity: self.max_sounds,
        };
        match control.groups[group as usize].play(data) {
            Ok((handle, queue)) => {
                if !held {
                    queue.start();
                }
                // Reuse a finished endpoint by transferring it with the already
                // reserved acceptance job. No PCM destruction occurs on control.
                let retired_queue = control.queues[queue_slot].replace(queue);
                #[cfg(test)]
                {
                    let hook = signal.shared.after_accept.lock().unwrap().clone();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                chunk.fill_from_iter(std::iter::once(Envelope::normal(
                    Job::AcceptedData(sound, retired_queue),
                    &signal,
                )));
                signal.shared.notify();
                let mut history = std::collections::VecDeque::with_capacity(self.max_sounds + 1);
                history.push_back((0, bounds));
                Ok(Playback {
                    handle,
                    bounds: history,
                    queue_slot,
                })
            }
            Err(_) => Err(reject(sound)),
        }
    }
    /// Announce before decoding so an exhausted segment holds its content clock.
    pub fn start(&mut self, playback: &Playback) {
        self.manager.manager.as_ref().unwrap().queues[playback.queue_slot]
            .as_ref()
            .expect("live playback queue")
            .start();
    }
    pub fn announce(&mut self, playback: &Playback, index: u64) -> Result<(), AudioError> {
        if playback.is_stopped() {
            return Err(AudioError::new(
                AudioErrorKind::InvalidRequest,
                "audio has ended",
            ));
        }
        let queue = self.manager.manager.as_mut().unwrap().queues[playback.queue_slot]
            .as_ref()
            .expect("live playback queue");
        queue
            .announce(index)
            .map_err(|_| AudioError::new(AudioErrorKind::InvalidRequest, "audio has ended"))
    }
    pub fn queue(
        &mut self,
        playback: &mut Playback,
        index: u64,
        sound: DecodedSound,
    ) -> Result<(), Rejected<DecodedSound>> {
        if playback.is_stopped() {
            return Err(Rejected {
                request: sound,
                reason: RejectReason::new(RejectCode::InvalidRequest, "audio has ended"),
            });
        }
        let signal = self.signal.clone();
        let reject = |request| Rejected {
            request,
            reason: RejectReason::new(
                RejectCode::Busy,
                "audio queue or background ownership capacity exhausted",
            ),
        };
        let Some(mut producer) = try_producer(&signal.shared) else {
            return Err(reject(sound));
        };
        let Ok(chunk) = producer.write_chunk_uninit(1) else {
            return Err(reject(sound));
        };
        let queue = self.manager.manager.as_mut().unwrap().queues[playback.queue_slot]
            .as_mut()
            .expect("live playback queue");
        let item = QueuedStaticSound {
            index,
            data: sound.data.clone(),
            retention: Some(sound.budget.clone()),
        };
        if let Err(item) = queue.push(item) {
            drop(item); // The unchanged request still owns both PCM and reservation.
            return Err(reject(sound));
        }
        let current = playback.queue_cursor().0;
        while playback
            .bounds
            .front()
            .is_some_and(|(item, _)| *item < current)
        {
            playback.bounds.pop_front();
        }
        playback
            .bounds
            .push_back((index, PlaybackBounds::new(&sound.data)));
        chunk.fill_from_iter(std::iter::once(Envelope::normal(
            Job::AcceptedData(sound, None),
            &signal,
        )));
        signal.shared.notify();
        Ok(())
    }
    pub(crate) fn queue_bytes(&self) -> u64 {
        let capacity = self.max_sounds as u64;
        capacity
            * ((2 * capacity + 7) * std::mem::size_of::<QueuedStaticSound>() as u64
                + (capacity + 1) * std::mem::size_of::<(u64, PlaybackBounds)>() as u64
                + std::mem::size_of::<Option<StaticSoundQueue>>() as u64)
            + self.stream_cache_bytes
    }
}
/// Small transport metadata; never retains a second PCM allocation.
struct PlaybackBounds {
    sample_rate: u32,
    frames: usize,
    loop_region: Option<(usize, usize)>,
    source: Option<Arc<reel_audio::sound::static_sound::StreamStatus>>,
    source_loop_end: bool,
    explicit_end: bool,
}
impl PlaybackBounds {
    fn new(data: &StaticSoundData) -> Self {
        Self {
            sample_rate: data.sample_rate,
            frames: data.num_frames(),
            source: match &data.frames {
                reel_audio::sound::static_sound::FrameStorage::Stream(source) => source.status(),
                _ => None,
            },
            source_loop_end: data
                .settings
                .loop_region
                .is_some_and(|r| matches!(r.end, EndPosition::EndOfAudio)),
            explicit_end: data.slice.is_some(),
            loop_region: data.settings.loop_region.map(|region| {
                let from = region.start.into_samples(data.sample_rate);
                let to = match region.end {
                    EndPosition::EndOfAudio => data.num_frames(),
                    EndPosition::Custom(end) => end.into_samples(data.sample_rate),
                };
                (from, to)
            }),
        }
    }
    fn frame(&self, position: Duration) -> usize {
        let frame = (position.as_secs_f64() * f64::from(self.sample_rate)).round() as usize;
        let frames = self.frames.min(
            self.source
                .as_ref()
                .cloned()
                .map_or(self.frames, |s| s.len()),
        );
        let region = self
            .loop_region
            .map(|(from, to)| (from, if self.source_loop_end { frames } else { to }));
        match region {
            Some((from, to)) if from >= to => frames,
            Some((from, to)) if frame >= to => from + (frame - from) % (to - from),
            Some(_) => frame,
            None => frame.min(frames),
        }
    }
    fn time(&self, frame: usize) -> Duration {
        Duration::from_secs_f64(frame as f64 / f64::from(self.sample_rate))
    }
}
/// Control wrapper; state/position describe content consumption, not device presentation.
pub struct Playback {
    handle: StaticSoundHandle,
    bounds: std::collections::VecDeque<(u64, PlaybackBounds)>,
    queue_slot: usize,
}
impl Playback {
    /// A worker error or interval invalidated when an unknown source reaches EOF.
    pub fn error(&self) -> Option<String> {
        let current = self.queue_cursor().0;
        let bounds = &self.bounds.iter().find(|(item, _)| *item == current)?.1;
        let source = bounds.source.as_ref()?;
        if let Some(error) = source.error() {
            return Some(error);
        }
        let length = source.len();
        if length != usize::MAX
            && (bounds.explicit_end && bounds.frames > length
                || bounds.loop_region.is_some_and(|(from, to)| {
                    from >= length || !bounds.source_loop_end && to > length
                }))
        {
            return Some("audio interval exceeds the actual source end".into());
        }
        None
    }
    pub fn queue_cursor(&self) -> (u64, f64) {
        self.handle.queue_cursor()
    }
    /// Coherent source cursor and rate ramp, including unapplied control commands.
    pub fn snapshot(&self) -> (u64, f64, AudioRate) {
        let state = self.handle.playback();
        (
            state.item,
            state.position,
            AudioRate {
                value: state.rate.0,
                target: state.rate_target.0,
                remaining: state.rate_remaining,
            },
        )
    }
    pub fn position_seconds(&self) -> f64 {
        self.handle.position()
    }
    /// Seek the existing transport through a coalesced command slot.
    pub fn seek(&mut self, position: Duration) -> Duration {
        if self.is_stopped() {
            return Duration::from_secs_f64(self.position_seconds());
        }
        let current = self.queue_cursor().0;
        let bounds = &self
            .bounds
            .iter()
            .find(|(item, _)| *item == current)
            .expect("current transport metadata")
            .1;
        let position = bounds.time(bounds.frame(position));
        self.handle.seek_to(position.as_secs_f64());
        position
    }
    pub fn is_stopped(&self) -> bool {
        self.handle.state() == PlaybackState::Stopped
    }
    /// Coalesced Kira command slots; never competes for the bounded play queue.
    pub fn set_volume_db(&mut self, db: f32, duration: Duration) -> Result<(), AudioError> {
        let db = validate_db(db)?;
        self.handle.set_volume(db, tween(duration));
        Ok(())
    }
    pub fn set_pan(&mut self, pan: f32, duration: Duration) -> Result<(), AudioError> {
        if !(-1.0..=1.0).contains(&pan) {
            return Err(AudioError::new(
                AudioErrorKind::InvalidRequest,
                "pan must be between -1 and 1",
            ));
        }
        self.handle.set_panning(pan, tween(duration));
        Ok(())
    }
    /// Change speed and pitch through the existing coalesced transport command.
    pub fn set_playback_rate(&mut self, rate: f64, duration: Duration) -> Result<(), AudioError> {
        validate_rate(rate)?;
        self.handle.set_playback_rate(rate, tween(duration));
        Ok(())
    }
    pub fn fade_out(&mut self, duration: Duration) {
        self.handle.stop(tween(duration));
    }
    pub fn pause(&mut self) {
        self.handle.pause(immediate());
    }
    pub fn resume(&mut self) {
        self.handle.resume(immediate());
    }
    pub fn stop(&mut self) {
        self.handle.stop(immediate());
    }
}
fn validate_db(db: f32) -> Result<f32, AudioError> {
    if db.is_nan() || db == f32::INFINITY {
        return Err(AudioError::new(
            AudioErrorKind::InvalidRequest,
            "volume must not be NaN or positive infinity",
        ));
    }
    Ok(db.max(Decibels::SILENCE.0))
}
fn validate_rate(rate: f64) -> Result<(), AudioError> {
    if AudioRate::constant(rate).is_valid() {
        Ok(())
    } else {
        Err(AudioError::new(
            AudioErrorKind::InvalidRequest,
            "audio rate must be finite and between 0.25 and 4",
        ))
    }
}
fn immediate() -> Tween {
    tween(Duration::ZERO)
}
fn tween(duration: Duration) -> Tween {
    Tween {
        duration,
        ..Tween::default()
    }
}

#[cfg(test)]
mod retirement_tests;
