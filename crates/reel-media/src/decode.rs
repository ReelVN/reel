//! Codec-independent source timelines and borrowed PCM for background preparation.
use reel_audio::Frame;
use reel_core::budget::BudgetPool;
use std::sync::{Arc, atomic::AtomicBool};

pub use reel_audio::sound::streaming::AudioRead as MediaRead;

#[cfg(feature = "decode")]
mod software;

/// The audible source timeline after codec priming and trailing padding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioInfo {
    /// Source frames per second; codec rates and output device rates are separate.
    pub sample_rate: u32,
    /// Audible source frames, or unknown until EOF.
    pub frames: Option<u64>,
}

/// A worker-owned decoder whose PCM storage remains in its adapter.
pub trait AudioDecoder: Send {
    /// The audible source rate and length.
    fn info(&self) -> AudioInfo;
    /// Seek in audible source frames, returning the actual (possibly earlier) frame.
    fn seek(&mut self, frame: u64) -> Result<u64, String>;
    /// Decode the next nonempty chunk. False means EOF, including delayed output.
    fn decode(&mut self, cancelled: &AtomicBool) -> Result<bool, String>;
    /// Borrow the last decoded chunk until the next mutable decoder operation.
    fn pcm(&self) -> &[Frame];
}

/// Opens decoders on preparation workers using the caller's actual CPU ledger.
/// Implementations reserve owned buffers as they grow, rather than reserving a
/// configured maximum. The stop flag interrupts codec work after admission.
pub trait AudioDecoderFactory: std::fmt::Debug + Send + Sync {
    /// Open one owned encoded source; no IO or codec work runs on the audio callback.
    fn open(
        &self,
        source: Box<dyn MediaRead>,
        length: u64,
        budget: BudgetPool,
        stopped: Arc<AtomicBool>,
    ) -> Result<Box<dyn AudioDecoder>, String>;
}

/// Opens software codecs with the `decode` feature using one borrowed PCM packet.
#[derive(Debug, Default)]
pub struct SoftwareAudioDecoderFactory;
impl AudioDecoderFactory for SoftwareAudioDecoderFactory {
    fn open(
        &self,
        source: Box<dyn MediaRead>,
        length: u64,
        budget: BudgetPool,
        stopped: Arc<AtomicBool>,
    ) -> Result<Box<dyn AudioDecoder>, String> {
        #[cfg(feature = "decode")]
        {
            software::open(source, length, budget, stopped)
        }
        #[cfg(not(feature = "decode"))]
        {
            let _ = (source, length, budget, stopped);
            Err("software audio decoding requires the decode feature".into())
        }
    }
}
