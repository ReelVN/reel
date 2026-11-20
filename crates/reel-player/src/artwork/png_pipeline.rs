//! Overlap PNG inflate with reconstruction of complete scanlines on desktop.

use super::direct_png_workspace;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, SyncSender, sync_channel},
};

const PACKET_BYTES: usize = 128 << 10;
const INFLATE_SLACK: usize = 40 << 10;
const QUEUE_PACKETS: usize = 2;
static HELPER_BUSY: AtomicBool = AtomicBool::new(false);

struct HelperPermit;

impl HelperPermit {
    fn acquire() -> Option<Self> {
        HELPER_BUSY
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for HelperPermit {
    fn drop(&mut self) {
        HELPER_BUSY.store(false, Ordering::Release);
    }
}

pub(super) fn workspace(width: u32) -> u64 {
    let stride = u64::from(width) * 4 + 1;
    let packet = (PACKET_BYTES as u64).max(stride);
    // The ordinary parser allowance also covers fdeflate's tables. In addition,
    // count the inflate window, queued packets, each thread's active packet and
    // the previous reconstructed row. The output retains its separate ticket.
    direct_png_workspace(width)
        + packet * (QUEUE_PACKETS as u64 + 3)
        + INFLATE_SLACK as u64
        + stride * 2
}

pub(super) fn decode(
    input: &[u8],
    [width, height]: [u32; 2],
    channels: usize,
    temporary: u64,
    output: u64,
    cancelled: &AtomicBool,
) -> Option<Result<Vec<u8>, String>> {
    let width_usize = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    let row_bytes = width_usize.checked_mul(channels)?;
    let stride = row_bytes.checked_add(1)?;
    let padded = stride.checked_mul(height)?;
    let output_row = width_usize.checked_mul(4)?;
    let output_len = output_row.checked_mul(height)?;
    if output_len == 0
        || output_len > isize::MAX as usize
        || output_len as u64 > output
        || temporary < workspace(width)
    {
        return None;
    }
    let _permit = HelperPermit::acquire()?;
    let packet = (PACKET_BYTES / stride).max(1) * stride;
    std::thread::scope(|scope| {
        let (send, recv) = sync_channel(QUEUE_PACKETS);
        // If the platform cannot start a helper, resume on the ordinary decoder.
        let inflater = std::thread::Builder::new()
            .name("reel-png-inflate".into())
            .spawn_scoped(scope, move || {
                inflate(input, stride, packet, padded, send, cancelled)
            })
            .ok()?;
        let pixels = reconstruct(recv, stride, channels, output_row, output_len, cancelled);
        let inflated = inflater
            .join()
            .map_err(|_| "PNG inflate helper panicked".to_owned());
        Some(inflated.and_then(|result| result).and(pixels))
    })
}

fn inflate(
    mut input: &[u8],
    stride: usize,
    packet: usize,
    mut remaining: usize,
    send: SyncSender<Vec<u8>>,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let mut decoder = png::StreamingDecoder::new();
    let mut region = png::UnfilterRegion::default();
    let mut raw = vec![0; packet + INFLATE_SLACK + stride];
    let mut sent = 0;
    while !input.is_empty() {
        check_cancelled(cancelled)?;
        let (consumed, decoded) = decoder
            .update(input, Some(&mut region.as_buf(&mut raw)))
            .map_err(|error| error.to_string())?;
        input = &input[consumed..];
        let finished = matches!(decoded, png::Decoded::ImageDataFlushed);
        let complete = region.filled / stride * stride;
        if complete - sent > remaining {
            return Err("PNG has more data than its dimensions allow".into());
        }
        let prior_sent = sent;
        while complete - sent >= packet || finished && complete > sent {
            let end = complete.min(sent + packet);
            // Only the copy can be reconstructed. The inflator may still refer
            // back to the original filtered bytes in its lookback window.
            if send.send(raw[sent..end].to_vec()).is_err() {
                return Ok(());
            }
            remaining -= end - sent;
            sent = end;
        }
        if finished {
            return if remaining == 0 && complete == region.filled {
                Ok(())
            } else {
                Err("PNG ended before its last row".into())
            };
        }
        let discard = if sent > prior_sent {
            region.available.min(sent) / stride * stride
        } else {
            0
        };
        if discard != 0 {
            raw.copy_within(discard..region.filled, 0);
            region.available -= discard;
            region.filled -= discard;
            sent -= discard;
        }
        if consumed == 0 && matches!(decoded, png::Decoded::Nothing) && discard == 0 {
            return Err("PNG inflator could not advance".into());
        }
    }
    Err("PNG ended before its image data was flushed".into())
}

fn reconstruct(
    packets: Receiver<Vec<u8>>,
    stride: usize,
    channels: usize,
    output_row: usize,
    output_len: usize,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let mut pixels = vec![0; output_len];
    let mut remaining = pixels.chunks_exact_mut(output_row);
    let mut previous = vec![0; stride - 1];
    for mut packet in packets {
        check_cancelled(cancelled)?;
        for row in packet.chunks_exact_mut(stride) {
            let filter = match row[0] {
                0 => png::Filter::NoFilter,
                1 => png::Filter::Sub,
                2 => png::Filter::Up,
                3 => png::Filter::Avg,
                4 => png::Filter::Paeth,
                _ => return Err("invalid PNG row filter".into()),
            };
            let data = &mut row[1..];
            // png 0.18.1 exposes its ordinary row filters behind this feature.
            png::benchable_apis::unfilter(filter, channels as u8, &previous, data);
            previous.copy_from_slice(data);
            let output = remaining.next().ok_or("PNG has too many rows")?;
            if channels == 4 {
                output.copy_from_slice(data);
            } else {
                for (&[red, green, blue], pixel) in data
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .zip(output.as_chunks_mut::<4>().0)
                {
                    *pixel = [red, green, blue, 255];
                }
            }
        }
    }
    if remaining.len() != 0 {
        return Err("PNG ended before its last row".into());
    }
    Ok(pixels)
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("decode cancelled".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstructed_png_is_exact_and_rejects_corrupt_or_incomplete_data() {
        let size = [1100, 1000];
        let expected: Vec<u8> = (0..size[0] * size[1])
            .flat_map(|index| {
                let sample = (index % 241) as u8;
                [sample, sample ^ 97, 240 - sample, sample]
            })
            .collect();
        let temporary = workspace(size[0]);
        let output = expected.len() as u64;
        let cancelled = AtomicBool::new(false);
        let run = |input: &[u8], limit: u64| decode(input, size, 4, limit, output, &cancelled);

        let mut encoded = Vec::new();
        let mut encoder = png::Encoder::new(&mut encoded, size[0], size[1]);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_filter(png::Filter::Paeth);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&expected)
            .unwrap();
        assert!(run(&encoded, temporary - 1).is_none());
        assert_eq!(run(&encoded, temporary).unwrap().unwrap(), expected);

        // The first chunk after IHDR is IDAT in this fixture. Alter only its
        // checksum so the filtered bytes are valid until the parser checks it.
        assert_eq!(&encoded[37..41], b"IDAT");
        let idat_bytes = u32::from_be_bytes(encoded[33..37].try_into().unwrap()) as usize;
        let crc = 41 + idat_bytes;
        assert!(run(&encoded[..crc - 1], temporary).unwrap().is_err());
        encoded[crc] ^= 1;
        assert!(run(&encoded, temporary).unwrap().is_err());
    }
}
