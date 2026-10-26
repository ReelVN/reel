//! The production media path with an offline device driven by host elapsed time.
use reel_core::{clock::MonoTime, result::Rejected, wake::WakeSignal};
use reel_media::{
    AudioCaps, AudioClockObservation, AudioDeviceConfig, AudioError, AudioFormat, AudioOutput,
    AudioRequest, AudioStatus, CallbackTiming, ChannelLayout, OfflineOutput, PcmFill, SampleFormat,
};
use reel_player::{
    assets::Assets,
    media::{MediaOptions, MediaPlayer},
};
use std::{
    cell::RefCell,
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

#[derive(Clone, Copy, Debug, Default)]
pub struct AudioStats {
    pub frames: u64,
    pub peak: f32,
}

#[derive(Default)]
pub struct AudioClock {
    outputs: Rc<RefCell<Vec<Weak<RefCell<OfflineOutput>>>>>,
    elapsed_frames: u64,
    stats: AudioStats,
}

impl AudioClock {
    /// Each restored/new session gets its own MediaPlayer and output. Weak
    /// registrations let retiring outputs finish without retaining their PCM.
    pub fn factory(
        &self,
        assets: Assets,
    ) -> impl Fn() -> Box<dyn reel_engine::media::MediaBackend> + 'static {
        let outputs = self.outputs.clone();
        move || {
            let outputs = outputs.clone();
            Box::new(MediaPlayer::new(
                assets.clone(),
                MediaOptions::default(),
                WakeSignal::new(|| {}),
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
            ))
        }
    }

    /// Consume the elapsed interval before applying the next runtime tick's
    /// audio commands. Fractional frames carry through the absolute clock;
    /// repeated VM/UI slices at one time add none. This host estimates timing
    /// at its tick boundaries and makes no physical presentation-clock claim.
    pub fn advance(&mut self, elapsed: Duration) {
        let target = (elapsed.as_nanos() * u128::from(RATE) / 1_000_000_000) as u64;
        assert!(
            target >= self.elapsed_frames,
            "audio host clock moved backwards"
        );
        let mut outputs = self.outputs.borrow_mut();
        outputs.retain(|output| output.strong_count() != 0);
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
                self.stats.frames += frames as u64;
                for sample in pcm.iter() {
                    assert!(sample.is_finite(), "non-finite mixed PCM");
                    self.stats.peak = self.stats.peak.max(sample.abs());
                }
            }
            self.elapsed_frames += frames as u64;
        }
    }

    pub fn stats(&self) -> AudioStats {
        self.stats
    }
}
