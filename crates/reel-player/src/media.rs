//! Session media assembly. Audio and video share one output and resource service.
use crate::assets::{AssetKind, Assets, LoadId, worker::Worker};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    identity::{Priority, ResourceVersion, Scope},
    wake::WakeSignal,
};
use reel_engine::{
    audio::AudioRequest,
    media::{MediaBackend, MediaCommand, MediaCompletion, MediaId, MediaPlayback, MediaResult},
};
use reel_media::*;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

#[derive(Clone, Debug)]
pub struct MediaOptions {
    pub mixer: MixerLimits,
    pub pcm_bytes: u64,
    pub queue_bytes: u64,
    #[cfg(feature = "video")]
    pub video: video::VideoLimits,
}
impl Default for MediaOptions {
    fn default() -> Self {
        Self {
            mixer: MixerLimits::default(),
            pcm_bytes: 256 << 20,
            queue_bytes: 64 << 20,
            #[cfg(feature = "video")]
            video: video::VideoLimits::default(),
        }
    }
}
struct Input(Arc<OwnedBytes>);
impl AsRef<[u8]> for Input {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}
struct Decode {
    id: MediaId,
    version: ResourceVersion,
    bytes: Arc<OwnedBytes>,
    ticket: reel_core::budget::BudgetTicket,
}
struct Decoded {
    id: MediaId,
    version: ResourceVersion,
    sound: Result<DecodedSound, String>,
}
enum Phase {
    Loading(LoadId),
    Queued,
    Ready(ResourceVersion, Box<DecodedSound>),
    Playing(reel_media::MediaId),
    Done(MediaResult),
}
struct Entry {
    request: AudioRequest,
    phase: Phase,
    paused: bool,
}
type OutputFactory = Box<dyn FnOnce(WakeSignal) -> Result<Box<dyn AudioOutput>, String>>;

