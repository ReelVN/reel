//! Codec-independent source timelines and borrowed PCM for background preparation.
use reel_audio::Frame;
use reel_core::budget::BudgetPool;
use std::{
    io::Seek,
    sync::{Arc, atomic::AtomicBool},
};

pub use reel_audio::sound::streaming::AudioRead as MediaRead;

mod at9;
#[cfg(all(feature = "decode", not(target_os = "vita")))]
mod software;

/// The audible source timeline after codec priming and trailing padding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioInfo {
    /// Source frames per second; codec rates and output device rates are separate.
    pub sample_rate: u32,
    /// Audible source frames, or unknown until EOF.
    pub frames: Option<u64>,
}

/// Borrowed decoded samples, valid until the decoder's next mutable operation.
/// Mono PCM is duplicated only when consumed.
#[derive(Clone, Copy, Debug)]
pub enum AudioPcm<'a> {
    /// Existing stereo floating-point output, without a second packet allocation.
    Frames(&'a [Frame]),
    /// Native signed 16-bit mono output.
    MonoI16(&'a [i16]),
    /// Native signed 16-bit interleaved stereo output.
    StereoI16(&'a [[i16; 2]]),
}
impl AudioPcm<'_> {
    /// Number of source frames in this chunk.
    pub fn len(self) -> usize {
        match self {
            Self::Frames(pcm) => pcm.len(),
            Self::MonoI16(pcm) => pcm.len(),
            Self::StereoI16(pcm) => pcm.len(),
        }
    }
    /// Whether the chunk contains no source frames.
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
    /// Convert one source frame directly into the common mixer representation.
    pub fn frame(self, index: usize) -> Frame {
        match self {
            Self::Frames(pcm) => pcm[index],
            Self::MonoI16(pcm) => Frame::from_mono(f32::from(pcm[index]) / 32768.),
            Self::StereoI16(pcm) => Frame::new(
                f32::from(pcm[index][0]) / 32768.,
                f32::from(pcm[index][1]) / 32768.,
            ),
        }
    }
}

/// A worker-owned decoder. Native handles and PCM storage remain in its adapter.
pub trait AudioDecoder: Send {
    /// The audible source rate and length.
    fn info(&self) -> AudioInfo;
    /// Seek in audible source frames, returning the actual (possibly earlier) frame.
    fn seek(&mut self, frame: u64) -> Result<u64, String>;
    /// Decode the next nonempty chunk. False means EOF, including delayed output.
    fn decode(&mut self, cancelled: &AtomicBool) -> Result<bool, String>;
    /// Borrow the chunk produced by the last successful decode.
    fn pcm(&self) -> AudioPcm<'_>;
}

/// Opens decoders on preparation workers using the caller's actual CPU ledger.
/// Implementations reserve owned buffers as they grow, rather than reserving a
/// configured maximum. The stop flag also interrupts native work after admission.
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

/// Selects the configured software or native codec at the media module boundary.
#[derive(Debug, Default)]
pub struct PlatformAudioDecoderFactory;
impl AudioDecoderFactory for PlatformAudioDecoderFactory {
    fn open(
        &self,
        mut source: Box<dyn MediaRead>,
        length: u64,
        budget: BudgetPool,
        stopped: Arc<AtomicBool>,
    ) -> Result<Box<dyn AudioDecoder>, String> {
        let at9 = at9::inspect(&mut *source, length)?;
        source.rewind().map_err(|e| e.to_string())?;
        if let Some(header) = at9 {
            return at9::open(source, header, budget, stopped);
        }
        #[cfg(all(feature = "decode", not(target_os = "vita")))]
        {
            software::open(source, length, budget, stopped)
        }
        #[cfg(not(all(feature = "decode", not(target_os = "vita"))))]
        {
            let _ = (source, budget, stopped);
            Err("no audio decoder is configured for this resource".into())
        }
    }
}
