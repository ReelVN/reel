//! Source artwork decoding shared by the player and offline resource tools.
use image::{ImageDecoder, ImageFormat, ImageReader, Limits};
use std::{
    fs::File,
    io::{BufRead, BufReader, Seek},
    path::Path,
};

/// Maximum compressed input for Vita game assets.
pub const VITA_INPUT_BYTES: u64 = 24 << 20;
/// Vita game workspace for archive reads and artwork decoding.
pub const VITA_DECODE_BYTES: u64 = 12 << 20;

/// An inspected source image and its codec workspace estimate, excluding decoded pixels.
pub type ArtworkDecoder<'a> = (Box<dyn ImageDecoder + 'a>, u64);

/// An RGBA8 PNG's first chunk contains its exact decoded shape.
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
pub(crate) struct Rgba8Png {
    pub size: [u32; 2],
    pub bytes: u64,
    pub data_only: bool,
}

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
pub(crate) fn rgba8_png_shape(bytes: &[u8]) -> Option<Rgba8Png> {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    const HEADER_BYTES: usize = 33;
    if !bytes.starts_with(SIGNATURE) {
        return None;
    }
    let mut header = png::Decoder::new(std::io::Cursor::new(bytes.get(..HEADER_BYTES)?));
    let info = header.read_header_info().ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let pixels = u64::from(info.width) * u64::from(info.height);
    Some(Rgba8Png {
        size: [info.width, info.height],
        bytes: pixels.checked_mul(4)?,
        data_only: png_has_only_image_data(&bytes[HEADER_BYTES..]),
    })
}

/// Data-only PNGs have no variable-size metadata to claim the decoder's full
/// allowance. Walk chunk headers only. Unusual inputs use the ordinary budget.
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
fn png_has_only_image_data(mut chunks: &[u8]) -> bool {
    for _ in 0..256 {
        let Some(header) = chunks.get(..8) else { break };
        if &header[4..] == b"IEND" {
            return header[..4] == [0; 4] && chunks.len() == 12;
        }
        if &header[4..] != b"IDAT" {
            break;
        }
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let Some(rest) = length.checked_add(12).and_then(|end| chunks.get(end..)) else {
            break;
        };
        chunks = rest;
    }
    false
}

/// Inspect a source file by its contents.
pub fn open(path: &Path, memory_bytes: u64) -> Result<ArtworkDecoder<'static>, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let length = file.metadata().map_err(|error| error.to_string())?.len();
    decoder(BufReader::new(file), length, memory_bytes)
}

/// Inspect a source stream. The caller reserves the decoded pixels and workspace before decoding.
pub fn decoder<'a>(
    source: impl BufRead + Seek + 'a,
    source_bytes: u64,
    memory_bytes: u64,
) -> Result<ArtworkDecoder<'a>, String> {
    let mut reader = ImageReader::new(source)
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    match reader.format() {
        Some(ImageFormat::Png | ImageFormat::Jpeg) => {
            let mut limits = Limits::no_limits();
            limits.max_alloc = Some(memory_bytes);
            reader.limits(limits);
            let decoder = reader.into_decoder().map_err(|error| error.to_string())?;
            Ok((Box::new(decoder), 0))
        }
        Some(ImageFormat::WebP) => {
            let decoder = image::codecs::webp::WebPDecoder::new(reader.into_inner())
                .map_err(|error| error.to_string())?;
            if decoder.has_animation() {
                return Err("animated WebP is not supported for artwork".into());
            }
            let (width, height) = decoder.dimensions();
            // Account for padded VP8 planes, alpha or VP8L transform buffers,
            // compressed partitions and decoder tables alongside the pixel output.
            let padded =
                u64::from(width.next_multiple_of(16)) * u64::from(height.next_multiple_of(16));
            let workspace = padded * 8 + source_bytes + (128 << 10);
            Ok((Box::new(decoder), workspace))
        }
        _ => Err("expected PNG, JPEG or static WebP artwork".into()),
    }
}
