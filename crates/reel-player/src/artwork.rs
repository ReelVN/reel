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

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const PNG_HEADER_BYTES: usize = 33;

/// A PNG whose output size can be determined without a worker probe.
pub(crate) struct PngShape {
    pub size: [u32; 2],
    pub output: u64,
    pub bounded_metadata: bool,
    pixel_workspace: u64,
    direct_output: bool,
}

impl PngShape {
    pub(crate) fn temporary(&self, available: u64, allow_direct: bool) -> u64 {
        if !self.bounded_metadata {
            return self.pixel_workspace;
        }
        let full_image = self
            .pixel_workspace
            .saturating_add(128 << 10)
            .min(available)
            .max(self.pixel_workspace);
        if allow_direct && self.direct_output {
            full_image.min(direct_png_workspace(self.size[0]))
        } else {
            full_image
        }
    }
}

pub(crate) fn direct_png_workspace(width: u32) -> u64 {
    // png 0.18.1 retains a 32 KiB lookback and grows its row window in 8 KiB
    // steps. Reserve room for its 128 KiB or four-row compaction threshold,
    // old/new allocation overlap, fdeflate 0.3.7's tables and bounded chunks.
    (576 << 10) + 26 * (u64::from(width) * 4 + 1)
}

fn png_header_has_color(header: &[u8], color: png::ColorType) -> bool {
    header.starts_with(PNG_SIGNATURE)
        && header[24] == png::BitDepth::Eight as u8
        && header[25] == color as u8
}

pub(crate) fn is_bounded_rgba8_png(bytes: &[u8]) -> bool {
    bytes.get(..PNG_HEADER_BYTES).is_some_and(|header| {
        png_header_has_color(header, png::ColorType::Rgba)
            && header[28] == 0
            && bounded_png_transparency(&bytes[PNG_HEADER_BYTES..]).is_some()
    })
}

pub(crate) fn png_shape(bytes: &[u8]) -> Option<PngShape> {
    if !bytes.starts_with(PNG_SIGNATURE) {
        return None;
    }
    let mut header = png::Decoder::new(std::io::Cursor::new(bytes.get(..PNG_HEADER_BYTES)?));
    let info = header.read_header_info().ok()?;
    if info.bit_depth == png::BitDepth::Sixteen {
        return None;
    }
    let transparency = bounded_png_transparency(&bytes[PNG_HEADER_BYTES..]);
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
        pixel_workspace: pixels.checked_mul(4 + channels)?,
        bounded_metadata: transparency.is_some(),
        direct_output: !info.interlaced
            && (info.color_type == png::ColorType::Rgba
                || (info.color_type == png::ColorType::Rgb && transparency == Some(false))),
    })
}

/// Decode bounded, noninterlaced RGB8 directly into the resident RGBA8 buffer.
pub(crate) fn decode_rgb_png_rows(
    bytes: &[u8],
    temporary: u64,
    output: u64,
) -> Option<Result<Vec<u8>, String>> {
    let header = bytes.get(..PNG_HEADER_BYTES)?;
    if !png_header_has_color(header, png::ColorType::Rgb)
        || header[28] != 0
        || bounded_png_transparency(&bytes[PNG_HEADER_BYTES..]) != Some(false)
    {
        return None;
    }
    Some((|| {
        let mut decoder = png::Decoder::new_with_limits(
            std::io::Cursor::new(bytes),
            png::Limits {
                bytes: usize::try_from(temporary).unwrap_or(usize::MAX),
            },
        );
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
        let (width, height) = reader.info().size();
        let pixels = u64::from(width) * u64::from(height);
        let rgba = pixels.checked_mul(4).ok_or("image size overflow")?;
        let workspace = pixels.saturating_mul(7).min(direct_png_workspace(width));
        if workspace > temporary || rgba > output {
            return Err("image decode reservation too small".into());
        }
        let length = usize::try_from(rgba)
            .ok()
            .filter(|&len| len <= isize::MAX as usize)
            .ok_or("image size overflow")?;
        let stride = u64::from(width) * 4;
        let mut rgba = vec![0; length];
        for destination in rgba.chunks_exact_mut(stride as usize) {
            let row = reader
                .next_row()
                .map_err(|error| error.to_string())?
                .ok_or("PNG ended before its last row")?;
            for (&[red, green, blue], pixel) in row
                .data()
                .as_chunks::<3>()
                .0
                .iter()
                .zip(destination.as_chunks_mut::<4>().0)
            {
                *pixel = [red, green, blue, 255];
            }
        }
        // Flush the current frame's remaining IDAT data, as next_frame does.
        reader.next_row().map_err(|error| error.to_string())?;
        Ok(rgba)
    })())
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
