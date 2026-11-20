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

/// A PNG whose output size can be determined without a worker probe.
pub(crate) struct PngShape {
    pub size: [u32; 2],
    pub output: u64,
    pub temporary: u64,
    pub bounded_metadata: bool,
}

pub(crate) fn png_shape(bytes: &[u8]) -> Option<PngShape> {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    const HEADER_BYTES: usize = 33;
    if !bytes.starts_with(SIGNATURE) {
        return None;
    }
    let mut header = png::Decoder::new(std::io::Cursor::new(bytes.get(..HEADER_BYTES)?));
    let info = header.read_header_info().ok()?;
    if info.bit_depth == png::BitDepth::Sixteen {
        return None;
    }
    let transparency = bounded_png_transparency(&bytes[HEADER_BYTES..]);
    // The image decoder expands a palette or a tRNS color key before
    // converting the resulting pixels to RGBA8.
    let channels = match (info.color_type, transparency) {
        (png::ColorType::Rgba, _) => 4,
        (png::ColorType::GrayscaleAlpha, Some(_)) => 2,
        (png::ColorType::Rgb | png::ColorType::Indexed, Some(alpha)) => 3 + u64::from(alpha),
        (png::ColorType::Grayscale, Some(alpha)) => 1 + u64::from(alpha),
        _ => return None,
    };
    let pixels = u64::from(info.width) * u64::from(info.height);
    Some(PngShape {
        size: [info.width, info.height],
        output: pixels.checked_mul(4)?,
        temporary: pixels.checked_mul(4 + channels)?,
        bounded_metadata: transparency.is_some(),
    })
}

/// Identify transparency in PNGs with bounded ancillary data. The worker
/// validates the chunks and decodes the image using the same information.
fn bounded_png_transparency(mut chunks: &[u8]) -> Option<bool> {
    let mut transparent = false;
    for _ in 0..256 {
        let Some(header) = chunks.get(..8) else { break };
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        match (&header[4..], length) {
            (b"IEND", 0) => return (chunks.len() == 12).then_some(transparent),
            (b"tRNS", 1..=256) => transparent = true,
            (b"IDAT", _)
            | (b"PLTE", 3..=768)
            | (b"pHYs", 9)
            | (b"sRGB", 1)
            | (b"gAMA", 4)
            | (b"cHRM", 32)
            | (b"bKGD", 1 | 2 | 6)
            | (b"tIME", 7) => {}
            _ => break,
        }
        let Some(rest) = length.checked_add(12).and_then(|end| chunks.get(end..)) else {
            break;
        };
        chunks = rest;
    }
    None
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