/// A session-local backend. A custom host supplies AssetIo through Assets and
/// any AudioOutput; neither the engine nor this coordinator requires a window.
pub struct MediaPlayer {
    assets: Assets,
    options: MediaOptions,
    wake: WakeSignal,
    factory: Option<OutputFactory>,
    output: Option<Box<dyn AudioOutput>>,
    service: Option<AudioActivityService>,
    reclaim: Option<AudioReclaimService>,
    owner: Option<AudioOwner>,
    pcm: BudgetPool,
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
    failure: Option<String>,
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
            .map_err(|error| error.to_string())
    }
    pub fn new(
        assets: Assets,
        options: MediaOptions,
        wake: WakeSignal,
        create_output: impl FnOnce(WakeSignal) -> Result<Box<dyn AudioOutput>, String> + 'static,
    ) -> Self {
        let asset_wake = wake.clone();
        assets.set_waker(Arc::new(move || asset_wake.wake()));
        let queue = QueueLimits {
            max_requests: u32::try_from(options.mixer.max_sounds).unwrap_or(u32::MAX),
            max_bytes: options.queue_bytes,
        };
        let pcm = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: queue.max_requests,
                max_bytes: options.pcm_bytes,
            },
        );
        let mut player = Self {
            #[cfg(feature = "video")]
            movies: video::Movies::new(&options.video),
            assets,
            options,
            wake: wake.clone(),
            factory: Some(Box::new(create_output)),
            output: None,
            service: None,
            reclaim: None,
            owner: None,
            pcm,
            worker: Worker::new(
                queue,
                wake,
                |request: Decode, cancelled| Decoded {
                    id: request.id,
                    version: request.version,
                    sound: DecodedSound::decode(Input(request.bytes), request.ticket, cancelled),
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
            failure: None,
        };
        // Device preparation belongs to host assembly, before executing scripts.
        // Games without audio can still run if this host has no output device.
        if let Err(error) = player.initialize() {
            player.failure = Some(error);
        }
        player
    }
    #[cfg(all(
        feature = "audio-output",
        any(target_os = "windows", target_os = "linux", target_os = "macos")
    ))]
    pub fn desktop(assets: Assets, options: MediaOptions, wake: WakeSignal) -> Self {
        Self::new(assets, options, wake, |wake| {
            CpalOutput::new(wake, reel_core::clock::MonoTime(0))
                .map(|output| Box::new(output) as Box<dyn AudioOutput>)
                .map_err(|e| e.to_string())
        })
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
        let format = output
            .configure(reel_media::AudioRequest::default())
            .map_err(|e| e.to_string())?;
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
        let mut service = AudioActivityService::new(
            mixer,
            QueueLimits {
                max_requests: slots,
                max_bytes: AudioActivityService::slot_bytes(slots)
                    .saturating_add(self.options.queue_bytes),
            },
            MediaEpoch(1),
            self.wake.clone(),
        )
        .map_err(|e| e.reason.message)?;
        let owner = service
            .register_owner(Scope::Process)
            .map_err(|e| e.message)?;
        output
            .start(service.take_fill().unwrap())
            .map_err(|e| e.reason.message)?;
        if self.suspended {
            output.suspend().map_err(|e| e.to_string())?;
        }
        self.owner = Some(owner);
        self.output = Some(output);
        self.service = Some(service);
        self.reclaim = Some(reclaim);
        Ok(())
    }
    fn output_failure(&mut self) {
        if let Some(output) = &self.output
            && output.status().state == AudioState::Failed
            && let Some(service) = &mut self.service
        {
            let status = output.status();
            let reason = status
                .latest_device_error
                .map_or(AudioErrorKind::DeviceFailure, |e| e.kind);
            let _ = service.fail_output(reason);
            self.failure = Some(format!("audio output: {reason:?}"));
            for (&id, entry) in &mut self.entries {
                if !matches!(entry.phase, Phase::Playing(_) | Phase::Done(_)) {
                    match entry.phase {
                        Phase::Loading(id) => self.assets.cancel(id),
                        Phase::Queued => self.worker.cancel(id.0),
                        _ => {}
                    }
                    entry.phase =
                        Phase::Done(MediaResult::Failed(format!("audio output: {reason:?}")));
                }
            }
        }
        if self.failure.is_some()
            && let Some(mut output) = self.output.take()
        {
            let _ = output.stop();
        }
    }
    fn stop_entry(&mut self, id: MediaId, deliver: bool) {
        if let Some(entry) = self.entries.get_mut(&id) {
            match entry.phase {
                Phase::Loading(load) => self.assets.cancel(load),
                Phase::Queued => self.worker.cancel(id.0),
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
        #[cfg(feature = "video")]
        {
            self.entries.len() + self.movies.entries.len()
        }
        #[cfg(not(feature = "video"))]
        {
            self.entries.len()
        }
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
}
fn db(volume: f64) -> f32 {
    if volume == 0. {
        -60.0
    } else {
        (20. * volume.log10()) as f32
    }
}
impl MediaBackend for MediaPlayer {
    #[cfg(feature = "video")]
    fn play_video(&mut self, request: reel_engine::video::VideoRequest) -> Result<MediaId, String> {
        self.start_video(request)
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
        request.start_position = self.position(id).ok_or("audio transport missing")?;
        Ok(MediaPlayback::Audio(reel_engine::audio::AudioPlayback {
            request,
            paused: entry.paused,
            result: if let Phase::Done(result) = &entry.phase {
                Some(result.clone())
            } else {
                None
            },
        }))
    }
    fn play_audio(&mut self, request: AudioRequest) -> Result<MediaId, String> {
        if self.closed {
            return Err("audio session closed".into());
        }
        if !request.volume.is_finite() || request.volume < 0. {
            return Err("audio volume must be finite and nonnegative".into());
        }
        if !(-1.0..=1.0).contains(&request.pan) {
            return Err("audio pan must be between -1 and 1".into());
        }
        if self.active_count() >= self.options.mixer.max_sounds {
            return Err("audio activity capacity exhausted".into());
        }
        self.initialize()?;
        self.next = self.next.checked_add(1).ok_or("audio identity exhausted")?;
        let id = MediaId(self.next);
        let load = self.assets.request(&request.source, AssetKind::Bytes);
        self.entries.insert(
            id,
            Entry {
                request,
                phase: Phase::Loading(load),
                paused: false,
            },
        );
        Ok(id)
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
        self.wake.acknowledge();
        self.assets.maintain();
        self.output_failure();
        #[cfg(feature = "video")]
        self.maintain_video(elapsed);
        #[cfg(not(feature = "video"))]
        let _ = elapsed;
        self.worker.drain(&mut self.decoded, 64);
        for event in self.decoded.drain(..) {
            if let Some(entry) = self.entries.get_mut(&event.id)
                && matches!(entry.phase, Phase::Queued)
            {
                // Resolve transport settings on the control side: a seek may have
                // arrived after this decode was queued.
                entry.phase = match event.sound.and_then(|sound| {
                    sound
                        .with_playback(entry.request.start_position, entry.request.loop_region)
                        .map_err(|error| error.to_string())
                }) {
                    Ok(sound) => {
                        entry.request.start_position = sound.start_position();
                        Phase::Ready(event.version, Box::new(sound))
                    }
                    Err(error) => Phase::Done(MediaResult::Failed(error)),
                };
            }
        }
        use std::ops::Bound::{Excluded, Unbounded};
        let mut start = self.cursor.map_or(Unbounded, Excluded);
        if self.entries.range((start, Unbounded)).next().is_none() {
            start = Unbounded;
        }
        for (&id, entry) in self.entries.range_mut((start, Unbounded)).take(64) {
            self.cursor = Some(id);
            if let Phase::Loading(load) = entry.phase {
                match self.assets.poll_bytes(load) {
                    Ok(Some((version, bytes))) => {
                        let result =
                            self.pcm
                                .reserve(0)
                                .map_err(|e| e.message)
                                .and_then(|ticket| {
                                    let charge = bytes.as_slice().len() as u64;
                                    self.worker
                                        .accept(
                                            id.0,
                                            Decode {
                                                id,
                                                version,
                                                bytes,
                                                ticket,
                                            },
                                            charge,
                                            Priority::Visible,
                                        )
                                        .map_err(|e| e.reason.message)
                                });
                        entry.phase = match result {
                            Ok(()) => Phase::Queued,
                            Err(e) => Phase::Done(MediaResult::Failed(e)),
                        };
                    }
                    Ok(None) => {}
                    Err(e) => entry.phase = Phase::Done(MediaResult::Failed(e)),
                }
            }
            if !entry.paused && matches!(entry.phase, Phase::Ready(_, _)) {
                let Phase::Ready(version, sound) =
                    std::mem::replace(&mut entry.phase, Phase::Queued)
                else {
                    unreachable!()
                };
                let sound = (*sound)
                    .with_volume_db(db(entry.request.volume))
                    .with_pan(entry.request.pan as f32)
                    .with_fade_in(entry.request.fade_in);
                let service = self.service.as_mut().unwrap();
                entry.phase = match service.play(AudioActivityRequest {
                    epoch: service.epoch(),
                    owner: self.owner.as_ref().unwrap().clone(),
                    resource: version,
                    group: entry.request.group,
                    sound,
                }) {
                    Ok(media) => {
                        self.playing.insert(media.0, id);
                        Phase::Playing(media)
                    }
                    Err(error) if error.reason.retryable() => {
                        Phase::Ready(error.request.resource, Box::new(error.request.sound))
                    }
                    Err(error) => Phase::Done(MediaResult::Failed(error.reason.message)),
                };
            }
        }
        if let Some(service) = &mut self.service {
            service.maintain(64, 64);
            service.drain(&mut self.events, 64, |id, _, _| {
                self.playing.contains_key(&id.0)
            });
            for event in self.events.drain(..) {
                if let Some(id) = self.playing.remove(&event.id.0)
                    && let Some(entry) = self.entries.get_mut(&id)
                {
                    entry.request.start_position = Duration::from_secs_f64(event.position_seconds);
                    entry.phase = Phase::Done(match event.terminal {
                        AudioActivityTerminal::Completed { .. } => MediaResult::Completed,
                        AudioActivityTerminal::Stopped { .. } => MediaResult::Stopped,
                        AudioActivityTerminal::Cancelled => MediaResult::Cancelled,
                        AudioActivityTerminal::Failed(error) => {
                            MediaResult::Failed(format!("audio output: {error:?}"))
                        }
                    });
                }
            }
        }
        if self.failure.is_some()
            && self
                .service
                .as_ref()
                .is_some_and(|service| service.occupied() == 0)
        {
            self.service = None;
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
        (self.video_pending()
            || !self.entries.is_empty()
            || self.cleanup_pending()
            || self.service.as_ref().is_some_and(|s| s.occupied() != 0)
            || self.pcm.usage().bytes != 0)
            .then_some(if self.wake.is_pending() {
                Duration::ZERO
            } else {
                Duration::from_millis(10)
            })
    }
    fn suspend(&mut self) {
        self.suspended = true;
        #[cfg(feature = "video")]
        for entry in self.movies.entries.values() {
            entry.pause(true);
        }
        if let Some(output) = &mut self.output {
            let _ = output.suspend();
        }
        self.output_failure();
    }
    fn resume(&mut self) {
        self.suspended = false;
        #[cfg(feature = "video")]
        for entry in self.movies.entries.values() {
            entry.pause(entry.paused);
        }
        if let Some(output) = &mut self.output {
            let _ = output.resume();
        }
        self.output_failure();
    }
    fn close(&mut self) {
        if self.closed {
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
        self.output = None;
        self.service = None; // Existing deferred teardown owns all accepted PCM.
        self.factory = None;
    }
    fn cleanup_pending(&self) -> bool {
        // Cancelled asset reads belong to the shared service. Waiting for its
        // global input pool would also wait for the replacement session's work.
        self.closed
            && (self.video_pending()
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

#[cfg(all(test, feature = "video"))]
mod video_tests;
