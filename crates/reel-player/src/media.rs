//! Session media assembly. Audio and video share one output and resource service.
use crate::assets::{AssetStream, Assets, StreamControl, worker::Worker};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, BudgetTicket, QueueLimits},
    identity::{Priority, ResourceVersion, Scope},
    wake::WakeSignal,
};
use reel_engine::{
    audio::{AudioQueueItem, AudioRequest},
    media::{MediaBackend, MediaCommand, MediaCompletion, MediaId, MediaPlayback, MediaResult},
};
use reel_media::*;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Duration,
};

// Keep only the latest periodic state. The diagnostic thread owns transport IO;
// a slow reader cannot queue reports or make media maintenance wait for it.
#[cfg(feature = "profiling")]
static MEDIA_REPORT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[cfg(all(
    feature = "performance",
    any(feature = "window", target_os = "vita", test)
))]
pub(crate) fn take_profile_report() -> Option<String> {
    MEDIA_REPORT.try_lock().ok()?.take()
}

#[cfg(all(
    feature = "audio-output",
    any(target_os = "windows", target_os = "linux", target_os = "macos")
))]
mod platform;
#[cfg(all(
    feature = "audio-output",
    any(target_os = "windows", target_os = "linux", target_os = "macos")
))]
pub(crate) use platform::desktop_media;

#[derive(Clone, Debug)]
pub struct MediaOptions {
    /// Background codec selection; playback and content positions stay shared.
    pub decoder: Arc<dyn decode::AudioDecoderFactory>,
    pub mixer: MixerLimits,
    pub pcm_bytes: u64,
    pub static_pcm_threshold_bytes: u64,
    pub stream_window_frames: usize,
    pub queue_bytes: u64,
    #[cfg(feature = "video")]
    pub video: video::VideoLimits,
    /// Worker-owned video decoding selected by the platform assembly.
    #[cfg(feature = "video")]
    pub video_decoder: Option<Arc<dyn reel_media::video::VideoDecoderFactory>>,
}
impl Default for MediaOptions {
    fn default() -> Self {
        Self {
            decoder: Arc::new(decode::SoftwareAudioDecoderFactory),
            mixer: MixerLimits::default(),
            pcm_bytes: 256 << 20,
            static_pcm_threshold_bytes: 8 << 20,
            stream_window_frames: 16_384,
            queue_bytes: 64 << 20,
            #[cfg(feature = "video")]
            video: video::VideoLimits::default(),
            #[cfg(feature = "video")]
            video_decoder: {
                #[cfg(feature = "video-software")]
                {
                    Some(Arc::new(reel_media::video::SoftwareVideoDecoderFactory))
                }
                #[cfg(not(feature = "video-software"))]
                {
                    None
                }
            },
        }
    }
}
struct Decode {
    id: MediaId,
    item: u64,
    reader: AssetStream,
    control: StreamControl,
    pool: BudgetPool,
    threshold: u64,
    window: usize,
}
struct Decoded {
    id: MediaId,
    item: u64,
    version: Option<ResourceVersion>,
    sound: Result<DecodedSound, String>,
}
enum Phase {
    Loading,
    Queued(StreamControl),
    Ready(ResourceVersion, Box<DecodedSound>),
    Playing(reel_media::MediaId),
    Done(MediaResult),
}
struct Entry {
    io_deadline: Option<reel_core::io::ReadDeadline>,
    request: AudioRequest,
    phase: Phase,
    paused: bool,
    queue: VecDeque<Segment>,
    item: u64,
    tail: u64,
    key: u64,
    initial_batch: Option<u64>,
    started: bool,
    descriptor: BudgetTicket,
    queue_storage: Option<BudgetTicket>,
}
struct Segment {
    io_deadline: Option<reel_core::io::ReadDeadline>,
    item: u64,
    key: u64,
    request: AudioQueueItem,
    phase: Phase,
    descriptor: BudgetTicket,
}
impl Entry {
    fn refresh_deadlines(&mut self, suspended: bool) {
        fn capture(phase: &Phase, deadline: &mut Option<reel_core::io::ReadDeadline>) {
            if deadline.is_none()
                && let Phase::Queued(control) = phase
            {
                *deadline = Some(control.read_deadline());
            }
        }
        capture(&self.phase, &mut self.io_deadline);
        if let Some(deadline) = &self.io_deadline {
            deadline.rate(self.request.rate.value.max(self.request.rate.target));
            deadline.pause(
                suspended
                    || self.paused
                    || !self.started
                    || !matches!(self.phase, Phase::Playing(_)),
            );
        }
        for segment in &mut self.queue {
            capture(&segment.phase, &mut segment.io_deadline);
            if let Some(deadline) = &segment.io_deadline {
                deadline.pause(true);
            }
        }
    }
    fn observe(&mut self, state: &reel_media::AudioActivitySnapshot<'_>) {
        self.advance_to(state.item, Duration::from_secs_f64(state.position_seconds));
        self.request.rate = state.rate;
    }
    fn advance_to(&mut self, item: u64, position: Duration) {
        while self
            .queue
            .front()
            .is_some_and(|segment| segment.item <= item)
        {
            let segment = self.queue.pop_front().unwrap();
            self.io_deadline = segment.io_deadline;
            self.item = segment.item;
            self.key = segment.key;
            self.request.source = segment.request.source;
            self.request.loop_region = segment.request.loop_region;
            self.request.end_position = segment.request.end_position;
            self.descriptor = segment.descriptor;
        }
        self.request.start_position = position;
    }
}
fn descriptor_bytes(source: &String) -> u64 {
    (std::mem::size_of::<Entry>() + std::mem::size_of::<Segment>() + source.capacity()) as u64
}
fn resolve_decoded(
    event: Decoded,
    phase: &mut Phase,
    start: &mut Duration,
    end: Option<Duration>,
    loop_region: Option<reel_core::audio::AudioLoopRegion>,
    source: &String,
    descriptor: &mut BudgetTicket,
) {
    descriptor
        .resize(descriptor_bytes(source))
        .expect("release decode input charge");
    *phase = match event.sound.and_then(|sound| {
        sound
            .with_playback(*start, end, loop_region)
            .map_err(|error| error.to_string())
    }) {
        Ok(sound) => {
            *start = sound.start_position();
            match event.version {
                Some(version) => Phase::Ready(version, Box::new(sound)),
                None => Phase::Done(MediaResult::Failed(
                    "audio resource version unavailable".into(),
                )),
            }
        }
        Err(error) => Phase::Done(MediaResult::Failed(format!("{source}: {error}"))),
    };
}
fn cancel_segment(_assets: &Assets, worker: &mut Worker<Decode, Decoded>, key: u64, phase: &Phase) {
    if let Phase::Queued(control) = phase {
        control.cancel();
        worker.cancel(key);
    }
}
struct SegmentLoader<'a> {
    assets: &'a Assets,
    worker: &'a mut Worker<Decode, Decoded>,
    pcm: &'a BudgetPool,
    threshold: u64,
    window: usize,
}
impl SegmentLoader<'_> {
    fn poll(
        &mut self,
        id: MediaId,
        item: u64,
        key: u64,
        source: &str,
        phase: &mut Phase,
        descriptor: &mut BudgetTicket,
    ) {
        if !matches!(phase, Phase::Loading) {
            return;
        }
        let result = self
            .assets
            .stream(source.to_owned())
            .and_then(|(control, reader)| {
                let request = Decode {
                    id,
                    item,
                    reader,
                    control: control.clone(),
                    pool: self.pcm.clone(),
                    threshold: self.threshold,
                    window: self.window,
                };
                match self.worker.accept(key, request, 0, Priority::Visible) {
                    Ok(()) => Ok(control),
                    Err(error) => {
                        control.cancel();
                        Err(error.reason.message)
                    }
                }
            });
        *phase = match result {
            Ok(control) => Phase::Queued(control),
            Err(error) => Phase::Done(MediaResult::Failed(format!("{source}: {error}"))),
        };
        let _ = descriptor;
    }
}

