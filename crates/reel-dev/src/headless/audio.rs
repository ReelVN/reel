//! Production media with a bounded offline audio callback buffer.
use reel_core::{clock::MonoTime, result::Rejected, wake::WakeSignal};
use reel_media::{
    AudioCaps, AudioClockObservation, AudioDeviceConfig, AudioError, AudioFormat, AudioOutput,
    AudioRequest, AudioStatus, CallbackTiming, ChannelLayout, OfflineOutput, PcmFill, SampleFormat,
};
use reel_player::{
    assets::{AssetOptions, Assets},
    media::{MediaOptions, MediaPlayer},
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    path::PathBuf,
    rc::{Rc, Weak},
    time::Duration,
};

const RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const CHUNK_FRAMES: usize = 512;

struct Output(Rc<RefCell<OfflineOutput>>);

impl AudioOutput for Output {
    fn capabilities(&self) -> AudioCaps {
        self.0.borrow().capabilities()
    }
    fn configure(&mut self, request: AudioRequest) -> Result<AudioFormat, AudioError> {
        self.0.borrow_mut().configure(request)
    }
    fn start(&mut self, fill: PcmFill) -> Result<(), Rejected<PcmFill>> {
        self.0.borrow_mut().start(fill)
    }
    fn suspend(&mut self) -> Result<(), AudioError> {
        self.0.borrow_mut().suspend()
    }
    fn resume(&mut self) -> Result<(), AudioError> {
        self.0.borrow_mut().resume()
    }
    fn status(&self) -> AudioStatus {
        self.0.borrow().status()
    }
    fn clock_observation(&self) -> AudioClockObservation {
        self.0.borrow().clock_observation()
    }
    fn stop(&mut self) -> Result<(), AudioError> {
        self.0.borrow_mut().stop()
    }
}

#[derive(Default)]
pub(super) struct AudioClock {
    outputs: Rc<RefCell<Vec<Weak<RefCell<OfflineOutput>>>>>,
    elapsed_frames: u64,
    active: Rc<Cell<usize>>,
    pub frames: u64,
    pub peak: f32,
}

impl AudioClock {
    pub fn factory(
        &self,
        root: PathBuf,
        wake: WakeSignal,
    ) -> impl Fn() -> Box<dyn reel_engine::media::MediaBackend> + 'static {
        let outputs = self.outputs.clone();
        let active = self.active.clone();
        move || {
            let outputs = outputs.clone();
            Box::new(TrackedMedia {
                player: MediaPlayer::new(
                    Assets::directory(AssetOptions {
                        root: root.clone(),
                        ..Default::default()
                    }),
                    MediaOptions::default(),
                    wake.clone(),
                    move |wake| {
                        let output = Rc::new(RefCell::new(
                            OfflineOutput::new(
                                AudioDeviceConfig {
                                    sample_rate_hz: RATE,
                                    sample_format: SampleFormat::F32,
                                    layout: ChannelLayout::Stereo,
                                },
                                wake,
                            )
                            .map_err(|error| error.to_string())?,
                        ));
                        let mut outputs = outputs.borrow_mut();
                        outputs.retain(|output| output.strong_count() != 0);
                        outputs.push(Rc::downgrade(&output));
                        Ok(Box::new(Output(output)))
                    },
                ),
                active: active.clone(),
                ids: BTreeSet::new(),
            })
        }
    }

    pub fn advance(&mut self, elapsed: Duration) -> Result<(), String> {
        let target = (elapsed.as_nanos() * u128::from(RATE) / 1_000_000_000) as u64;
        let mut outputs = self.outputs.borrow_mut();
        outputs.retain(|output| output.strong_count() != 0);
        if outputs.is_empty() || self.active.get() == 0 {
            self.elapsed_frames = target;
            return Ok(());
        }
        let mut pcm = [0.0_f32; CHUNK_FRAMES * CHANNELS];
        while self.elapsed_frames < target {
            let frames = (target - self.elapsed_frames).min(CHUNK_FRAMES as u64) as usize;
            let now = MonoTime(self.elapsed_frames * 1_000_000_000 / u64::from(RATE));
            for output in outputs.iter().filter_map(Weak::upgrade) {
                let pcm = &mut pcm[..frames * CHANNELS];
                output.borrow_mut().render(
                    pcm,
                    CallbackTiming {
                        now,
                        latency_frames: None,
                        uncertainty_frames: None,
                    },
                );
                self.frames += frames as u64;
                for sample in pcm.iter() {
                    if !sample.is_finite() {
                        return Err("non-finite mixed PCM".into());
                    }
                    self.peak = self.peak.max(sample.abs());
                }
            }
            self.elapsed_frames += frames as u64;
        }
        Ok(())
    }
}

