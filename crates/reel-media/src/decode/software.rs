//! The software adapter owns one codec packet; the shared window borrows it.
use super::*;
use reel_audio::sound::streaming::{Decoder, symphonia::SymphoniaDecoder};
use reel_core::budget::BudgetTicket;
use std::sync::atomic::Ordering;

const READER_BYTES: u64 = 64 << 10;
struct Software {
    decoder: SymphoniaDecoder,
    packet: Vec<Frame>,
    scratch: BudgetTicket,
    stopped: Arc<AtomicBool>,
}
pub(super) fn open(
    source: Box<dyn MediaRead>,
    length: u64,
    pool: BudgetPool,
    stopped: Arc<AtomicBool>,
) -> Result<Box<dyn AudioDecoder>, String> {
    let scratch = pool.reserve(READER_BYTES).map_err(|e| e.message)?;
    if stopped.load(Ordering::Acquire) {
        return Err("audio preparation cancelled".into());
    }
    let decoder = SymphoniaDecoder::from_reader(source, length).map_err(|e| e.to_string())?;
    Ok(Box::new(Software {
        decoder,
        packet: Vec::new(),
        scratch,
        stopped,
    }))
}
impl AudioDecoder for Software {
    fn info(&self) -> AudioInfo {
        AudioInfo {
            sample_rate: self.decoder.sample_rate(),
            frames: (self.decoder.num_frames() != usize::MAX)
                .then_some(self.decoder.num_frames() as u64),
        }
    }
    fn seek(&mut self, frame: u64) -> Result<u64, String> {
        let frame = usize::try_from(frame).map_err(|_| "audio frame exceeds address space")?;
        self.packet.clear();
        self.decoder
            .seek(frame)
            .map(|n| n as u64)
            .map_err(|e| e.to_string())
    }
    fn decode(&mut self, cancelled: &AtomicBool) -> Result<bool, String> {
        self.packet = Vec::new();
        self.packet = self
            .decoder
            .decode_reserved(|bytes| {
                if cancelled.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire) {
                    return Err(std::io::Error::other("audio preparation cancelled"));
                }
                let bytes = READER_BYTES
                    .checked_add(bytes as u64)
                    .ok_or_else(|| std::io::Error::other("audio workspace overflow"))?;
                self.scratch
                    .resize(self.scratch.bytes().max(bytes))
                    .map_err(|e| std::io::Error::other(e.message))
            })
            .map_err(|e| e.to_string())?;
        self.scratch
            .resize(
                self.scratch
                    .bytes()
                    .max(READER_BYTES + self.packet.capacity() as u64 * size_of::<Frame>() as u64),
            )
            .map_err(|e| e.message)?;
        Ok(!self.packet.is_empty())
    }
    fn pcm(&self) -> AudioPcm<'_> {
        AudioPcm::Frames(&self.packet)
    }
}
