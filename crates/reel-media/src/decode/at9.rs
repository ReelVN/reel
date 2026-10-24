//! ATRAC9 RIFF metadata and audible ranges. Codec work stays in the adapter.
use super::*;
use std::io::{Read, Seek, SeekFrom};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(target_os = "vita"), allow(dead_code))]
pub(super) struct Format {
    pub rate: u32,
    pub channels: u32,
    pub frames: u64,
}
const GUID: [u8; 16] = [
    0xd2, 0x42, 0xe1, 0x47, 0xba, 0x36, 0x8d, 0x4d, 0x88, 0xfc, 0x61, 0x65, 0x4f, 0x8c, 0x83, 0x6c,
];
fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(target_os = "vita"), allow(dead_code))]
pub(super) struct Header {
    pub format: Format,
    pub config: [u8; 4],
    /// Encoded rate, independent of a retained source-rate metadata chunk.
    pub codec_rate: u32,
    pub data_offset: u64,
    pub data_bytes: u64,
    pub block_bytes: u32,
    pub block_frames: u32,
    pub frame_samples: u32,
    pub delay: u32,
}
#[cfg_attr(not(target_os = "vita"), allow(dead_code))]
impl Header {
    pub fn block_samples(&self) -> u32 {
        self.frame_samples * self.block_frames
    }
    /// Locate the superframe that can restore overlap before an audible seek.
    pub fn seek(&self, sample: u64) -> Result<(u64, u64)> {
        if sample > self.format.frames {
            return Err("ATRAC9 seek exceeds audible frame count".into());
        }
        let requested = u64::from(self.delay) + sample;
        let overlap_start = requested.saturating_sub(u64::from(self.frame_samples));
        let packet = overlap_start / u64::from(self.block_samples());
        let address = packet * u64::from(self.block_bytes) + self.data_offset;
        let discard = requested - packet * u64::from(self.block_samples());
        Ok((address, discard))
    }
    /// Select audible frames from one decoded superframe, including seek preroll.
    pub fn pcm_range(&self, sample: u64, block: u64) -> Result<std::ops::Range<usize>> {
        if sample > self.format.frames || block >= self.data_bytes / u64::from(self.block_bytes) {
            return Err("ATRAC9 block outside source timeline".into());
        }
        let count = u64::from(self.block_samples());
        let start = block * count;
        let audible = sample + u64::from(self.delay);
        let end = self.format.frames + u64::from(self.delay);
        Ok(audible.saturating_sub(start).min(count) as usize
            ..end.saturating_sub(start).min(count) as usize)
    }
}

