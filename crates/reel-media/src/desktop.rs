use crate::{
    output::{Callback, DrivenOutput, Driver, output_methods},
    *,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use reel_core::{clock::MonoTime, result::Rejected, wake::WakeSignal};
use std::time::{Duration, Instant};

struct CpalDriver {
    device: cpal::Device,
    device_id: cpal::DeviceId,
    capabilities: AudioCaps,
    default_signature: (u16, u32, cpal::SampleFormat),
    mono_origin: MonoTime,
    instant_origin: Instant,
}
fn error(e: cpal::Error) -> AudioError {
    use cpal::ErrorKind as C;
    let kind = match e.kind() {
        C::DeviceBusy => AudioErrorKind::Busy,
        C::DeviceChanged | C::StreamInvalidated => AudioErrorKind::ReconfigureRequired,
        C::UnsupportedConfig | C::UnsupportedOperation => AudioErrorKind::Unsupported,
        C::InvalidInput => AudioErrorKind::InvalidRequest,
        C::HostUnavailable | C::DeviceNotAvailable => AudioErrorKind::Unavailable,
        _ => AudioErrorKind::DeviceFailure,
    };
    AudioError::new(kind, e.to_string())
}
fn sample_format(f: cpal::SampleFormat) -> Option<SampleFormat> {
    match f {
        cpal::SampleFormat::F32 => Some(SampleFormat::F32),
        cpal::SampleFormat::F64 => Some(SampleFormat::F64),
        cpal::SampleFormat::I16 => Some(SampleFormat::I16),
        cpal::SampleFormat::U16 => Some(SampleFormat::U16),
        cpal::SampleFormat::I32 => Some(SampleFormat::I32),
        _ => None,
    }
}
pub(crate) const CPAL_CLOCK_CAPABILITY: AudioClockCapability = AudioClockCapability::Unavailable;

pub(crate) fn native_error(shared: &crate::output::Shared, kind: cpal::ErrorKind) {
    match kind {
        cpal::ErrorKind::Xrun => shared.native_xrun(),
        cpal::ErrorKind::RealtimeDenied => shared.realtime_denied(),
        cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::StreamInvalidated => {
            shared.fail(AudioErrorKind::ReconfigureRequired)
        }
        _ => shared.fail(AudioErrorKind::DeviceFailure),
    }
}

fn capabilities(device: &cpal::Device) -> Result<AudioCaps, AudioError> {
    let mut configurations = Vec::new();
    for config in device.supported_output_configs().map_err(error)? {
        // CPAL exposes a count, not an ordered native speaker map. Only these are known.
        let layout = match config.channels() {
            1 => ChannelLayout::Mono,
            2 => ChannelLayout::Stereo,
            _ => continue,
        };
        let Some(sample_format) = sample_format(config.sample_format()) else {
            continue;
        };
        configurations.push(AudioConfigRange {
            min_sample_rate_hz: config.min_sample_rate(),
            max_sample_rate_hz: config.max_sample_rate(),
            sample_format,
            layout,
        });
    }
    Ok(AudioCaps {
        clock: CPAL_CLOCK_CAPABILITY,
        configurations,
        min_pull_frames: 1,
        max_pull_frames: MAX_PULL_FRAMES,
        suspend: true,
    })
}
fn signature(device: &cpal::Device) -> Result<(u16, u32, cpal::SampleFormat), AudioError> {
    let config = device.default_output_config().map_err(error)?;
    Ok((
        config.channels(),
        config.sample_rate(),
        config.sample_format(),
    ))
}
impl CpalDriver {
    fn build_typed<T: DeviceSample + cpal::SizedSample>(
        &self,
        format: &AudioFormat,
        mut callback: Callback,
    ) -> Result<cpal::Stream, AudioError> {
        let config = cpal::StreamConfig {
            channels: format.device_channel_layout.channels().len() as u16,
            sample_rate: format.sample_rate_hz,
            buffer_size: cpal::BufferSize::Default,
        };
        let shared = callback.shared.clone();
        let instant_origin = self.instant_origin;
        let mono_origin = self.mono_origin;
        let rate = format.sample_rate_hz;
        self.device
            .build_output_stream::<T, _, _>(
                config,
                move |data, info| {
                    // CPAL invokes us before writei and has no write acknowledgement.
                    // EAGAIN with frames_written==0 can silently discard this block.
                    // These timestamps are diagnostic; capability stays Unavailable.
                    let stamp = info.timestamp();
                    let latency_frames = stamp
                        .playback
                        .checked_duration_since(stamp.callback)
                        .and_then(|delay| {
                            u32::try_from(
                                (delay.as_nanos() * u128::from(rate)).div_ceil(1_000_000_000),
                            )
                            .ok()
                        });
                    let now = MonoTime(
                        mono_origin.0.saturating_add(
                            instant_origin
                                .elapsed()
                                .as_nanos()
                                .min(u128::from(u64::MAX)) as u64,
                        ),
                    );
                    callback.render(
                        data,
                        CallbackTiming {
                            now,
                            latency_frames,
                            uncertainty_frames: None,
                        },
                    );
                },
                move |e| native_error(&shared, e.kind()),
                Some(Duration::from_secs(2)),
            )
            .map_err(error)
    }
}
impl Driver for CpalDriver {
    type Stream = cpal::Stream;
    fn capabilities(&self) -> AudioCaps {
        self.capabilities.clone()
    }
    fn configure(&mut self, request: &AudioRequest) -> Result<AudioFormat, AudioError> {
        // Reacquire default device only on the explicit stopped configure path.
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| {
                AudioError::new(AudioErrorKind::Unavailable, "no default output device")
            })?;
        let caps = capabilities(&device)?;
        let format = negotiate(&caps, request)?;
        let device_id = device.id().map_err(error)?;
        let default_signature = signature(&device)?;
        self.device = device;
        self.device_id = device_id;
        self.default_signature = default_signature;
        self.capabilities = caps;
        Ok(format)
    }
    fn build(
        &mut self,
        format: &AudioFormat,
        callback: Callback,
    ) -> Result<cpal::Stream, AudioError> {
        match format.device_sample_format {
            SampleFormat::F32 => self.build_typed::<f32>(format, callback),
            SampleFormat::F64 => self.build_typed::<f64>(format, callback),
            SampleFormat::I16 => self.build_typed::<i16>(format, callback),
            SampleFormat::U16 => self.build_typed::<u16>(format, callback),
            SampleFormat::I32 => self.build_typed::<i32>(format, callback),
        }
    }
    fn play(&mut self, stream: &cpal::Stream) -> Result<(), AudioError> {
        stream.play().map_err(error)
    }
    fn pause(&mut self, stream: &cpal::Stream) -> Result<(), AudioError> {
        stream.pause().map_err(error)
    }
    fn verify(&self, format: &AudioFormat) -> Result<(), AudioError> {
        let current = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| {
                AudioError::new(
                    AudioErrorKind::DeviceFailure,
                    "default output device disappeared",
                )
            })?;
        if current.id().map_err(error)? != self.device_id
            || signature(&current)? != self.default_signature
        {
            return Err(AudioError::new(
                AudioErrorKind::ReconfigureRequired,
                "default device or format changed",
            ));
        }
        let caps = capabilities(&self.device)?;
        if !caps.configurations.iter().any(|c| {
            c.layout == format.device_channel_layout
                && c.sample_format == format.device_sample_format
                && (c.min_sample_rate_hz..=c.max_sample_rate_hz).contains(&format.sample_rate_hz)
        }) {
            return Err(AudioError::new(
                AudioErrorKind::ReconfigureRequired,
                "negotiated format no longer supported",
            ));
        }
        Ok(())
    }
}

/// Default desktop output. No window or render dependencies.
pub struct CpalOutput {
    inner: DrivenOutput<CpalDriver>,
}
impl CpalOutput {
    /// mono_origin must be the host's MonoTime sampled at assembly. Observations
    /// use this anchor + Instant elapsed; CPAL latency remains an unbounded estimate.
    pub fn new(wake: WakeSignal, mono_origin: MonoTime) -> Result<Self, AudioError> {
        let instant_origin = Instant::now();
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| {
                AudioError::new(AudioErrorKind::Unavailable, "no default output device")
            })?;
        let driver = CpalDriver {
            device_id: device.id().map_err(error)?,
            capabilities: capabilities(&device)?,
            default_signature: signature(&device)?,
            device,
            mono_origin,
            instant_origin,
        };
        Ok(Self {
            inner: DrivenOutput::new(driver, wake)?,
        })
    }
    pub fn underrun_signal(&self) -> UnderrunSignal {
        UnderrunSignal::from_shared(self.inner.shared.clone())
    }
}
impl AudioOutput for CpalOutput {
    output_methods!();
}
