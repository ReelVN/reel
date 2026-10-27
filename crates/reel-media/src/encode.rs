//! Bounded, offline audio conversion for authoring and resource preparation.
use std::{
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use reel_core::budget::BudgetPool;
use rubato::{
    Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction, audioadapter_buffers::direct::InterleavedSlice,
};

use crate::decode::{
    AudioDecoder, AudioDecoderFactory, AudioInfo, MediaRead, SoftwareAudioDecoderFactory,
};

const RATE: u32 = 48_000;
const FRAMES: usize = 960;
const PACKET_BYTES: usize = 4000;
const SERIAL: u32 = 0x5265_656c;

/// Convert a seekable encoded source to deterministic Ogg/Opus on the caller's worker.
///
/// Sources include WAV, Vorbis, FLAC, MP3 and Opus audio.
/// Source mono/stereo channels are preserved at 48 kHz in 20 ms packets. Other source rates
/// use Rubato's band-limited sinc resampler. The header pre-skip and final granule
/// remove resampler/encoder delay and packet padding, preserving the audible length.
/// `bitrate` is bits per second in `6000..=512000`. `budget` charges owned PCM and
/// packet buffers; third-party codec/resampler internal storage is measured separately.
/// Cancellation is checked between source packets and output packets. An error may
/// leave a partial output, so callers publish their staging file only after success.
pub fn opus(
    reader: impl MediaRead + 'static,
    length: u64,
    output: impl Write,
    bitrate: u32,
    budget: BudgetPool,
    cancelled: &AtomicBool,
) -> Result<AudioInfo, String> {
    if !(6000..=512_000).contains(&bitrate) {
        return Err("Opus bitrate must be 6000..=512000 bits per second".into());
    }
    checkpoint(cancelled)?;
    let decoder = SoftwareAudioDecoderFactory.open(
        Box::new(reader),
        length,
        budget.clone(),
        Arc::new(AtomicBool::new(false)),
    )?;
    opus_decoded(decoder, output, bitrate, budget, cancelled)
}

/// Encode a worker-owned PCM decoder through the same bounded Opus pipeline.
///
/// The decoder retains ownership of its reusable packet and external resources.
/// EOF must include decoder/transport success; an error leaves an unpublished
/// partial output. Mono/stereo channels and the source timeline are preserved.
/// `budget` accounts for encoder/resampler storage alongside the decoder's pool.
pub fn opus_decoded(
    decoder: Box<dyn AudioDecoder>,
    output: impl Write,
    bitrate: u32,
    budget: BudgetPool,
    cancelled: &AtomicBool,
) -> Result<AudioInfo, String> {
    if !(6000..=512_000).contains(&bitrate) {
        return Err("Opus bitrate must be 6000..=512000 bits per second".into());
    }
    checkpoint(cancelled)?;
    let rate = decoder.info().sample_rate;
    let channels = usize::from(decoder.info().channels);
    if !(1..=2).contains(&channels) {
        return Err("Opus conversion requires mono or stereo audio".into());
    }
    if !(8000..=384_000).contains(&rate) {
        return Err("Opus conversion requires an 8000..=384000 Hz source".into());
    }
    let mut source = Source {
        decoder,
        offset: 0,
        eof: false,
        frames: 0,
        channels,
    };
    let mut resampler = if rate == RATE {
        None
    } else {
        let params = SincInterpolationParameters::new(256, WindowFunction::BlackmanHarris2)
            .oversampling_factor(256)
            .interpolation(SincInterpolationType::Cubic);
        Some(
            Async::<f32>::new_sinc(
                f64::from(RATE) / f64::from(rate),
                1.0,
                &params,
                FRAMES,
                channels,
                FixedAsync::Output,
            )
            .map_err(error)?,
        )
    };
    let input_capacity = resampler
        .as_ref()
        .map_or(FRAMES, Resampler::input_frames_max);
    let bytes = (input_capacity + FRAMES) * channels * size_of::<f32>() + 2 * PACKET_BYTES;
    let _workspace = budget.reserve(bytes as u64).map_err(|e| e.message)?;
    let mut input = vec![0.; input_capacity * channels];
    let mut pcm = vec![0.; FRAMES * channels];
    let mut encoder = opus::Encoder::new(
        RATE,
        if channels == 1 {
            opus::Channels::Mono
        } else {
            opus::Channels::Stereo
        },
        opus::Application::Audio,
    )
    .map_err(error)?;
    encoder
        .set_bitrate(opus::Bitrate::Bits(bitrate as i32))
        .map_err(error)?;
    let preskip = u16::try_from(
        encoder.get_lookahead().map_err(error)? as usize
            + resampler.as_ref().map_or(0, Resampler::output_delay),
    )
    .map_err(|_| "Opus pre-skip exceeds header capacity")?;
    let mut writer = PacketWriter::new(output);
    let mut head = Vec::from(&b"OpusHead\x01"[..]);
    head.push(channels as u8);
    head.extend_from_slice(&preskip.to_le_bytes());
    head.extend_from_slice(&rate.to_le_bytes());
    head.extend_from_slice(&[0, 0, 0]);
    writer
        .write_packet(head, SERIAL, PacketWriteEndInfo::EndPage, 0)
        .map_err(error)?;
    // Empty, deterministic vendor string and no author metadata.
    writer
        .write_packet(
            Vec::from(&b"OpusTags\0\0\0\0\0\0\0\0"[..]),
            SERIAL,
            PacketWriteEndInfo::EndPage,
            0,
        )
        .map_err(error)?;
    let mut encoded = 0_u64;
    let mut pending = Vec::with_capacity(PACKET_BYTES);
    let mut packet = [0_u8; PACKET_BYTES];
    loop {
        checkpoint(cancelled)?;
        let needed = resampler
            .as_ref()
            .map_or(FRAMES, Resampler::input_frames_next);
        let actual = source.fill(&mut input[..needed * channels], cancelled)?;
        let audible = rescaled_frames(source.frames, rate)?;
        let final_granule = audible
            .checked_add(u64::from(preskip))
            .ok_or("Opus duration overflow")?;
        if source.eof && source.frames == 0 {
            return Err("audio contains no PCM frames".into());
        }
        if source.eof && encoded >= final_granule {
            writer
                .write_packet(
                    pending,
                    SERIAL,
                    PacketWriteEndInfo::EndStream,
                    final_granule,
                )
                .map_err(error)?;
            writer.inner_mut().flush().map_err(error)?;
            return Ok(AudioInfo {
                sample_rate: RATE,
                channels: channels as u8,
                frames: Some(audible),
            });
        }
        if let Some(resampler) = &mut resampler {
            let input_adapter =
                InterleavedSlice::new(&input, channels, input_capacity).map_err(error)?;
            let mut output_adapter =
                InterleavedSlice::new_mut(&mut pcm, channels, FRAMES).map_err(error)?;
            resampler
                .process_into_buffer(
                    &input_adapter,
                    &mut output_adapter,
                    Some(&Indexing::new().partial_len(actual)),
                )
                .map_err(error)?;
        } else {
            pcm.copy_from_slice(&input);
        }
        if !pending.is_empty() {
            writer
                .write_packet(
                    std::mem::take(&mut pending),
                    SERIAL,
                    PacketWriteEndInfo::EndPage,
                    encoded,
                )
                .map_err(error)?;
        }
        let count = encoder.encode_float(&pcm, &mut packet).map_err(error)?;
        pending = Vec::from(&packet[..count]);
        encoded = encoded
            .checked_add(FRAMES as u64)
            .ok_or("Opus duration overflow")?;
        if source.eof && encoded >= final_granule {
            writer
                .write_packet(
                    pending,
                    SERIAL,
                    PacketWriteEndInfo::EndStream,
                    final_granule,
                )
                .map_err(error)?;
            writer.inner_mut().flush().map_err(error)?;
            return Ok(AudioInfo {
                sample_rate: RATE,
                channels: channels as u8,
                frames: Some(audible),
            });
        }
    }
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn checkpoint(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        Err("audio conversion cancelled".into())
    } else {
        Ok(())
    }
}
fn rescaled_frames(frames: u64, rate: u32) -> Result<u64, String> {
    u64::try_from((u128::from(frames) * u128::from(RATE) + u128::from(rate) / 2) / u128::from(rate))
        .map_err(|_| "Opus duration overflow".into())
}
struct Source {
    decoder: Box<dyn AudioDecoder>,
    offset: usize,
    eof: bool,
    frames: u64,
    channels: usize,
}
impl Source {
    fn fill(&mut self, output: &mut [f32], cancelled: &AtomicBool) -> Result<usize, String> {
        output.fill(0.);
        let mut written = 0;
        while written < output.len() / self.channels && !self.eof {
            if self.offset == self.decoder.pcm().len() {
                if !self.decoder.decode(cancelled)? {
                    self.eof = true;
                    break;
                }
                self.offset = 0;
            }
            let packet = self.decoder.pcm();
            let count = (packet.len() - self.offset).min(output.len() / self.channels - written);
            for (target, frame) in output
                [written * self.channels..(written + count) * self.channels]
                .chunks_exact_mut(self.channels)
                .zip(&packet[self.offset..self.offset + count])
            {
                target.copy_from_slice(&[frame.left, frame.right][..self.channels]);
            }
            self.offset += count;
            written += count;
            self.frames = self
                .frames
                .checked_add(count as u64)
                .ok_or("audio duration overflow")?;
        }
        Ok(written)
    }
}