fn bytes_at(input: &mut (impl Read + Seek + ?Sized), offset: u64, bytes: &mut [u8]) -> Result<()> {
    input
        .seek(SeekFrom::Start(offset))
        .and_then(|_| input.read_exact(bytes))
        .map_err(|e| format!("ATRAC9 source: {e}"))
}
#[derive(Clone, Copy)]
struct Chunk {
    kind: [u8; 4],
    body: u64,
    size: u64,
    next: u64,
}
fn chunk_at(input: &mut (impl Read + Seek + ?Sized), offset: u64) -> Result<Chunk> {
    let mut bytes = [0; 8];
    bytes_at(input, offset, &mut bytes)?;
    let size = u64::from(u32le(&bytes, 4));
    let body = offset + 8;
    Ok(Chunk {
        kind: bytes[..4].try_into().unwrap(),
        body,
        size,
        next: body + size + (size & 1),
    })
}
// Recognition uses only the extensible format tag. Ordinary WAVE metadata,
// including fact chunks before fmt, belongs to the selected software decoder.
fn has_at9_format(input: &mut (impl Read + Seek + ?Sized), length: u64) -> Result<bool> {
    let mut offset = 12;
    for _ in 0..256 {
        if offset + 8 > length {
            return Ok(false);
        }
        let chunk = chunk_at(input, offset)?;
        if chunk.kind == *b"fmt " {
            if chunk.size < 40 || chunk.body + 40 > length {
                return Ok(false);
            }
            let mut fmt = [0; 40];
            bytes_at(input, chunk.body, &mut fmt)?;
            return Ok(u16le(&fmt, 0) == 0xfffe && fmt[24..40] == GUID);
        }
        if chunk.next > length {
            return Ok(false);
        }
        offset = chunk.next;
    }
    Ok(false)
}
/// Recognize ATRAC9 before applying its container and codec restrictions.
pub(super) fn inspect(
    input: &mut (impl Read + Seek + ?Sized),
    length: u64,
) -> Result<Option<Header>> {
    if length < 12 {
        return Ok(None);
    }
    let mut riff = [0; 12];
    bytes_at(input, 0, &mut riff)?;
    if riff[..4] != *b"RIFF" || riff[8..] != *b"WAVE" || !has_at9_format(input, length)? {
        return Ok(None);
    }
    let end = u64::from(u32le(&riff, 4)) + 8;
    if !(12..=length).contains(&end) {
        return Err("ATRAC9 RIFF size exceeds the source".into());
    }
    let (mut fmt, mut fact, mut payload, mut source_rate) = (None, None, None, None);
    let mut offset = 12;
    for _ in 0..256 {
        if offset == end {
            break;
        }
        if offset + 8 > end {
            return Err("ATRAC9 chunk header is truncated".into());
        }
        let chunk = chunk_at(input, offset)?;
        if chunk.next > end {
            return Err("ATRAC9 chunk exceeds its RIFF container".into());
        }
        offset = chunk.next;
        match chunk.kind {
            id if id == *b"fmt " => {
                if fmt.is_some() || chunk.size != 52 {
                    return Err("ATRAC9 requires one 52-byte format chunk".into());
                }
                let mut value = [0; 52];
                bytes_at(input, chunk.body, &mut value)?;
                fmt = Some(value);
            }
            id if id == *b"fact" => {
                if fact.is_some() || chunk.size != 12 {
                    return Err("ATRAC9 requires one 12-byte fact chunk".into());
                }
                let mut value = [0; 12];
                bytes_at(input, chunk.body, &mut value)?;
                fact = Some(value);
            }
            id if id == *b"data" => {
                if payload.replace((chunk.body, chunk.size)).is_some() {
                    return Err("ATRAC9 has multiple encoded payloads".into());
                }
            }
            id if id == *b"krSR" => {
                if source_rate.is_some() || chunk.size != 8 {
                    return Err("invalid ATRAC9 source clock metadata".into());
                }
                let mut value = [0; 8];
                bytes_at(input, chunk.body, &mut value)?;
                let rate = u32le(&value, 4);
                if u32le(&value, 0) != 1 || !(8000..=96000).contains(&rate) {
                    return Err("unsupported ATRAC9 source clock".into());
                }
                source_rate = Some(rate);
            }
            _ => {}
        }
    }
    if offset != end {
        return Err("ATRAC9 container exceeds 256 chunks".into());
    }
    let fmt = fmt.ok_or("ATRAC9 format is missing")?;
    let fact = fact.ok_or("ATRAC9 sample timeline is missing")?;
    let (data_offset, data_bytes) = payload.ok_or("ATRAC9 payload is missing")?;
    if u16le(&fmt, 0) != 0xfffe
        || fmt[24..40] != GUID
        || u16le(&fmt, 16) != 34
        || u32le(&fmt, 40) > 2
    {
        return Err("unsupported ATRAC9 extensible format".into());
    }
    let config = [fmt[44], fmt[45], fmt[46], fmt[47]];
    let code = u32::from_be_bytes(config);
    let codec_rate = match (code >> 20) & 15 {
        1 => 12000,
        4 => 24000,
        7 => 48000,
        _ => return Err("ATRAC9 sample rate is unsupported by the native codec".into()),
    };
    let channels = match (code >> 17) & 7 {
        0 => 1,
        1 | 2 => 2,
        _ => return Err("ATRAC9 native decoding supports mono or stereo".into()),
    };
    let frame_bytes = ((code >> 5) & 2047) + 1;
    let log_frames = (code >> 3) & 3;
    if config[0] != 0xfe
        || code & 0x10000 != 0
        || !matches!(log_frames, 0 | 2)
        || frame_bytes > 1024
    {
        return Err("unsupported ATRAC9 superframe geometry".into());
    }
    let block_frames = 1 << log_frames;
    let block_bytes = frame_bytes * block_frames;
    let frame_samples = codec_rate * 256 / 48000;
    let block_samples = frame_samples * block_frames;
    let samples = u32le(&fact, 0);
    let delay = u32le(&fact, 8);
    let encoded_frames = data_bytes / u64::from(block_bytes) * u64::from(block_samples);
    if u32::from(u16le(&fmt, 2)) != channels
        || u32le(&fmt, 4) != codec_rate
        || u32::from(u16le(&fmt, 12)) != block_bytes
        || u32::from(u16le(&fmt, 18)) != block_samples
        || !(frame_samples..=block_samples * 4).contains(&delay)
        || samples == 0
        || data_bytes % u64::from(block_bytes) != 0
        || u64::from(samples) + u64::from(delay) > encoded_frames
    {
        return Err("ATRAC9 format and audible timeline disagree".into());
    }
    Ok(Some(Header {
        format: Format {
            rate: source_rate.unwrap_or(codec_rate),
            channels,
            frames: u64::from(samples),
        },
        config,
        codec_rate,
        data_offset,
        data_bytes,
        block_bytes,
        block_frames,
        frame_samples,
        delay,
    }))
}
#[cfg(target_os = "vita")]
mod vita;
pub(super) fn open(
    source: Box<dyn MediaRead>,
    header: Header,
    budget: BudgetPool,
    stopped: Arc<AtomicBool>,
) -> Result<Box<dyn AudioDecoder>> {
    #[cfg(target_os = "vita")]
    {
        vita::open(source, header, budget, stopped)
    }
    #[cfg(not(target_os = "vita"))]
    {
        let _ = (source, header, budget, stopped);
        Err("ATRAC9 hardware decoding is unavailable on this target".into())
    }
}

#[cfg(test)]
mod tests;
