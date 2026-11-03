use reel_core::{clock::MonoTime, result::Rejected};

pub type PcmFill = Box<dyn FnMut(&mut [f32]) + Send + 'static>;

/// Only known, ordered layouts. An unidentified native layout is never guessed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelLayout {
    Mono,
    Stereo,
    Surround71,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Mono,
    FrontLeft,
    FrontRight,
    Center,
    Lfe,
    BackLeft,
    BackRight,
    SideLeft,
    SideRight,
}
impl ChannelLayout {
    pub fn channels(self) -> &'static [Channel] {
        use Channel::*;
        match self {
            Self::Mono => &[Mono],
            Self::Stereo => &[FrontLeft, FrontRight],
            Self::Surround71 => &[
                FrontLeft, FrontRight, Center, Lfe, BackLeft, BackRight, SideLeft, SideRight,
            ],
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelMap {
    MonoAverage,
    Left,
    Right,
    Silence,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    F32,
    F64,
    I16,
    U16,
    I32,
}

/// A valid combination; rates are a range *within this format/layout pair*.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioConfigRange {
    pub min_sample_rate_hz: u32,
    pub max_sample_rate_hz: u32,
    pub sample_format: SampleFormat,
    pub layout: ChannelLayout,
}
/// Presentation capability is independent of generating PCM or reporting latency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioClockCapability {
    /// Native submission/presentation is not confirmed. No MediaPTS anchor.
    Unavailable,
    /// Driver guarantees previous blocks were submitted and provides a latency
    /// estimate. Used by the controlled offline sink; a future native adapter
    /// must prove those guarantees before opting in. Never implies Measured.
    Estimated,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioCaps {
    pub clock: AudioClockCapability,
    pub configurations: Vec<AudioConfigRange>,
    pub min_pull_frames: u32,
    pub max_pull_frames: u32,
    /// Native pause may still fail; failure obeys the ownership contract.
    pub suspend: bool,
}
pub const MAX_PULL_FRAMES: u32 = 4096;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioRequest {
    pub preferred_sample_rate_hz: u32,
    pub allowed_sample_formats: Vec<SampleFormat>,
    pub allowed_layouts: Vec<ChannelLayout>,
    pub max_pull_frames: u32,
}
impl Default for AudioRequest {
    fn default() -> Self {
        Self {
            preferred_sample_rate_hz: 48_000,
            allowed_sample_formats: vec![
                SampleFormat::F32,
                SampleFormat::I16,
                SampleFormat::U16,
                SampleFormat::I32,
                SampleFormat::F64,
            ],
            allowed_layouts: vec![ChannelLayout::Stereo, ChannelLayout::Mono],
            max_pull_frames: 512,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate_hz: u32,
    pub mixer_channels: u16,
    pub device_sample_format: SampleFormat,
    pub device_channel_layout: ChannelLayout,
    pub max_pull_frames: u32,
    /// Ordered exactly like device_channel_layout.channels().
    pub channel_map: Vec<ChannelMap>,
}
impl AudioFormat {
    pub fn new(
        sample_rate_hz: u32,
        sample_format: SampleFormat,
        layout: ChannelLayout,
        max_pull_frames: u32,
    ) -> Result<Self, AudioError> {
        if sample_rate_hz == 0 || !(1..=MAX_PULL_FRAMES).contains(&max_pull_frames) {
            return Err(AudioError::new(
                AudioErrorKind::InvalidRequest,
                "invalid rate or pull bound",
            ));
        }
        let channel_map = layout
            .channels()
            .iter()
            .map(|channel| match channel {
                Channel::Mono => ChannelMap::MonoAverage,
                Channel::FrontLeft => ChannelMap::Left,
                Channel::FrontRight => ChannelMap::Right,
                _ => ChannelMap::Silence,
            })
            .collect();
        Ok(Self {
            sample_rate_hz,
            mixer_channels: 2,
            device_sample_format: sample_format,
            device_channel_layout: layout,
            max_pull_frames,
            channel_map,
        })
    }
    pub(crate) fn validate(&self) -> Result<(), AudioError> {
        let expected = Self::new(
            self.sample_rate_hz,
            self.device_sample_format,
            self.device_channel_layout,
            self.max_pull_frames,
        )?;
        if *self != expected {
            return Err(AudioError::new(
                AudioErrorKind::InvalidRequest,
                "noncanonical mixer channels or channel map",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioDeviceConfig {
    pub sample_rate_hz: u32,
    pub sample_format: SampleFormat,
    pub layout: ChannelLayout,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutputEpoch(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioState {
    /// Fill is disabled. Native retirement may still be pending; query close_complete.
    Stopped,
    /// Fill was accepted and is enabled; native startup may still be pending.
    Running,
    /// Fill is closed and retained; native queued PCM may still be draining.
    Suspended,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioErrorKind {
    DecodeFailure,
    Busy,
    Unsupported,
    InvalidRequest,
    Unavailable,
    DeviceFailure,
    ReconfigureRequired,
    DeviceContract,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioError {
    pub kind: AudioErrorKind,
    pub message: String,
}
impl AudioError {
    pub fn new(kind: AudioErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}
impl std::error::Error for AudioError {}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioStatus {
    pub state: AudioState,
    pub output_epoch: OutputEpoch,
    /// Device xruns and explicit missing PCM frames are distinct counters.
    pub underrun_count: u64,
    pub silent_frames: u64,
    /// Scheduling promotion is advisory; denial never discards the mixer.
    pub realtime_denied_count: u64,
    pub latest_device_error: Option<AudioError>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockPrecision {
    Measured,
    Estimated,
    /// Numeric presentation fields are invalid and MUST NOT anchor MediaPTS.
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioClockObservation {
    pub output_epoch: OutputEpoch,
    /// Software-generated device frames (including silence), for diagnostics.
    /// Does not prove native write acceptance or presentation; epoch-scoped.
    pub generated_frames: u64,
    /// Frames handed to a driver with submission guarantees. Zero is an invalid
    /// sentinel under Unavailable precision, NOT proof that zero frames played.
    pub submitted_frames: u64,
    /// Usable only when precision != Unavailable and output_epoch is current.
    /// Zero under Unavailable is an invalid sentinel, NEVER a MediaPTS anchor.
    /// Prefer presented_frames() so missing capability is represented as None.
    pub estimated_presented_frames: u64,
    pub monotonic_time: MonoTime,
    pub sample_rate_hz: u32,
    pub latency_frames: Option<u32>,
    /// None means the backend cannot bound its timing error.
    pub uncertainty_frames: Option<u32>,
    pub precision: ClockPrecision,
}
impl AudioClockObservation {
    /// None under Unavailable: the numeric field then has no submission validity.
    pub fn submitted_frames(&self) -> Option<u64> {
        (self.precision != ClockPrecision::Unavailable).then_some(self.submitted_frames)
    }
    pub fn presented_frames(&self) -> Option<u64> {
        (self.precision != ClockPrecision::Unavailable).then_some(self.estimated_presented_frames)
    }
    pub(crate) fn unavailable(epoch: OutputEpoch, rate: u32) -> Self {
        Self {
            output_epoch: epoch,
            generated_frames: 0,
            submitted_frames: 0,
            estimated_presented_frames: 0,
            monotonic_time: MonoTime(0),
            sample_rate_hz: rate,
            latency_frames: None,
            uncertainty_frames: None,
            precision: ClockPrecision::Unavailable,
        }
    }
}
/// Driver timing in the host MonoTime domain. Latency alone proves no submission
/// or presentation. Capability is declared separately; CPAL values are diagnostic.
/// A future driver with confirmed submission can opt into the existing estimate
/// path only if its timestamps and preceding blocks satisfy that guarantee.
#[derive(Clone, Copy, Debug)]
pub struct CallbackTiming {
    pub now: MonoTime,
    pub latency_frames: Option<u32>,
    pub uncertainty_frames: Option<u32>,
}

pub trait AudioOutput {
    fn capabilities(&self) -> AudioCaps;
    /// Negotiate a stopped output. Backends may prepare native stream resources;
    /// PCM supply begins only after start accepts the fill.
    fn configure(&mut self, request: AudioRequest) -> Result<AudioFormat, AudioError>;
    /// Rejects with the identical, uncalled fill. After acceptance only status reports faults.
    fn start(&mut self, fill: PcmFill) -> Result<(), Rejected<PcmFill>>;
    /// Closes entry and waits for the in-flight fill call; retains the same fill.
    /// Native queued PCM may drain asynchronously while the software state is
    /// Suspended. Native handles/buffers remain owned through actual completion.
    fn suspend(&mut self) -> Result<(), AudioError>;
    fn resume(&mut self) -> Result<(), AudioError>;
    fn status(&self) -> AudioStatus;
    fn clock_observation(&self) -> AudioClockObservation;
    /// True after stop when the actual native output handle and buffers have
    /// closed. Pending close retains native ownership; software Stopped alone
    /// does not prove release. Default synchronous drivers complete in stop.
    /// Control-side polling never waits for native drain or unfinished workers.
    fn close_complete(&self) -> bool {
        self.status().state == AudioState::Stopped
    }
    /// Even an error return guarantees no further fill invocation. Native release
    /// may remain pending: retain the output until close_complete, or transfer it
    /// to the platform's bounded retirement owner. Never call inside fill.
    fn stop(&mut self) -> Result<(), AudioError>;
}

pub(crate) fn negotiate(
    caps: &AudioCaps,
    request: &AudioRequest,
) -> Result<AudioFormat, AudioError> {
    if request.preferred_sample_rate_hz == 0
        || !(caps.min_pull_frames..=caps.max_pull_frames).contains(&request.max_pull_frames)
    {
        return Err(AudioError::new(
            AudioErrorKind::InvalidRequest,
            "invalid rate or pull bound",
        ));
    }
    let mut candidates = caps
        .configurations
        .iter()
        .filter(|c| {
            request.allowed_layouts.contains(&c.layout)
                && request.allowed_sample_formats.contains(&c.sample_format)
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|c| {
        let rate = request
            .preferred_sample_rate_hz
            .clamp(c.min_sample_rate_hz, c.max_sample_rate_hz);
        (
            rate.abs_diff(request.preferred_sample_rate_hz),
            request.allowed_layouts.iter().position(|l| *l == c.layout),
            request
                .allowed_sample_formats
                .iter()
                .position(|f| *f == c.sample_format),
        )
    });
    let c = candidates.first().ok_or_else(|| {
        AudioError::new(
            AudioErrorKind::Unsupported,
            "no known device format/layout matches request",
        )
    })?;
    AudioFormat::new(
        request
            .preferred_sample_rate_hz
            .clamp(c.min_sample_rate_hz, c.max_sample_rate_hz),
        c.sample_format,
        c.layout,
        request.max_pull_frames,
    )
}
