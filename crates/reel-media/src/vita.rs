//! Native AudioOut through the existing ownership gate and callback telemetry.
use crate::{
    output::{Callback, DrivenOutput, Driver, output_methods},
    *,
};
use pvr_psp2_sys::platform::audio;
use reel_core::{clock::MonoTime, result::Rejected, wake::WakeSignal};

struct VitaDriver {
    retired: Option<audio::Stream>,
}
fn error(error: pvr_psp2_sys::platform::Error) -> AudioError {
    AudioError::new(
        if error.operation == "AudioOut configuration changed" {
            AudioErrorKind::ReconfigureRequired
        } else {
            AudioErrorKind::DeviceFailure
        },
        error.to_string(),
    )
}
impl VitaDriver {
    fn release_retired(&mut self) -> Result<(), AudioError> {
        if let Some(stream) = &mut self.retired {
            stream.stop().map_err(error)?;
        }
        self.retired = None;
        Ok(())
    }
}
impl Driver for VitaDriver {
    type Stream = audio::Stream;
    fn capabilities(&self) -> AudioCaps {
        AudioCaps {
            clock: AudioClockCapability::Unavailable,
            configurations: vec![AudioConfigRange {
                min_sample_rate_hz: audio::SAMPLE_RATE,
                max_sample_rate_hz: audio::SAMPLE_RATE,
                sample_format: SampleFormat::I16,
                layout: ChannelLayout::Stereo,
            }],
            min_pull_frames: audio::BLOCK_FRAMES as u32,
            max_pull_frames: audio::BLOCK_FRAMES as u32,
            suspend: true,
        }
    }
    fn configure(&mut self, request: &AudioRequest) -> Result<AudioFormat, AudioError> {
        self.release_retired()?;
        negotiate(&self.capabilities(), request)
    }
    fn build(
        &mut self,
        _: &AudioFormat,
        mut callback: Callback,
    ) -> Result<Self::Stream, AudioError> {
        self.release_retired()?;
        let shared = callback.shared.clone();
        audio::Stream::new(
            move |pcm, micros| {
                callback.render(
                    pcm,
                    CallbackTiming {
                        now: MonoTime(micros.saturating_mul(1000)),
                        latency_frames: None,
                        uncertainty_frames: None,
                    },
                )
            },
            move |fault| {
                if fault.operation == "audio realtime scheduling denied" {
                    shared.realtime_denied();
                } else if fault.operation == "AudioOut configuration changed" {
                    shared.fail(AudioErrorKind::ReconfigureRequired);
                } else {
                    shared.fail(AudioErrorKind::DeviceFailure);
                }
            },
        )
        .map_err(error)
    }
    fn play(&mut self, stream: &Self::Stream) -> Result<(), AudioError> {
        stream.play().map_err(error)
    }
    fn pause(&mut self, stream: &Self::Stream) -> Result<(), AudioError> {
        stream.pause().map_err(error)
    }
    fn close(&mut self, mut stream: Self::Stream) -> Result<(), AudioError> {
        if let Err(fault) = stream.stop() {
            self.retired = Some(stream);
            return Err(error(fault));
        }
        Ok(())
    }
    fn verify(&self, format: &AudioFormat) -> Result<(), AudioError> {
        if format.sample_rate_hz != audio::SAMPLE_RATE
            || format.device_sample_format != SampleFormat::I16
            || format.device_channel_layout != ChannelLayout::Stereo
            || format.max_pull_frames != audio::BLOCK_FRAMES as u32
        {
            return Err(AudioError::new(
                AudioErrorKind::ReconfigureRequired,
                "Vita AudioOut format changed",
            ));
        }
        Ok(())
    }
}
/// Owned Vita stereo output. The native presentation clock is unavailable;
/// playback follows the common mixer's consumed-content position.
pub struct VitaOutput {
    inner: DrivenOutput<VitaDriver>,
}
impl VitaOutput {
    /// Create the control service; `start` opens the fixed 48 kHz stereo port.
    pub fn new(wake: WakeSignal) -> Result<Self, AudioError> {
        Ok(Self {
            inner: DrivenOutput::new(VitaDriver { retired: None }, wake)?,
        })
    }
}
impl AudioOutput for VitaOutput {
    output_methods!();
}
