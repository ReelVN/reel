//! Source artwork decoding shared by the player and offline resource tools.
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
mod png_pipeline;

use image::{ImageDecoder, ImageFormat, ImageReader, Limits};
use std::{
    fs::File,
    io::{BufRead, BufReader, Seek},
    path::Path,
    sync::atomic::AtomicBool,
};

/// Maximum compressed input for Vita game assets.
pub const VITA_INPUT_BYTES: u64 = 24 << 20;
/// Vita game workspace for archive reads and artwork decoding.
pub const VITA_DECODE_BYTES: u64 = 12 << 20;

/// An inspected source image and its codec workspace estimate, excluding decoded pixels.
pub type ArtworkDecoder<'a> = (Box<dyn ImageDecoder + 'a>, u64);

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const PNG_HEADER_BYTES: usize = 33;
const QOI_SIGNATURE: [u8; 4] = qoi::consts::QOI_MAGIC.to_be_bytes();

fn check_qoi_colorspace(header: &qoi::Header) -> Result<(), String> {
    if header.colorspace == qoi::ColorSpace::Srgb {
        Ok(())
    } else {
        Err("QOI artwork requires sRGB color channels".into())
    }
}

/// Read the fixed QOI header without allocating a decoder or pixel buffer.
pub(crate) fn qoi_shape(bytes: &[u8]) -> Option<([u32; 2], u64)> {
    if !bytes.starts_with(&QOI_SIGNATURE) {
        return None;
    }
    let header = qoi::decode_header(bytes).ok()?;
    let size = [header.width, header.height];
    let output = u64::from(header.width) * u64::from(header.height) * 4;
    Some((size, output))
}

/// Decode QOI directly into the resident RGBA8 allocation. The codec's pixel
/// index is fixed-size stack storage.
pub(crate) fn decode_qoi(bytes: &[u8], output: u64) -> Option<Result<Vec<u8>, String>> {
    if !bytes.starts_with(&QOI_SIGNATURE) {
        return None;
    }
    Some((|| {
        let mut decoder = qoi::Decoder::new(bytes)
            .map_err(|error| error.to_string())?
            .with_channels(qoi::Channels::Rgba);
        check_qoi_colorspace(decoder.header())?;
        if decoder.required_buf_len() as u64 > output {
            return Err("image decode reservation too small".into());
        }
        decoder.decode_to_vec().map_err(|error| error.to_string())
    })())
}

/// Read a static WebP canvas size for bounded worker admission. The worker
/// validates the container and pixels using the ordinary codec.
pub(crate) fn webp_shape(bytes: &[u8]) -> Option<([u32; 2], u64, u64)> {
    let riff_header = bytes.get(..20)?;
    if &riff_header[..4] != b"RIFF" || &riff_header[8..12] != b"WEBP" {
        return None;
    }
    let length = bytes.len() as u64;
    let riff = u64::from(u32::from_le_bytes(riff_header[4..8].try_into().unwrap()));
    let chunk = u64::from(u32::from_le_bytes(riff_header[16..20].try_into().unwrap()));
    let first_end = chunk.next_multiple_of(2) + 20;
    if riff + 8 != length || first_end > length {
        return None;
    }
    let (size, channels, may_use_vp8) = match &riff_header[12..16] {
        b"VP8 " if first_end == length => {
            let header = bytes.get(20..30)?;
            let tag = u32::from_le_bytes([header[0], header[1], header[2], 0]);
            if tag & 1 != 0
                || header[3..6] != [0x9d, 0x01, 0x2a]
                || chunk < 10 + u64::from(tag >> 5)
            {
                return None;
            }
            let size = [6, 8].map(|start| {
                u32::from(u16::from_le_bytes(header[start..start + 2].try_into().unwrap()) & 0x3fff)
            });
            (size, 3, true)
        }
        b"VP8L" if first_end == length && chunk >= 5 && bytes[20] == 0x2f => {
            let header = u32::from_le_bytes(bytes[21..25].try_into().unwrap());
            if header >> 29 != 0 {
                return None;
            }
            // Match the extent reported by the current image-webp decoder.
            let size = [(1 + header) & 0x3fff, (1 + (header >> 14)) & 0x3fff];
            (size, 3 + u64::from((header >> 28) & 1), false)
        }
        b"VP8X" if chunk == 10 && bytes[20] & 0x02 == 0 => {
            let size =
                [24, 27].map(|i| 1 + u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], 0]));
            (size, 4, true)
        }
        _ => return None,
    };
    if size.contains(&0) {
        return None;
    }
    let pixels = u64::from(size[0]) * u64::from(size[1]);
    // In image-webp 0.2.4 the last partition can hold both the growing
    // read buffer and its aligned copy. At most one malformed 24-bit
    // partition length can allocate before its read fails.
    let encoded_workspace = if may_use_vp8 {
        (length * 4).max(length + (1 << 24))
    } else {
        length
    };
    let workspace = webp_workspace(size, encoded_workspace) + pixels * (4 + channels);
    Some((size, pixels * 4, workspace))
}

