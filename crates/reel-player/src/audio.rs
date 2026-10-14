//! Author audio assembly. IO, decode and PCM each have independent execution lanes.
use crate::assets::{AssetKind, Assets, LoadId, worker::Worker};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    identity::{Priority, ResourceVersion, Scope},
    wake::WakeSignal,
};
use reel_engine::audio::{AudioBackend, AudioCommand, AudioId, AudioRequest, AudioResult};
use reel_media::*;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

#[derive(Clone, Debug)]
pub struct AudioOptions {
    pub mixer: MixerLimits,
    pub pcm_bytes: u64,
    pub queue_bytes: u64,
}
impl Default for AudioOptions {
    fn default() -> Self {
        Self {
            mixer: MixerLimits::default(),
            pcm_bytes: 256 << 20,
            queue_bytes: 64 << 20,
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
    id: AudioId,
    version: ResourceVersion,
    bytes: Arc<OwnedBytes>,
    ticket: reel_core::budget::BudgetTicket,
}
struct Decoded {
    id: AudioId,
    version: ResourceVersion,
    sound: Result<DecodedSound, String>,
}
enum Phase {
    Loading(LoadId),
    Queued,
    Ready(ResourceVersion, Box<DecodedSound>),
    Playing(MediaId),
    Done(AudioResult),
}
struct Entry {
    request: AudioRequest,
    phase: Phase,
    paused: bool,
}
type OutputFactory = Box<dyn FnOnce(WakeSignal) -> Result<Box<dyn AudioOutput>, String>>;

/// A session-local backend. A custom host supplies AssetIo through Assets and
/// any AudioOutput; neither the engine nor this coordinator requires a window.
pub struct AudioPlayer {
    assets: Assets,
    options: AudioOptions,
    wake: WakeSignal,
    factory: Option<OutputFactory>,
    output: Option<Box<dyn AudioOutput>>,
    service: Option<AudioActivityService>,
    reclaim: Option<AudioReclaimService>,
    owner: Option<AudioOwner>,
    pcm: BudgetPool,
    worker: Worker<Decode, Decoded>,
    decoded: Vec<Decoded>,
    events: Vec<AudioActivityEvent>,
    entries: BTreeMap<AudioId, Entry>,
    playing: BTreeMap<u64, AudioId>,
    cursor: Option<AudioId>,
    next: u64,
    closed: bool,
    suspended: bool,
    failure: Option<String>,
}
impl AudioPlayer {
    pub fn new(
        assets: Assets,
        options: AudioOptions,
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
    pub fn desktop(assets: Assets, options: AudioOptions, wake: WakeSignal) -> Self {
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
                        Phase::Done(AudioResult::Failed(format!("audio output: {reason:?}")));
                }
            }
        }
        if self.failure.is_some()
            && let Some(mut output) = self.output.take()
        {
            let _ = output.stop();
        }
    }
    fn stop_entry(&mut self, id: AudioId, deliver: bool) {
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
                entry.phase = Phase::Done(AudioResult::Stopped);
            }
        }
        if !deliver
            && let Some(entry) = self.entries.remove(&id)
            && let Phase::Playing(media) = entry.phase
        {
            self.playing.remove(&media.0);
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
impl AudioBackend for AudioPlayer {
    fn play(&mut self, request: AudioRequest) -> Result<AudioId, String> {
        if self.closed {
            return Err("audio session closed".into());
        }
        if !request.volume.is_finite() || request.volume < 0. {
            return Err("audio volume must be finite and nonnegative".into());
        }
        if !(-1.0..=1.0).contains(&request.pan) {
            return Err("audio pan must be between -1 and 1".into());
        }
        if self.entries.len() >= self.options.mixer.max_sounds {
            return Err("audio activity capacity exhausted".into());
        }
        self.initialize()?;
        self.next = self.next.checked_add(1).ok_or("audio identity exhausted")?;
        let id = AudioId(self.next);
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
    fn command(&mut self, id: AudioId, command: AudioCommand) -> Result<(), String> {
        if matches!(command, AudioCommand::Stop)
            || matches!(command, AudioCommand::FadeOut(duration) if duration.is_zero() || self.entries.get(&id).is_some_and(|e| e.paused || !matches!(e.phase, Phase::Playing(_))))
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
            AudioCommand::Volume {
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
            AudioCommand::Pan { value, duration } => {
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
            AudioCommand::FadeOut(duration) => {
                if let Phase::Playing(media) = entry.phase {
                    self.service.as_mut().unwrap().fade_out(media, duration);
                }
            }
            AudioCommand::Pause | AudioCommand::Resume => {
                entry.paused = matches!(command, AudioCommand::Pause);
                if let Phase::Playing(media) = entry.phase {
                    let service = self.service.as_mut().unwrap();
                    if entry.paused {
                        service.pause(media);
                    } else {
                        service.resume(media);
                    }
                }
            }
            AudioCommand::Stop => unreachable!(),
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
    fn cancel(&mut self, id: AudioId) {
        self.stop_entry(id, false);
    }
    fn maintain(&mut self) {
        self.wake.acknowledge();
        self.assets.maintain();
        self.output_failure();
        self.worker.drain(&mut self.decoded, 64);
        for event in self.decoded.drain(..) {
            if let Some(entry) = self.entries.get_mut(&event.id)
                && matches!(entry.phase, Phase::Queued)
            {
                entry.phase = match event.sound {
                    Ok(sound) => Phase::Ready(event.version, Box::new(sound)),
                    Err(error) => Phase::Done(AudioResult::Failed(error)),
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
                            Err(e) => Phase::Done(AudioResult::Failed(e)),
                        };
                    }
                    Ok(None) => {}
                    Err(e) => entry.phase = Phase::Done(AudioResult::Failed(e)),
                }
            }
            if !entry.paused && matches!(entry.phase, Phase::Ready(_, _)) {
                let Phase::Ready(version, sound) =
                    std::mem::replace(&mut entry.phase, Phase::Queued)
                else {
                    unreachable!()
                };
                let mut sound = *sound;
                if entry.request.looping {
                    sound = sound.looping();
                }
                sound = sound
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
                    Err(error) => Phase::Done(AudioResult::Failed(error.reason.message)),
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
                    entry.phase = Phase::Done(match event.terminal {
                        AudioActivityTerminal::Completed { .. } => AudioResult::Completed,
                        AudioActivityTerminal::Stopped { .. } => AudioResult::Stopped,
                        AudioActivityTerminal::Cancelled => AudioResult::Cancelled,
                        AudioActivityTerminal::Failed(error) => {
                            AudioResult::Failed(format!("audio output: {error:?}"))
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
    fn take_result(&mut self, id: AudioId) -> Option<AudioResult> {
        if !self
            .entries
            .get(&id)
            .is_some_and(|e| matches!(e.phase, Phase::Done(_)))
        {
            return None;
        }
        let Phase::Done(result) = self.entries.remove(&id).unwrap().phase else {
            unreachable!()
        };
        Some(result)
    }
    fn poll_after(&self) -> Option<Duration> {
        (!self.entries.is_empty()
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
        if let Some(output) = &mut self.output {
            let _ = output.suspend();
        }
        self.output_failure();
    }
    fn resume(&mut self) {
        self.suspended = false;
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
        self.closed
            && (!self.worker.is_idle()
                || self.assets.pending_io()
                || self.owner.as_ref().is_some_and(|owner| !owner.is_retired())
                || self.reclaim.as_ref().is_some_and(|reclaim| {
                    let status = reclaim.status();
                    status.live_generations != 0 || status.outstanding_work != 0
                }))
    }
}
impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests;
