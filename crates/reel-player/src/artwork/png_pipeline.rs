//! Overlap PNG inflate with reconstruction of complete scanlines on desktop.

use super::direct_png_workspace;
use std::{
    ops::Range,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
};

const PACKET_BYTES: usize = 128 << 10;
const INFLATE_SLACK: usize = 40 << 10;
const QUEUE_PACKETS: usize = 2;
const BUFFERS: usize = QUEUE_PACKETS + 3;
static HELPER_BUSY: AtomicBool = AtomicBool::new(false);

struct Packet {
    bytes: Vec<u8>,
    rows: Range<usize>,
}

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
    // count both producer buffers during a swap, queued packets, the consumer
    // buffer and its previous row. The output retains its separate ticket.
    direct_png_workspace(width) + (packet + INFLATE_SLACK as u64 + stride) * BUFFERS as u64 + stride
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
        let (recycle, recycled) = sync_channel(BUFFERS);
        // If the platform cannot start a helper, resume on the ordinary decoder.
        let inflater = std::thread::Builder::new()
            .name("reel-png-inflate".into())
            .spawn_scoped(scope, move || {
                inflate(input, stride, packet, padded, send, recycled, cancelled)
            })
            .ok()?;
        let pixels = reconstruct(
            recv, recycle, stride, channels, output_row, output_len, cancelled,
        );
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
    send: SyncSender<Packet>,
    recycled: Receiver<Vec<u8>>,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    let mut decoder = png::StreamingDecoder::new();
    let mut region = png::UnfilterRegion::default();
    let buffer_len = packet + INFLATE_SLACK + stride;
    let mut raw = vec![0; buffer_len];
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
        if finished {
            if remaining != complete - sent || complete != region.filled {
                return Err("PNG ended before its last row".into());
            }
            if complete > sent {
                let _ = send.send(Packet {
                    bytes: raw,
                    rows: sent..complete,
                });
            }
            return Ok(());
        }
        let discard = if complete - sent >= packet {
            region.available.min(complete) / stride * stride
        } else {
            0
        };
        if discard != 0 {
            // Reconstruction consumes the whole buffer. Preserve only the
            // filtered lookback and partial row in the next reusable buffer.
            let mut next = recycled.try_recv().unwrap_or_else(|_| vec![0; buffer_len]);
            next[..region.filled - discard].copy_from_slice(&raw[discard..region.filled]);
            let bytes = std::mem::replace(&mut raw, next);
            if send
                .send(Packet {
                    bytes,
                    rows: sent..complete,
                })
                .is_err()
            {
                return Ok(());
            }
            remaining -= complete - sent;
            region.available -= discard;
            region.filled -= discard;
            sent = complete - discard;
        }
        if consumed == 0 && matches!(decoded, png::Decoded::Nothing) && discard == 0 {
            return Err("PNG inflator could not advance".into());
        }
    }
    Err("PNG ended before its image data was flushed".into())
}

fn reconstruct(
    packets: Receiver<Packet>,
    recycle: SyncSender<Vec<u8>>,
    stride: usize,
    channels: usize,
    output_row: usize,
    output_len: usize,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let mut pixels = vec![0; output_len];
    let mut remaining = pixels.chunks_exact_mut(output_row);
    let mut previous = vec![0; stride - 1];
    for Packet { mut bytes, rows } in packets {
        check_cancelled(cancelled)?;
        let mut previous_in_packet: Option<&[u8]> = None;
        for row in bytes[rows].chunks_exact_mut(stride) {
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
            png::benchable_apis::unfilter(
                filter,
                channels as u8,
                previous_in_packet.unwrap_or(&previous),
                data,
            );
            previous_in_packet = Some(data);
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
        if let Some(last_row) = previous_in_packet {
            previous.copy_from_slice(last_row);
        }
        let _ = recycle.send(bytes);
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
        let temporary = workspace(size[0]);
        let cancelled = AtomicBool::new(false);
        for (color, channels) in [(png::ColorType::Rgb, 3), (png::ColorType::Rgba, 4)] {
            let expected: Vec<u8> = (0..size[0] * size[1])
                .flat_map(|index| {
                    let sample = (index % 241) as u8;
                    let alpha = if channels == 4 { sample } else { 255 };
                    [sample, sample ^ 97, 240 - sample, alpha]
                })
                .collect();
            let source: Vec<u8> = expected
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|pixel| pixel[..channels].iter().copied())
                .collect();
            let output = expected.len() as u64;
            let run =
                |input: &[u8], limit: u64| decode(input, size, channels, limit, output, &cancelled);
            let mut encoded = Vec::new();
            let mut encoder = png::Encoder::new(&mut encoded, size[0], size[1]);
            encoder.set_color(color);
            encoder.set_filter(png::Filter::Paeth);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&source)
                .unwrap();
            assert!(run(&encoded, temporary - 1).is_none());
            assert_eq!(run(&encoded, temporary).unwrap().unwrap(), expected);

            // Alter only the first IDAT checksum, leaving filtered bytes valid.
            assert_eq!(&encoded[37..41], b"IDAT");
            let idat_bytes = u32::from_be_bytes(encoded[33..37].try_into().unwrap()) as usize;
            let crc = 41 + idat_bytes;
            assert!(run(&encoded[..crc - 1], temporary).unwrap().is_err());
            encoded[crc] ^= 1;
            assert!(run(&encoded, temporary).unwrap().is_err());
        }
    }
}