fn webp_workspace([width, height]: [u32; 2], encoded_workspace: u64) -> u64 {
    // Account for padded VP8 planes, alpha or VP8L transform buffers,
    // compressed partitions and decoder tables alongside the pixel output.
    let padded = u64::from(width.next_multiple_of(16)) * u64::from(height.next_multiple_of(16));
    padded * 8 + encoded_workspace + (128 << 10)
}

/// A PNG whose output size can be determined without a worker probe.
pub(crate) struct PngShape {
    pub size: [u32; 2],
    pub output: u64,
    pub bounded_metadata: bool,
    pixel_workspace: u64,
    direct_output: bool,
}

impl PngShape {
    pub(crate) fn temporary(
        &self,
        available: u64,
        allow_direct: bool,
        pipeline_workspace_limit: Option<u64>,
    ) -> u64 {
        if !self.bounded_metadata {
            return self.pixel_workspace;
        }
        let full_image = self
            .pixel_workspace
            .saturating_add(128 << 10)
            .min(available)
            .max(self.pixel_workspace);
        if allow_direct && self.direct_output {
            let workspace = direct_png_workspace(self.size[0]);
            #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
            let workspace = {
                let pipeline = png_pipeline::workspace(self.size[0]);
                // Parallel decoding is worthwhile once the parallel row buffers fit
                // within the existing full-image allocation bound.
                if pipeline <= full_image
                    && pipeline_workspace_limit.is_some_and(|limit| pipeline <= limit)
                {
                    workspace.max(pipeline)
                } else {
                    workspace
                }
            };
            #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
            let _ = pipeline_workspace_limit;
            full_image.min(workspace)
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

/// Decode bounded PNG rows into the resident RGBA8 buffer.
pub(crate) fn decode_png_rows(
    bytes: &[u8],
    temporary: u64,
    output: u64,
    allow_pipeline: bool,
    cancelled: &AtomicBool,
) -> Option<Result<Vec<u8>, String>> {
    let header = bytes.get(..PNG_HEADER_BYTES)?;
    let rgb = png_header_has_color(header, png::ColorType::Rgb);
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    let width = u32::from_be_bytes(header[16..20].try_into().unwrap());
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    let pipeline = allow_pipeline && png_pipeline::workspace(width) <= temporary;
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    let pipeline = false;
    if header[28] != 0 || !(rgb || pipeline && png_header_has_color(header, png::ColorType::Rgba)) {
        return None;
    }
    let transparency = bounded_png_transparency(&bytes[PNG_HEADER_BYTES..])?;
    if rgb && transparency {
        return None;
    }
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    if pipeline {
        let height = u32::from_be_bytes(header[20..24].try_into().unwrap());
        if let Some(decoded) = png_pipeline::decode(
            bytes,
            [width, height],
            if rgb { 3 } else { 4 },
            temporary,
            output,
            cancelled,
        ) {
            return Some(decoded);
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    let _ = (allow_pipeline, cancelled);
    if !rgb {
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
        Some(ImageFormat::Qoi) => {
            let mut source = reader.into_inner();
            let mut bytes = [0; qoi::consts::QOI_HEADER_SIZE];
            source
                .read_exact(&mut bytes)
                .map_err(|error| error.to_string())?;
            let header = qoi::decode_header(bytes).map_err(|error| error.to_string())?;
            check_qoi_colorspace(&header)?;
            source
                .seek_relative(-(bytes.len() as i64))
                .map_err(|error| error.to_string())?;
            let decoder =
                image::codecs::qoi::QoiDecoder::new(source).map_err(|error| error.to_string())?;
            Ok((Box::new(decoder), 0))
        }
        Some(ImageFormat::WebP) => {
            let decoder = image::codecs::webp::WebPDecoder::new(reader.into_inner())
                .map_err(|error| error.to_string())?;
            if decoder.has_animation() {
                return Err("animated WebP is not supported for artwork".into());
            }
            let (width, height) = decoder.dimensions();
            let workspace = webp_workspace([width, height], source_bytes);
            Ok((Box::new(decoder), workspace))
        }
        _ => Err("expected PNG, JPEG, static WebP or QOI artwork".into()),
    }
}
