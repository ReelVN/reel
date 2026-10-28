//! Native AudioOut through the existing ownership gate and callback telemetry.
use crate::output::port::{NativePort, RetiringStream};
use crate::{
    output::{Callback, DrivenOutput, Driver, output_methods},
    *,
};
use reel_core::{clock::MonoTime, result::Rejected, wake::WakeSignal};
use std::sync::{Arc, OnceLock};
use vita_native::audio;

impl RetiringStream for audio::Stream {
    fn begin_stop(&self) -> Result<(), AudioError> {
        self.begin_stop().map_err(error)
    }
    fn poll_stop(&mut self) -> Result<bool, AudioError> {
        self.poll_stop().map_err(error)
    }
}
struct VitaDriver {
    port: Arc<NativePort<audio::Stream>>,
    generation: Option<u64>,
}
fn port() -> Arc<NativePort<audio::Stream>> {
    static PORT: OnceLock<Arc<NativePort<audio::Stream>>> = OnceLock::new();
    PORT.get_or_init(|| Arc::new(NativePort::new())).clone()
}
fn error(error: vita_native::app::Error) -> AudioError {
    AudioError::new(AudioErrorKind::DeviceFailure, error.to_string())
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
        negotiate(&self.capabilities(), request)
    }
    fn build(
        &mut self,
        _: &AudioFormat,
        mut callback: Callback,
    ) -> Result<Self::Stream, AudioError> {
        let generation = self.port.reserve()?;
        self.generation = Some(generation);
        let shared = callback.shared.clone();
        let stream = audio::Stream::new(
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
                } else {
                    shared.fail(AudioErrorKind::DeviceFailure);
                }
            },
        )
        .map_err(error);
        if stream.is_err() {
            self.port.reject_open(generation);
        }
        stream
    }
    fn play(&mut self, stream: &Self::Stream) -> Result<(), AudioError> {
        stream.play().map_err(error)
    }
    fn pause(&mut self, stream: &Self::Stream) -> Result<(), AudioError> {
        stream.pause().map_err(error)
    }
    fn close(&mut self, stream: Self::Stream) -> Result<(), AudioError> {
        self.port
            .retire(self.generation.expect("reserved native port"), stream)
    }
    fn close_complete(&self) -> bool {
        self.generation.is_none_or(|id| self.port.closed(id))
    }
    fn retry_close(&mut self) -> Result<(), AudioError> {
        self.generation.map_or(Ok(()), |id| self.port.retry(id))
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
    /// Create the control service. Start requests a fixed 48 kHz stereo port;
    /// native opening and retirement remain on its output worker. A pending old
    /// port rejects a new start with Busy and returns the unchanged fill.
    pub fn new(wake: WakeSignal) -> Result<Self, AudioError> {
        Ok(Self {
            inner: DrivenOutput::new(
                VitaDriver {
                    port: port(),
                    generation: None,
                },
                wake,
            )?,
        })
    }
}
impl AudioOutput for VitaOutput {
    output_methods!();
}
