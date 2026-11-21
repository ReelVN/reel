//! Worker-owned video decoding and bounded playback using portable frames.
#[cfg(any(test, all(feature = "vita", target_os = "vita")))]
mod native_budget;
#[cfg(any(test, all(feature = "vita", target_os = "vita")))]
mod pcm_pool;
mod playback;
pub use playback::*;
#[cfg(feature = "video-inspect")]
mod mp4_profile;
#[cfg(feature = "video-inspect")]
pub use mp4_profile::{VITA_VIDEO_DIMENSIONS, validate_desktop_mp4, validate_vita_mp4};
#[cfg(any(feature = "video-inspect", all(feature = "vita", target_os = "vita")))]
mod h264;
#[cfg(feature = "video-software")]
mod software;
pub use crate::decode::MediaRead;
use reel_core::budget::{BudgetPool, BudgetTicket};
#[cfg(feature = "video-software")]
pub use software::{MovieDecoder, SoftwareVideoDecoderFactory};
#[cfg(all(feature = "vita", target_os = "vita"))]
mod vita;
use std::{
    sync::{Arc, atomic::AtomicBool},
    task::Poll,
};
#[cfg(all(feature = "vita", target_os = "vita"))]
pub use vita::VitaVideoDecoderFactory;

pub use reel_core::image::{Yuv420Layout, YuvColor, YuvMatrix, YuvTransfer};

fn budget_failure(
    pool: &BudgetPool,
    source: &str,
    bytes: u64,
    mut error: reel_core::result::RejectReason,
) -> reel_core::result::RejectReason {
    let snapshot = pool.snapshot();
    error.message = format!(
        "{}: {} (required={bytes}, pool_used_snapshot={}, pool_limit={}, pool_tickets_snapshot={}, pool_ticket_limit={}, group_snapshot={:?})",
        source,
        error.message,
        snapshot.usage.bytes,
        snapshot.limits.max_bytes,
        snapshot.usage.tickets,
        snapshot.limits.max_requests,
        snapshot.group
    );
    error
}
/// Owned top-down 8-bit YUV 4:2:0; no decoder-owned pointers escape.
#[derive(Debug)]
pub struct VideoFrame {
    /// Optional immutable owned storage. When present, it owns its budget and
    /// supplies the planes; the vector slots are empty. Native handles remain
    /// behind the portable YUV storage interface.
    pub storage: Option<Arc<dyn reel_core::image::Yuv420Storage>>,
    pub pts: f64,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    /// Planar Y/U/V or two-plane NV12; unused plane storage is empty.
    pub layout: Yuv420Layout,
    /// Producer byte strides, with a zero third entry for NV12.
    pub row_strides: [u32; 3],
    pub planes: [Vec<u8>; 3],
    pub color: YuvColor,
}
/// A decoded PCM packet and the reservation for its vector capacity.
#[derive(Debug)]
pub struct AudioFrames {
    pub pts: f64,
    pub sample_rate: u32,
    pub frames: Vec<[f32; 2]>,
    /// Reservation for the complete capacity of `frames`, transferred with it.
    pub storage: BudgetTicket,
}
#[derive(Debug)]
pub enum DecodedPacket {
    Video(VideoFrame),
    Audio(AudioFrames),
}
/// Tracks whose next owned packet fits the caller's bounded publication queues.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeDemand {
    pub video: bool,
    pub audio: bool,
}
impl DecodeDemand {
    /// Request either track, retaining the decoder's packet ordering policy.
    pub const ALL: Self = Self {
        video: true,
        audio: true,
    };
}
#[derive(Clone, Debug)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub duration: Option<f64>,
    pub audio_sample_rate: Option<u32>,
}

/// A decoder created, used and dropped on one preparation worker. Implementations
/// may keep thread-affine native state; decoded packets own their CPU storage.
/// Their source timeline drives both picture presentation and PCM consumption.
pub trait VideoDecoder {
    /// Source picture dimensions, duration and optional PCM sample rate.
    fn info(&self) -> &VideoInfo;
    /// Picture vector capacity the playback worker must reserve before polling.
    /// Adapters whose returned immutable storage carries its own complete ticket
    /// return zero; their allocations remain charged by the adapter's pool.
    fn video_output_bytes(&self) -> u64 {
        let info = self.info();
        u64::from(info.width) * u64::from(info.height)
            + 2 * u64::from(info.width.div_ceil(2)) * u64::from(info.height.div_ceil(2))
    }
    /// Whether no further audio packets remain, including delayed codec output.
    fn audio_finished(&self) -> bool;
    /// Limit decoded output to an exclusive source-time endpoint in seconds.
    fn set_end(&mut self, seconds: f64);
    /// Pause native transport while bounded publication queues are full.
    /// Software decoders advance only on demand and retain the default no-op.
    fn set_paused(&mut self, _paused: bool) -> Result<(), String> {
        Ok(())
    }
    /// Prepare the picture covering a saved position without admitting audio.
    /// Native adapters may internally drain codec output. Updates info to
    /// report no audio sample rate, so restoration creates no PCM stream.
    fn prepare_poster(&mut self, seconds: f64) -> Result<(), String>;
    /// Poll only the requested tracks, allowing one track to advance while the
    /// other publication queue is full. Pending means the requested tracks have
    /// no packet ready; Ready(None) means all tracks have ended. The worker owns
    /// readiness waits. Codec work observes the factory's shared stop flag.
    fn poll_packet(&mut self, demand: DecodeDemand) -> Result<Poll<Option<DecodedPacket>>, String>;
    /// Return a PCM packet after its initialized samples have entered the bounded
    /// sample queue. Called only on the decode worker; the packet's ticket
    /// charges its vector capacity until it is reused or released.
    fn recycle_audio(&mut self, _packet: AudioFrames) {}
    /// Seek on the source timeline, retaining picture preroll and trimming PCM
    /// before the requested position. No previous packet storage is mutated.
    fn seek(&mut self, seconds: f64) -> Result<(), String>;
}

/// Opens a decoder on the playback worker. The worker charges returned vector
/// picture storage and PCM queues; PCM packets, optional owned picture storage
/// and adapter buffers carry their own reservations from
/// the supplied pool. The shared stop flag covers initialization, decode and waits.
pub trait VideoDecoderFactory: std::fmt::Debug + Send + Sync {
    /// Own the seekable resource stream until the decoder has fenced its native
    /// callbacks and released it. This method never runs on the audio callback.
    fn open(
        &self,
        source: Box<dyn MediaRead>,
        budget: BudgetPool,
        stopped: Arc<AtomicBool>,
    ) -> Result<Box<dyn VideoDecoder>, String>;
}