// Track transport ownership at the existing backend boundary. An initialized
// mixer output with no submitted media has no content clock to consume.
struct TrackedMedia {
    player: MediaPlayer,
    active: Rc<Cell<usize>>,
    ids: BTreeSet<reel_engine::media::MediaId>,
}

impl TrackedMedia {
    fn retain(&mut self, id: reel_engine::media::MediaId) {
        if self.ids.insert(id) {
            self.active.set(self.active.get() + 1);
        }
    }
    fn release(&mut self, id: reel_engine::media::MediaId) {
        if self.ids.remove(&id) {
            self.active.set(self.active.get() - 1);
        }
    }
}

impl Drop for TrackedMedia {
    fn drop(&mut self) {
        self.active.set(self.active.get() - self.ids.len());
    }
}

impl reel_engine::media::MediaBackend for TrackedMedia {
    fn play_audio(
        &mut self,
        request: reel_engine::audio::AudioRequest,
        paused: bool,
    ) -> Result<reel_engine::media::MediaId, String> {
        let id = self.player.play_audio(request, paused)?;
        self.retain(id);
        Ok(id)
    }
    fn commit_audio(&mut self, id: reel_engine::media::MediaId) {
        self.player.commit_audio(id);
    }
    fn queue_audio(
        &mut self,
        id: reel_engine::media::MediaId,
        item: reel_engine::audio::AudioQueueItem,
    ) -> Result<(), String> {
        self.player.queue_audio(id, item)
    }
    fn play_video(
        &mut self,
        request: reel_engine::video::VideoRequest,
        paused: bool,
    ) -> Result<reel_engine::media::MediaId, String> {
        let id = self.player.play_video(request, paused)?;
        self.retain(id);
        Ok(id)
    }
    fn restore_video(
        &mut self,
        state: reel_engine::video::VideoPlaybackState,
    ) -> Result<reel_engine::media::MediaId, String> {
        let id = self.player.restore_video(state)?;
        self.retain(id);
        Ok(id)
    }
    fn snapshot(
        &self,
        id: reel_engine::media::MediaId,
    ) -> Result<reel_engine::media::MediaPlayback, String> {
        self.player.snapshot(id)
    }
    fn position(&self, id: reel_engine::media::MediaId) -> Option<Duration> {
        self.player.position(id)
    }
    fn command(
        &mut self,
        id: reel_engine::media::MediaId,
        command: reel_engine::media::MediaCommand,
    ) -> Result<(), String> {
        self.player.command(id, command)
    }
    fn take_video_frame(
        &mut self,
        id: reel_engine::media::MediaId,
    ) -> Option<reel_ui::resources::ResourceLease> {
        self.player.take_video_frame(id)
    }
    fn group_volume(
        &mut self,
        group: Option<reel_engine::audio::AudioGroup>,
        value: f64,
        duration: Duration,
    ) -> Result<(), String> {
        self.player.group_volume(group, value, duration)
    }
    fn cancel(&mut self, id: reel_engine::media::MediaId) {
        self.player.cancel(id);
        self.release(id);
    }
    fn maintain(&mut self, elapsed: Duration) {
        self.player.maintain(elapsed);
    }
    fn take_result(
        &mut self,
        id: reel_engine::media::MediaId,
    ) -> Option<reel_engine::media::MediaCompletion> {
        let result = self.player.take_result(id);
        if result.is_some() {
            self.release(id);
        }
        result
    }
    fn poll_after(&self) -> Option<Duration> {
        self.player.poll_after()
    }
    fn suspend(&mut self) {
        self.player.suspend();
    }
    fn resume(&mut self) {
        self.player.resume();
    }
    fn close(&mut self) {
        self.player.close();
        self.active.set(self.active.get() - self.ids.len());
        self.ids.clear();
    }
    fn cleanup_pending(&self) -> bool {
        self.player.cleanup_pending()
    }
}