mod output_idle;

type OutputFactory = Box<dyn FnOnce(WakeSignal) -> Result<Box<dyn AudioOutput>, String>>;

/// A session-local backend. A custom host supplies AssetIo through Assets and
/// any AudioOutput; neither the engine nor this coordinator requires a window.
pub struct MediaPlayer {
    assets: Assets,
    options: MediaOptions,
    wake: WakeSignal,
    factory: Option<OutputFactory>,
    output: Option<Box<dyn AudioOutput>>,
    pending_fill: Option<PcmFill>,
    service: Option<AudioActivityService>,
    reclaim: Option<AudioReclaimService>,
    owner: Option<AudioOwner>,
    pcm: BudgetPool,
    descriptors: BudgetPool,
    #[cfg(feature = "video")]
    movies: video::Movies,
    worker: Worker<Decode, Decoded>,
    decoded: Vec<Decoded>,
    events: Vec<AudioActivityEvent>,
    entries: BTreeMap<MediaId, Entry>,
    playing: BTreeMap<u64, MediaId>,
    cursor: Option<MediaId>,
    next: u64,
    closed: bool,
    suspended: bool,
    output_idle: output_idle::OutputIdle,
    failure: Option<String>,
    reclaim_pending: bool,
    #[cfg(feature = "profiling")]
    profile_at: std::time::Instant,
}
impl MediaPlayer {
    #[cfg(feature = "video")]
    pub fn play_video_audio(
        &mut self,
        sound: reel_media::video::StreamSoundData,
    ) -> Result<(), String> {
        self.initialize()?;
        self.service
            .as_mut()
            .unwrap()
            .play_video_audio(sound)
            .map_err(|error| error.to_string())?;
        self.run_output()
    }
    pub fn new(
        assets: Assets,
        options: MediaOptions,
        wake: WakeSignal,
        create_output: impl FnOnce(WakeSignal) -> Result<Box<dyn AudioOutput>, String> + 'static,
    ) -> Self {
        let queue = QueueLimits {
            max_requests: u32::try_from(options.mixer.max_sounds).unwrap_or(u32::MAX),
            max_bytes: options.queue_bytes,
        };
        let pcm = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: queue.max_requests.saturating_mul(2),
                max_bytes: options.pcm_bytes,
            },
        );
        let decoder = options.decoder.clone();
        let mut player = Self {
            #[cfg(feature = "video")]
            movies: video::Movies::new(&options.video),
            assets,
            options,
            wake: wake.clone(),
            factory: Some(Box::new(create_output)),
            output: None,
            pending_fill: None,
            service: None,
            reclaim: None,
            owner: None,
            pcm,
            descriptors: BudgetPool::new(
                BudgetCategory::CpuInput,
                QueueLimits {
                    max_requests: queue.max_requests.saturating_mul(2),
                    ..queue
                },
            ),
            worker: Worker::new(
                queue,
                wake,
                move |mut request: Decode, cancelled| {
                    let control = request.control.clone();
                    let cancel_control = control.clone();
                    let sound =
                        request
                            .reader
                            .open()
                            .map_err(|e| e.to_string())
                            .and_then(|length| {
                                DecodedSound::prepare(
                                    request.reader,
                                    length,
                                    request.pool,
                                    reel_media::DecodeLimits {
                                        io_deadline: Some(control.read_deadline()),
                                        static_threshold: request.threshold,
                                        window_frames: request.window,
                                    },
                                    Arc::new(move || cancel_control.cancel()),
                                    cancelled,
                                    decoder.as_ref(),
                                )
                            });
                    if sound.is_err() {
                        control.cancel();
                    }
                    Decoded {
                        id: request.id,
                        item: request.item,
                        version: control.version(),
                        sound,
                    }
                },
                |event| event.sound = Err("audio decode cancelled".into()),
            ),
            decoded: Vec::with_capacity(queue.max_requests as usize),
            events: Vec::with_capacity(queue.max_requests as usize),
            entries: BTreeMap::new(),
            playing: BTreeMap::new(),
            cursor: None,
            next: 0,
            closed: false,
            suspended: false,
            output_idle: output_idle::OutputIdle::default(),
            failure: None,
            reclaim_pending: false,
            #[cfg(feature = "profiling")]
            profile_at: std::time::Instant::now(),
        };
        // Device preparation belongs to host assembly, before executing scripts.
        // Games without audio can still run if this host has no output device.
        if let Err(error) = player.initialize() {
            player.failure = Some(error);
        }
        player
    }
    fn initialize(&mut self) -> Result<(), String> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if let Some(output) = &self.output {
            return if output.status().state == AudioState::Failed {
                Err("audio output failed".into())
            } else {
                Ok(())
            };
        }
        let factory = self.factory.take().ok_or("audio output unavailable")?;
        let mut output = factory(self.wake.clone())?;
        let mut request = reel_media::AudioRequest::default();
        let capabilities = output.capabilities();
        request.max_pull_frames = request.max_pull_frames.max(capabilities.min_pull_frames);
        let format = output.configure(request).map_err(|e| e.to_string())?;
        self.output_idle.can_suspend = output.capabilities().suspend;
        self.output_idle.max_pull_frames = format.max_pull_frames;
        let reclaim = AudioReclaimService::new(
            ReclaimLimits {
                max_generations: 1,
                queued_work: self.options.mixer.max_sounds,
            },
            self.wake.clone(),
        )
        .map_err(|e| e.to_string())?;
        let mixer =
            KiraMixer::new(&format, self.options.mixer, &reclaim).map_err(|e| e.to_string())?;
        let slots = u32::try_from(self.options.mixer.max_sounds)
            .map_err(|_| "audio capacity exceeds addressable slots")?;
        let fixed_bytes = AudioActivityService::slot_bytes(slots, &mixer);
        let mut service = AudioActivityService::new(
            mixer,
            QueueLimits {
                max_requests: slots,
                max_bytes: fixed_bytes.saturating_add(self.options.queue_bytes),
            },
            MediaEpoch(1),
            self.wake.clone(),
        )
        .map_err(|e| e.reason.message)?;
        service.set_output_suspended(true);
        let owner = service
            .register_owner(Scope::Process)
            .map_err(|e| e.message)?;
        // Keep the prepared renderer until a source can actually consume PCM.
        self.owner = Some(owner);
        self.output = Some(output);
        self.service = Some(service);
        self.reclaim = Some(reclaim);
        Ok(())
    }
    fn output_failure(&mut self) {
        if self.closed
            && self.failure.is_none()
            && let Some(error) = self
                .output
                .as_ref()
                .and_then(|output| output.status().latest_device_error)
        {
            self.failure = Some(format!("audio close: {error}"));
        }
        if let Some(output) = &self.output
            && (output.status().state == AudioState::Failed || self.failure.is_some())
            && let Some(service) = &mut self.service
        {
            let status = output.status();
            let reason = status
                .latest_device_error
                .map_or(AudioErrorKind::DeviceFailure, |e| e.kind);
            let first_failure = service.fail_output(reason).is_ok();
            let message = self
                .failure
                .get_or_insert_with(|| format!("audio output: {reason:?}"))
                .clone();
            if first_failure {
                for entry in self.entries.values_mut() {
                    if let Phase::Playing(media) = entry.phase
                        && let Some(state) = service.snapshot(media)
                    {
                        entry.observe(&state);
                    }
                    for segment in entry.queue.drain(..) {
                        cancel_segment(&self.assets, &mut self.worker, segment.key, &segment.phase);
                    }
                    if !matches!(entry.phase, Phase::Playing(_) | Phase::Done(_)) {
                        if let Phase::Queued(ref control) = entry.phase {
                            control.cancel();
                            self.worker.cancel(entry.key);
                        }
                        entry.phase = Phase::Done(MediaResult::Failed(message.clone()));
                    }
                }
            }
        }
        if self.failure.is_some() {
            drop(self.pending_fill.take());
            if let Some(output) = &mut self.output
                && output.status().state != AudioState::Stopped
            {
                let _ = output.stop();
            }
            if self
                .output
                .as_ref()
                .is_some_and(|output| output.close_complete())
            {
                self.output = None;
            }
        }
    }
    fn stop_entry(&mut self, id: MediaId, deliver: bool) {
        if let Some(entry) = self.entries.get_mut(&id) {
            if let Phase::Playing(media) = entry.phase
                && let Some(state) = self
                    .service
                    .as_ref()
                    .and_then(|service| service.snapshot(media))
            {
                entry.observe(&state);
            }
            for segment in entry.queue.drain(..) {
                cancel_segment(&self.assets, &mut self.worker, segment.key, &segment.phase);
            }
            match entry.phase {
                Phase::Queued(ref control) => {
                    control.cancel();
                    self.worker.cancel(entry.key);
                }
                Phase::Playing(media) => {
                    if let Some(service) = &mut self.service {
                        service.stop(media);
                    }
                }
                _ => {}
            }
            if deliver && !matches!(entry.phase, Phase::Done(_) | Phase::Playing(_)) {
                entry.phase = Phase::Done(MediaResult::Stopped);
            }
        }
        if !deliver
            && let Some(entry) = self.entries.remove(&id)
            && let Phase::Playing(media) = entry.phase
        {
            self.playing.remove(&media.0);
        }
    }
    fn active_count(&self) -> usize {
        let audio = self
            .entries
            .values()
            .map(|entry| 1 + entry.queue.len())
            .sum::<usize>();
        #[cfg(feature = "video")]
        {
            audio + self.movies.entries.len() + self.movies.retiring.len()
        }
        #[cfg(not(feature = "video"))]
        {
            audio
        }
    }
    fn audio_capacity_full(&self) -> bool {
        self.active_count() >= self.options.mixer.max_sounds
    }
    fn video_pending(&self) -> bool {
        #[cfg(feature = "video")]
        {
            !self.movies.entries.is_empty() || !self.movies.retiring.is_empty()
        }
        #[cfg(not(feature = "video"))]
        {
            false
        }
    }
    #[cfg(test)]
    fn audio_snapshot(&self, id: MediaId) -> Result<reel_engine::audio::AudioPlayback, String> {
        match self.snapshot(id)? {
            MediaPlayback::Audio(state) => Ok(state),
            MediaPlayback::Video(_) => Err("not an audio playback".into()),
        }
    }
    pub fn pcm_usage(&self) -> reel_core::budget::BudgetUsage {
        self.pcm.usage()
    }
    #[cfg(feature = "profiling")]
    fn profile(&self, phase: &str) -> String {
        let output = self
            .output
            .as_ref()
            .map(|output| (output.status(), output.clock_observation().generated_frames));
        let report = format!(
            "[reel-media] {phase} output={output:?} pcm={:?} audio={} reclaim={:?}\n",
            self.pcm.usage(),
            self.entries.len(),
            self.reclaim.as_ref().map(|reclaim| reclaim.status())
        );
        #[cfg(feature = "video")]
        let report = {
            let mut report = report;
            self.movies.profile(phase, &mut report);
            report
        };
        report
    }
}
fn db(volume: f64) -> f32 {
    if volume == 0. {
        -60.0
    } else {
        (20. * volume.log10()) as f32
    }
}
fn media_result(terminal: AudioActivityTerminal) -> MediaResult {
    match terminal {
        AudioActivityTerminal::Completed { .. } => MediaResult::Completed,
        AudioActivityTerminal::Stopped { .. } => MediaResult::Stopped,
        AudioActivityTerminal::Cancelled => MediaResult::Cancelled,
        AudioActivityTerminal::Failed(error) => {
            MediaResult::Failed(format!("audio output: {error:?}"))
        }
    }
}
impl MediaBackend for MediaPlayer {
    #[cfg(feature = "video")]
    fn play_video(
        &mut self,
        request: reel_engine::video::VideoRequest,
        paused: bool,
    ) -> Result<MediaId, String> {
        self.start_video(request, paused)
    }
    #[cfg(feature = "video")]
    fn restore_video(
        &mut self,
        state: reel_engine::video::VideoPlaybackState,
    ) -> Result<MediaId, String> {
        self.restore_movie(state)
    }
    #[cfg(feature = "video")]
    fn take_video_frame(&mut self, id: MediaId) -> Option<reel_ui::resources::ResourceLease> {
        self.movies.entries.get_mut(&id)?.frame.take()
    }
    fn position(&self, id: MediaId) -> Option<Duration> {
        #[cfg(feature = "video")]
        if let Some(entry) = self.movies.entries.get(&id) {
            return Some(entry.position());
        }
        let entry = self.entries.get(&id)?;
        if let Phase::Playing(media) = entry.phase {
            let state = self.service.as_ref()?.snapshot(media)?;
            Some(Duration::from_secs_f64(state.position_seconds))
        } else {
            Some(entry.request.start_position)
        }
    }
    fn snapshot(&self, id: MediaId) -> Result<MediaPlayback, String> {
        #[cfg(feature = "video")]
        if let Some(entry) = self.movies.entries.get(&id) {
            return Ok(MediaPlayback::Video(entry.snapshot()));
        }
        let entry = self
            .entries
            .get(&id)
            .ok_or("media activity is no longer retained")?;
        let mut request = entry.request.clone();
        let state = if let Phase::Playing(media) = entry.phase {
            self.service
                .as_ref()
                .and_then(|service| service.snapshot(media))
        } else {
            None
        };
        let item = state.as_ref().map_or(entry.item, |state| state.item);
        if let Some(segment) = entry.queue.iter().find(|segment| segment.item == item) {
            request.source = segment.request.source.clone();
            request.loop_region = segment.request.loop_region;
            request.end_position = segment.request.end_position;
        }
        request.start_position = state
            .as_ref()
            .map_or(entry.request.start_position, |state| {
                Duration::from_secs_f64(state.position_seconds)
            });
        if let Some(state) = &state {
            request.rate = state.rate;
        }
        Ok(MediaPlayback::Audio(reel_engine::audio::AudioPlayback {
            request,
            queue: entry
                .queue
                .iter()
                .filter(|segment| segment.item > item)
                .map(|segment| segment.request.clone())
                .collect(),
            paused: entry.paused,
            result: if let Phase::Done(result) = &entry.phase {
                Some(result.clone())
            } else {
                state.and_then(|state| state.terminal).map(media_result)
            },
        }))
    }
    fn play_audio(&mut self, request: AudioRequest, paused: bool) -> Result<MediaId, String> {
        if self.closed {
            return Err("audio session closed".into());
        }
        if !request.volume.is_finite() || request.volume < 0. {
            return Err("audio volume must be finite and nonnegative".into());
        }
        if !(-1.0..=1.0).contains(&request.pan) {
            return Err("audio pan must be between -1 and 1".into());
        }
        if !request.rate.is_valid() {
            return Err("audio rate and target must be finite and between 0.25 and 4".into());
        }
        if self.audio_capacity_full() {
            return Err("audio activity capacity exhausted".into());
        }
        self.initialize()?;
        let descriptor = self
            .descriptors
            .reserve(descriptor_bytes(&request.source))
            .map_err(|error| error.message)?;
        self.next = self.next.checked_add(1).ok_or("audio identity exhausted")?;
        let id = MediaId(self.next);
        self.entries.insert(
            id,
            Entry {
                io_deadline: None,
                request,
                phase: Phase::Loading,
                paused,
                queue: VecDeque::new(),
                item: 0,
                tail: 0,
                key: id.0,
                initial_batch: None,
                started: false,
                descriptor,
                queue_storage: None,
            },
        );
        Ok(id)
    }
    fn commit_audio(&mut self, id: MediaId) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.initial_batch.get_or_insert(entry.tail);
        }
    }
    fn queue_audio(&mut self, id: MediaId, request: AudioQueueItem) -> Result<(), String> {
        if self.audio_capacity_full() {
            return Err("audio segment capacity exhausted".into());
        }
        let entry = self
            .entries
            .get_mut(&id)
            .ok_or("audio activity is no longer retained")?;
        if matches!(entry.phase, Phase::Done(_)) {
            return Err("audio has ended".into());
        }
        let descriptor = self
            .descriptors
            .reserve(descriptor_bytes(&request.source))
            .map_err(|error| error.message)?;
        let item = entry
            .tail
            .checked_add(1)
            .ok_or("audio segment identity exhausted")?;
        let key = self.next.checked_add(1).ok_or("audio identity exhausted")?;
        if entry.queue.len() == entry.queue.capacity() {
            let capacity = (entry.queue.len() * 2)
                .max(1)
                .min(self.options.mixer.max_sounds - 1);
            let storage = self
                .descriptors
                .reserve((capacity * std::mem::size_of::<Segment>()) as u64)
                .map_err(|error| error.message)?;
            entry
                .queue
                .try_reserve_exact(capacity - entry.queue.len())
                .map_err(|error| error.to_string())?;
            entry.queue_storage = Some(storage);
        }
        if let Phase::Playing(media) = entry.phase {
            self.service
                .as_mut()
                .unwrap()
                .announce(media, item)
                .map_err(|error| error.to_string())?;
        }
        self.next = key;
        entry.queue.push_back(Segment {
            io_deadline: None,
            item,
            key: self.next,
            request,
            phase: Phase::Loading,
            descriptor,
        });
        entry.tail = item;
        Ok(())
    }
    fn command(&mut self, id: MediaId, command: MediaCommand) -> Result<(), String> {
        #[cfg(feature = "video")]
        if self.movies.entries.contains_key(&id) {
            return self.video_command(id, command);
        }
        if matches!(command, MediaCommand::Stop)
            || matches!(command, MediaCommand::FadeOut(duration) if duration.is_zero() || self.entries.get(&id).is_some_and(|e| e.paused || !matches!(e.phase, Phase::Playing(_))))
        {
            self.stop_entry(id, true);
            return Ok(());
        }
        let Some(entry) = self.entries.get_mut(&id) else {
            return Ok(());
        };
        if let Phase::Playing(media) = entry.phase
            && let Some(state) = self
                .service
                .as_ref()
                .and_then(|service| service.snapshot(media))
        {
            entry.observe(&state);
        }
        if matches!(entry.phase, Phase::Done(_)) {
            return Ok(());
        }
        match command {
            MediaCommand::Seek(position) => {
                entry.request.start_position = match &mut entry.phase {
                    Phase::Ready(_, sound) => sound.seek(position),
                    Phase::Playing(media) => self
                        .service
                        .as_mut()
                        .unwrap()
                        .seek(*media, position)
                        .map_err(|e| e.to_string())?,
                    _ => position,
                };
            }
            MediaCommand::Volume {
                value: volume,
                duration,
            } => {
                if !volume.is_finite() || volume < 0. {
                    return Err("audio volume must be finite and nonnegative".into());
                }
                if let Phase::Playing(media) = entry.phase {
                    self.service
                        .as_mut()
                        .unwrap()
                        .set_volume_db(media, db(volume), duration)
                        .map_err(|e| e.to_string())?;
                }
                entry.request.volume = volume;
            }
            MediaCommand::Pan { value, duration } => {
                if !(-1.0..=1.0).contains(&value) {
                    return Err("audio pan must be between -1 and 1".into());
                }
                if let Phase::Playing(media) = entry.phase {
                    self.service
                        .as_mut()
                        .unwrap()
                        .set_pan(media, value as f32, duration)
                        .map_err(|e| e.to_string())?;
                }
                entry.request.pan = value;
            }
            MediaCommand::Rate { value, duration } => {
                let rate = entry.request.rate.change(value, duration);
                if !rate.is_valid() {
                    return Err("audio rate must be finite and between 0.25 and 4".into());
                }
                if let Phase::Playing(media) = entry.phase {
                    self.service
                        .as_mut()
                        .unwrap()
                        .set_playback_rate(media, value, duration)
                        .map_err(|error| error.to_string())?;
                }
                entry.request.rate = rate;
            }
            MediaCommand::FadeOut(duration) => {
                if let Phase::Playing(media) = entry.phase {
                    self.service.as_mut().unwrap().fade_out(media, duration);
                }
            }
            MediaCommand::Pause | MediaCommand::Resume => {
                entry.paused = matches!(command, MediaCommand::Pause);
                if let Phase::Playing(media) = entry.phase {
                    let service = self.service.as_mut().unwrap();
                    if entry.paused {
                        service.pause(media);
                    } else {
                        service.resume(media);
                    }
                }
            }
            MediaCommand::Stop => unreachable!(),
            MediaCommand::Finish => {
                return Err("finish is available for finite video playback".into());
            }
        }
        entry.refresh_deadlines(self.suspended);
        self.maintain_output();
        self.output_failure();
        Ok(())
    }
    fn group_volume(
        &mut self,
        group: Option<AudioGroup>,
        value: f64,
        duration: Duration,
    ) -> Result<(), String> {
        if self.closed {
            return Err("audio session closed".into());
        }
        if !value.is_finite() || value < 0. {
            return Err("audio volume must be finite and nonnegative".into());
        }
        self.initialize()?;
        self.service
            .as_mut()
            .unwrap()
            .set_group_volume_db(group, db(value), duration)
            .map_err(|e| e.to_string())
    }
    fn cancel(&mut self, id: MediaId) {
        #[cfg(feature = "video")]
        self.movies.cancel(id);
        self.stop_entry(id, false);
    }
    fn maintain(&mut self, elapsed: Duration) {
        #[cfg(feature = "profiling")]
        if self.profile_at.elapsed() >= Duration::from_secs(1) {
            self.profile_at = std::time::Instant::now();
            let _observer = reel_core::profile::Scope::new(reel_core::profile::Phase::Observer);
            let report = self.profile("playing");
            if let Ok(mut pending) = MEDIA_REPORT.try_lock() {
                *pending = Some(report);
            }
        }
        self.wake.acknowledge();
        self.assets.maintain();
        self.output_failure();
        #[cfg(feature = "video")]
        self.maintain_video(elapsed);
        #[cfg(not(feature = "video"))]
        let _ = elapsed;
        let decoded = reel_core::profile::Scope::new(reel_core::profile::Phase::MediaDecoded);
        self.worker.drain(&mut self.decoded, 64);
        for event in self.decoded.drain(..) {
            if let Some(entry) = self.entries.get_mut(&event.id) {
                if event.item == entry.item && matches!(entry.phase, Phase::Queued(_)) {
                    resolve_decoded(
                        event,
                        &mut entry.phase,
                        &mut entry.request.start_position,
                        entry.request.end_position,
                        entry.request.loop_region,
                        &entry.request.source,
                        &mut entry.descriptor,
                    );
                } else if let Some(segment) = entry.queue.iter_mut().find(|segment| {
                    segment.item == event.item && matches!(segment.phase, Phase::Queued(_))
                }) {
                    resolve_decoded(
                        event,
                        &mut segment.phase,
                        &mut segment.request.start_position,
                        segment.request.end_position,
                        segment.request.loop_region,
                        &segment.request.source,
                        &mut segment.descriptor,
                    );
                }
            }
        }
        drop(decoded);
        let activities = reel_core::profile::Scope::new(reel_core::profile::Phase::MediaActivities);
        use std::ops::Bound::{Excluded, Unbounded};
        let mut start = self.cursor.map_or(Unbounded, Excluded);
        if self.entries.range((start, Unbounded)).next().is_none() {
            start = Unbounded;
        }
        for (&id, entry) in self.entries.range_mut((start, Unbounded)).take(64) {
            self.cursor = Some(id);
            if let Phase::Playing(media) = entry.phase
                && let Some(state) = self.service.as_ref().unwrap().snapshot(media)
            {
                entry.observe(&state);
            }
            let mut loader = SegmentLoader {
                assets: &self.assets,
                worker: &mut self.worker,
                pcm: &self.pcm,
                threshold: self.options.static_pcm_threshold_bytes,
                window: self.options.stream_window_frames,
            };
            loader.poll(
                id,
                entry.item,
                entry.key,
                &entry.request.source,
                &mut entry.phase,
                &mut entry.descriptor,
            );
            for segment in &mut entry.queue {
                loader.poll(
                    id,
                    segment.item,
                    segment.key,
                    &segment.request.source,
                    &mut segment.phase,
                    &mut segment.descriptor,
                );
            }
            entry.refresh_deadlines(self.suspended);
            if !entry.paused && matches!(entry.phase, Phase::Ready(_, _)) {
                let Phase::Ready(version, sound) =
                    std::mem::replace(&mut entry.phase, Phase::Loading)
                else {
                    unreachable!()
                };
                let sound = (*sound)
                    .with_playback_rate(entry.request.rate.value)
                    .expect("rate validated at activity admission")
                    .with_volume_db(db(entry.request.volume))
                    .with_pan(entry.request.pan as f32)
                    .with_fade_in(entry.request.fade_in);
                let service = self.service.as_mut().unwrap();
                entry.phase = match service.play_held(AudioActivityRequest {
                    epoch: service.epoch(),
                    owner: self.owner.as_ref().unwrap().clone(),
                    resource: version,
                    group: entry.request.group,
                    sound,
                }) {
                    Ok(media) => {
                        if !entry.request.rate.remaining.is_zero() {
                            service
                                .set_playback_rate(
                                    media,
                                    entry.request.rate.target,
                                    entry.request.rate.remaining,
                                )
                                .expect("new held activity with validated rate");
                        }
                        self.playing.insert(media.0, id);
                        if entry.tail != 0 {
                            service
                                .announce(media, entry.tail)
                                .expect("new held activity");
                        }
                        Phase::Playing(media)
                    }
                    Err(error) if error.reason.retryable() => {
                        Phase::Ready(error.request.resource, Box::new(error.request.sound))
                    }
                    Err(error) => Phase::Done(MediaResult::Failed(error.reason.message)),
                };
            }
            if let Phase::Playing(media) = entry.phase {
                let service = self.service.as_mut().unwrap();
                for segment in &mut entry.queue {
                    if matches!(segment.phase, Phase::Playing(_)) {
                        continue;
                    }
                    if !matches!(segment.phase, Phase::Ready(_, _)) {
                        break;
                    }
                    let Phase::Ready(version, sound) =
                        std::mem::replace(&mut segment.phase, Phase::Loading)
                    else {
                        unreachable!()
                    };
                    match service.queue(media, segment.item, *sound) {
                        Ok(()) => segment.phase = Phase::Playing(media),
                        Err(error) if error.reason.retryable() => {
                            segment.phase = Phase::Ready(version, Box::new(error.request));
                            break;
                        }
                        Err(error) => {
                            segment.phase = Phase::Done(MediaResult::Failed(error.reason.message));
                            break;
                        }
                    }
                }
                if !entry.started
                    && entry.initial_batch.is_some_and(|tail| {
                        entry
                            .queue
                            .iter()
                            .filter(|segment| segment.item <= tail)
                            .all(|segment| matches!(segment.phase, Phase::Playing(_)))
                    })
                {
                    service.start(media);
                    entry.started = true;
                }
            }
            entry.refresh_deadlines(self.suspended);
            let failure = entry
                .queue
                .iter()
                .find_map(|segment| match &segment.phase {
                    Phase::Done(MediaResult::Failed(error)) => Some(error.clone()),
                    _ => None,
                })
                .or_else(|| match &entry.phase {
                    Phase::Done(MediaResult::Failed(error)) => Some(error.clone()),
                    _ => None,
                });
            if let Some(error) = failure {
                if let Phase::Playing(media) = entry.phase {
                    self.service.as_mut().unwrap().stop(media);
                    self.playing.remove(&media.0);
                } else {
                    cancel_segment(&self.assets, &mut self.worker, entry.key, &entry.phase);
                }
                for segment in entry.queue.drain(..) {
                    cancel_segment(&self.assets, &mut self.worker, segment.key, &segment.phase);
                }
                entry.phase = Phase::Done(MediaResult::Failed(error));
            }
        }
        drop(activities);
        if let Some(service) = &mut self.service {
            let _timing =
                reel_core::profile::Scope::new(reel_core::profile::Phase::MediaAudioService);
            self.reclaim_pending = service.maintain(64, 64).reclaim.needs_maintenance;
            service.drain(&mut self.events, 64, |id, _, _| {
                self.playing.contains_key(&id.0)
            });
            for event in self.events.drain(..) {
                if let Some(id) = self.playing.remove(&event.id.0)
                    && let Some(entry) = self.entries.get_mut(&id)
                {
                    entry.advance_to(event.item, Duration::from_secs_f64(event.position_seconds));
                    entry.request.rate = event.rate;
                    for segment in entry.queue.drain(..) {
                        cancel_segment(&self.assets, &mut self.worker, segment.key, &segment.phase);
                    }
                    entry.phase = Phase::Done(match media_result(event.terminal) {
                        MediaResult::Failed(error) => {
                            MediaResult::Failed(format!("{}: {error}", entry.request.source))
                        }
                        result => result,
                    });
                }
            }
        }
        self.maintain_output();
        self.output_failure();
        if self.closed
            && self
                .output
                .as_ref()
                .is_some_and(|output| output.close_complete())
        {
            self.output = None;
        }
        if self.failure.is_some()
            && self
                .service
                .as_ref()
                .is_some_and(|service| service.occupied() == 0)
        {
            self.service = None;
            self.reclaim_pending = false;
        }
    }
    fn take_result(&mut self, id: MediaId) -> Option<MediaCompletion> {
        #[cfg(feature = "video")]
        if self.movies.entries.contains_key(&id) {
            return self.movies.take_result(id);
        }
        if !self
            .entries
            .get(&id)
            .is_some_and(|e| matches!(e.phase, Phase::Done(_)))
        {
            return None;
        }
        let entry = self.entries.remove(&id).unwrap();
        let Phase::Done(result) = entry.phase else {
            unreachable!()
        };
        Some(MediaCompletion {
            result,
            position: entry.request.start_position,
        })
    }
    fn poll_after(&self) -> Option<Duration> {
        let active = self.output_idle.quiet.is_some()
            || self.video_pending()
            || !self.entries.is_empty()
            || self.cleanup_pending()
            || self.reclaim_pending
            || self.service.as_ref().is_some_and(|s| s.occupied() != 0)
            || self.pcm.usage().bytes != 0;
        if !active {
            return None;
        }
        if self.wake.is_pending() {
            return Some(Duration::ZERO);
        }
        let interval = Duration::from_millis(10);
        #[cfg(feature = "video")]
        let interval = self
            .movies
            .next_frame_after()
            .map_or(interval, |frame| frame.min(interval));
        Some(interval)
    }
    fn suspend(&mut self) {
        self.suspended = true;
        for entry in self.entries.values_mut() {
            entry.refresh_deadlines(true);
        }
        #[cfg(feature = "video")]
        for entry in self.movies.entries.values() {
            entry.pause(true);
        }
        if let Some(output) = &mut self.output
            && output.status().state == AudioState::Running
        {
            let _ = output.suspend();
        }
        if let Some(service) = &mut self.service {
            service.set_output_suspended(true);
        }
        self.output_failure();
    }
    fn resume(&mut self) {
        self.suspended = false;
        if let Some(service) = &mut self.service {
            service.set_output_suspended(false);
        }
        for entry in self.entries.values_mut() {
            entry.refresh_deadlines(false);
        }
        #[cfg(feature = "video")]
        for entry in self.movies.entries.values() {
            entry.pause(entry.paused);
        }
        self.maintain_output();
        self.output_failure();
    }
    fn close(&mut self) {
        if self.closed {
            // Explicit retry uses the same retained native owner. Maintenance
            // only observes completion and never loops native release attempts.
            if let Some(output) = &mut self.output
                && !output.close_complete()
                && output.status().latest_device_error.is_some()
            {
                let _ = output.stop();
            }
            return;
        }
        self.closed = true;
        #[cfg(feature = "video")]
        self.movies.close();
        while let Some(&id) = self.entries.keys().next() {
            self.stop_entry(id, false);
        }
        if let Some(output) = &mut self.output {
            let _ = output.stop();
        }
        #[cfg(feature = "profiling")]
        {
            if let Ok(mut pending) = MEDIA_REPORT.try_lock() {
                *pending = None;
            }
            let report = self.profile("stopped");
            let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), report.as_bytes());
        }
        drop(self.pending_fill.take());
        if self
            .output
            .as_ref()
            .is_some_and(|output| output.close_complete())
        {
            self.output = None;
        }
        self.service = None; // Existing deferred teardown owns all accepted PCM.
        self.reclaim_pending = false;
        self.factory = None;
    }
    fn cleanup_pending(&self) -> bool {
        // Cancelled asset reads belong to the shared service. Waiting for its
        // global input pool would also wait for the replacement session's work.
        self.closed
            && (self
                .output
                .as_ref()
                .is_some_and(|output| !output.close_complete())
                || self.pcm.usage().bytes != 0
                || self.video_pending()
                || !self.worker.is_idle()
                || self.owner.as_ref().is_some_and(|owner| !owner.is_retired())
                || self.reclaim.as_ref().is_some_and(|reclaim| {
                    let status = reclaim.status();
                    status.live_generations != 0 || status.outstanding_work != 0
                }))
    }
}
impl Drop for MediaPlayer {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests;

#[cfg(feature = "video")]
pub mod video;

#[cfg(all(test, feature = "video-software"))]
mod video_tests;
