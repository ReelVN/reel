//! Offline image conversion into portable linear-premultiplied texture blocks.
use image::{
    ColorType, DynamicImage, ImageDecoder, ImageReader, RgbaImage, imageops, metadata::Cicp,
};
use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
use reel_core::texture::{
    ColorEncoding, TextureCodec, TextureDescriptor, TextureFormat, TextureHeader, TextureLayout,
};
use std::path::Path;
mod quality;

/// `Auto` uses BC1 for opaque images and BC3 for images with transparency.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Auto,
    Bc1,
    Bc3,
    /// Uncompressed linear-premultiplied RGBA8, useful for fine UI art and masks.
    Rgba8,
}

/// Optional offline RGB quantization for uncompressed textures.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Dither {
    /// Round to the nearest linear UNORM8 value.
    #[default]
    None,
    /// Use a fixed 8x8 threshold after linear filtering and premultiplication.
    /// RGB shares a zero-mean offset of at most 63/128 of one UNORM8 step.
    /// Alpha is rounded without dithering; RGB never exceeds rounded alpha.
    Ordered,
}

/// Conversion changes stored texels while preserving the original author coordinates.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Fit inside this box without enlarging the original image.
    pub max_size: Option<[u32; 2]>,
    /// Pad each stored axis to a power of two, with a minimum of eight texels.
    pub power_of_two: bool,
    pub format: Format,
    /// Stable RGB dithering for RGBA8 texture conversion; disabled by default.
    /// Coordinates refer to the complete resized image, with edge-repeated padding.
    /// Block-compressed formats and tiled-image conversion do not accept ordered dithering.
    pub dither: Dither,
}

impl Options {
    pub(crate) fn validate(self) -> Result<(), String> {
        if self.dither == Dither::Ordered && !matches!(self.format, Format::Rgba8) {
            return Err(
                "ordered dithering requires format rgba8; BC and auto formats are unsupported"
                    .into(),
            );
        }
        Ok(())
    }
}

/// Prepare PNG/JPEG artwork and atomically publish a new `.reel-texture` file.
/// Filtering uses linear-premultiplied colors; padding repeats the content edge.
/// Pixel and BC payloads are stored ready for upload. Release archives compress
/// these bytes in independently readable blocks. Existing output files are preserved.
/// BC fitting compares displayed colors, gradients and translucent edges against
/// the filtered source, refining affected images at the same GPU payload size.
pub fn prepare(source: &Path, output: &Path, options: Options) -> Result<TextureHeader, String> {
    prepare_bounded(
        source,
        output,
        options,
        crate::resource_conversion::MEMORY_BYTES,
    )
    .map(|(header, _)| header)
}

/// Prepare artwork within an explicit pixel and codec workspace reservation.
/// Returns the header and reserved peak bytes. The estimate covers decoded
/// pixels, linear conversion, separable resize, padded RGBA and BC encoding;
/// image decoder allocations are separately limited within the same estimate.
pub fn prepare_bounded(
    source: &Path,
    output: &Path,
    options: Options,
    memory_bytes: u64,
) -> Result<(TextureHeader, u64), String> {
    prepare_scaled_bounded(source, output, options, 1., memory_bytes)
}

pub(crate) fn prepare_scaled_bounded(
    source: &Path,
    output: &Path,
    options: Options,
    scale: f64,
    memory_bytes: u64,
) -> Result<(TextureHeader, u64), String> {
    options.validate()?;
    if output
        .extension()
        .is_none_or(|extension| extension != "reel-texture")
    {
        return Err("output must end in .reel-texture".into());
    }
    if output.exists() {
        return Err("output already exists; choose a new output path".into());
    }
    let mut reader = ImageReader::open(source)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)
    ) {
        return Err("original image must be PNG or JPEG".into());
    }
    let mut limits = image::Limits::no_limits();
    limits.max_alloc = Some(memory_bytes);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let dimensions = decoder.dimensions();
    let logical = [dimensions.0, dimensions.1];
    let content = scaled_content_size(logical, options.max_size, scale)?;
    let mut storage = content;
    for axis in &mut storage {
        *axis = if options.power_of_two {
            (*axis)
                .max(8)
                .checked_next_power_of_two()
                .ok_or("stored image dimensions overflow")?
        } else {
            axis.checked_add(3)
                .ok_or("stored image dimensions overflow")?
                / 4
                * 4
        };
    }
    // Pinned image 0.25.10: into_rgba32f holds at most the PNG16 source (8 B/px)
    // and float destination (16 B/px); apply_color_space then works in place.
    // Triangle resize keeps original float pixels, width*new_height float
    // intermediate, float result and a growing f32 weight vector (<=8 B/axis).
    // Quantization holds float content and padded RGBA8. BC fitting and quality
    // measurement retain the float reference, candidate blocks and RGBA scratch.
    let original_pixels = u128::from(logical[0]) * u128::from(logical[1]);
    let content_pixels = u128::from(content[0]) * u128::from(content[1]);
    let intermediate_pixels = u128::from(logical[0]) * u128::from(content[1]);
    let stored_pixels = u128::from(storage[0]) * u128::from(storage[1]);
    let cast = original_pixels * 24;
    let resize = if content != logical {
        (original_pixels + intermediate_pixels + content_pixels) * 16
            + u128::from(logical[0].max(logical[1])) * 8
    } else {
        0
    };
    let quantize = content_pixels * 16 + stored_pixels * 4;
    let encode = if !matches!(options.format, Format::Rgba8) {
        // The float reference and baseline blocks survive one candidate's fit
        // and decode.
        bc_workspace(content, storage)
    } else {
        0
    };
    let workspace = u64::try_from(cast.max(resize).max(quantize).max(encode) + (1 << 20))
        .map_err(|_| "image workspace size overflow")?;
    let pool = BudgetPool::new(
        BudgetCategory::Temporary,
        QueueLimits {
            max_requests: 1,
            max_bytes: memory_bytes,
        },
    );
    let _reservation = pool.reserve(workspace).map_err(|error| {
        format!(
            "texture preparation needs {workspace} workspace bytes; limit is {memory_bytes}: {}",
            error.message
        )
    })?;
    let pixels = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    prepare_decoded_scaled(pixels, output, options, scale).map(|result| (result, workspace))
}

/// Peak BC fitting, quality measurement and envelope storage after source decode.
pub(crate) fn bc_workspace(content: [u32; 2], storage: [u32; 2]) -> u128 {
    let content_pixels = u128::from(content[0]) * u128::from(content[1]);
    let stored_pixels = u128::from(storage[0]) * u128::from(storage[1]);
    content_pixels * 16 + stored_pixels * 6 + quality::scratch_bytes(content[0])
}

/// Encode already decoded artwork with the same filtering, block and publication
/// rules as `prepare`, without an intermediate image file.
pub(crate) fn prepare_decoded(
    pixels: DynamicImage,
    output: &Path,
    options: Options,
) -> Result<TextureHeader, String> {
    prepare_decoded_scaled(pixels, output, options, 1.)
}

fn prepare_decoded_scaled(
    pixels: DynamicImage,
    output: &Path,
    options: Options,
    scale: f64,
) -> Result<TextureHeader, String> {
    let (descriptor, linear) = prepare_linear(pixels, options, scale)?;
    let blocks = match descriptor.format {
        TextureFormat::Rgba8 => quantized_pixels(&linear, descriptor, options.dither).into_raw(),
        TextureFormat::Bc1Rgb | TextureFormat::Bc3Rgba => quality::encode(&linear, descriptor)?,
        _ => unreachable!("offline conversion selects RGBA8, BC1 or BC3"),
    };
    drop(linear);
    let header = TextureHeader::new(descriptor, TextureCodec::Raw, blocks.len() as u64)
        .map_err(|e| e.to_string())?;
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let target = parent.join(output.file_name().ok_or("output filename is missing")?);
    let published = reel_storage::publish(&target, false, |file| {
        file.write_all(&header.encode())?;
        file.write_all(&blocks)
    })
    .map_err(|e| e.to_string())?;
    if let Some(warning) = published.cleanup_warning {
        eprintln!("{warning}");
    }
    Ok(header)
}

fn content_size(original: [u32; 2], bounds: Option<[u32; 2]>) -> Result<[u32; 2], String> {
    let Some(bounds) = bounds else {
        return Ok(original);
    };
    if bounds.contains(&0) {
        return Err("maximum dimensions must be positive".into());
    }
    if original[0] <= bounds[0] && original[1] <= bounds[1] {
        return Ok(original);
    }
    let [width, height] = original.map(u64::from);
    let [max_width, max_height] = bounds.map(u64::from);
    Ok(if width * max_height > height * max_width {
        [bounds[0], (height * max_width / width).max(1) as u32]
    } else {
        [(width * max_height / height).max(1) as u32, bounds[1]]
    })
}

fn scaled_content_size(
    original: [u32; 2],
    bounds: Option<[u32; 2]>,
    scale: f64,
) -> Result<[u32; 2], String> {
    if scale < 1. {
        Ok(original.map(|axis| (f64::from(axis) * scale).floor().max(1.) as u32))
    } else {
        content_size(original, bounds)
    }
}

#[cfg(test)]
fn prepare_pixels(
    original: DynamicImage,
    options: Options,
    scale: f64,
) -> Result<(TextureDescriptor, RgbaImage), String> {
    let (descriptor, linear) = prepare_linear(original, options, scale)?;
    let pixels = quantized_pixels(&linear, descriptor, options.dither);
    Ok((descriptor, pixels))
}

fn prepare_linear(
    original: DynamicImage,
    options: Options,
    scale: f64,
) -> Result<(TextureDescriptor, image::Rgba32FImage), String> {
    options.validate()?;
    let logical_size = [original.width(), original.height()];
    if logical_size.contains(&0) {
        return Err("image dimensions must be positive".into());
    }
    let content_size = scaled_content_size(logical_size, options.max_size, scale)?;
    let mut original = DynamicImage::ImageRgba32F(original.into_rgba32f());
    original
        .convert_color_space(Cicp::SRGB_LINEAR, Default::default(), ColorType::Rgba32F)
        .map_err(|e| e.to_string())?;
    let mut linear = original.into_rgba32f();
    let opaque = linear.pixels().all(|pixel| pixel[3] == 1.);
    let format = match options.format {
        Format::Auto if opaque => TextureFormat::Bc1Rgb,
        Format::Auto | Format::Bc3 => TextureFormat::Bc3Rgba,
        Format::Bc1 if opaque => TextureFormat::Bc1Rgb,
        Format::Bc1 => {
            return Err("BC1 requires opaque artwork; use auto or bc3 to preserve alpha".into());
        }
        Format::Rgba8 => TextureFormat::Rgba8,
    };
    for pixel in linear.pixels_mut() {
        for channel in 0..3 {
            pixel[channel] *= pixel[3];
        }
    }
    if content_size != logical_size {
        linear = imageops::resize(
            &linear,
            content_size[0],
            content_size[1],
            imageops::FilterType::Triangle,
        );
    }
    let mut storage_size = content_size;
    for axis in &mut storage_size {
        *axis = if options.power_of_two {
            (*axis)
                .max(8)
                .checked_next_power_of_two()
                .ok_or("stored image dimensions overflow")?
        } else {
            axis.checked_add(format.block_size() - 1)
                .ok_or("stored image dimensions overflow")?
                / format.block_size()
                * format.block_size()
        };
    }
    let descriptor = TextureDescriptor {
        format,
        source_encoding: ColorEncoding::Srgb,
        layout: TextureLayout {
            logical_size,
            content_size,
            storage_size,
        },
    };
    descriptor.byte_len().map_err(|e| e.to_string())?;
    // RGB and alpha remain linear premultiplied when quantized. The decoder does
    // not interpret these bytes as an sRGB image or premultiply them a second time.
    Ok((descriptor, linear))
}

fn quantized_pixels(
    linear: &image::Rgba32FImage,
    descriptor: TextureDescriptor,
    dither: Dither,
) -> RgbaImage {
    let TextureLayout {
        content_size,
        storage_size,
        ..
    } = descriptor.layout;
    RgbaImage::from_fn(storage_size[0], storage_size[1], |x, y| {
        let coordinates = [x.min(content_size[0] - 1), y.min(content_size[1] - 1)];
        let pixel = linear.get_pixel(coordinates[0], coordinates[1]);
        image::Rgba(quantize(pixel.0, dither, coordinates))
    })
}

fn ordered_offset([x, y]: [u32; 2]) -> f32 {
    const RANKS: [[u8; 8]; 8] = [
        [0, 48, 12, 60, 3, 51, 15, 63],
        [32, 16, 44, 28, 35, 19, 47, 31],
        [8, 56, 4, 52, 11, 59, 7, 55],
        [40, 24, 36, 20, 43, 27, 39, 23],
        [2, 50, 14, 62, 1, 49, 13, 61],
        [34, 18, 46, 30, 33, 17, 45, 29],
        [10, 58, 6, 54, 9, 57, 5, 53],
        [42, 26, 38, 22, 41, 25, 37, 21],
    ];
    (f32::from(RANKS[(y % 8) as usize][(x % 8) as usize]) + 0.5) / 64. - 0.5
}

fn quantize(pixel: [f32; 4], dither: Dither, coordinates: [u32; 2]) -> [u8; 4] {
    let mut result = pixel.map(|value| (value.clamp(0., 1.) * 255.).round() as u8);
    if dither == Dither::Ordered {
        let offset = ordered_offset(coordinates);
        for channel in 0..3 {
            result[channel] =
                ((pixel[channel].clamp(0., 1.) * 255. + offset).round() as u8).min(result[3]);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_formats_publish_upload_ready_payloads() {
        let directory = tempfile::tempdir().unwrap();
        for (format, expected) in [
            (Format::Auto, TextureFormat::Bc3Rgba),
            (Format::Bc1, TextureFormat::Bc1Rgb),
            (Format::Bc3, TextureFormat::Bc3Rgba),
            (Format::Rgba8, TextureFormat::Rgba8),
        ] {
            let path = directory.path().join(format!("{format:?}.reel-texture"));
            let pixels = RgbaImage::from_fn(12, 9, |x, y| {
                let alpha = if matches!(format, Format::Bc1) {
                    255
                } else {
                    192
                };
                image::Rgba([(x * 17) as u8, (y * 23) as u8, 64, alpha])
            });
            prepare_decoded(
                pixels.into(),
                &path,
                Options {
                    format,
                    ..Default::default()
                },
            )
            .unwrap();
            let bytes = std::fs::read(path).unwrap();
            let (header, payload) = TextureHeader::parse(&bytes).unwrap();
            assert_eq!(header.codec(), TextureCodec::Raw);
            assert_eq!(header.descriptor().format, expected);
            assert_eq!(
                payload.len() as u64,
                header.descriptor().byte_len().unwrap()
            );
        }
    }

    #[test]
    fn ordered_dither_preserves_the_mean_of_subcode_gradients() {
        let offsets: Vec<_> = (0..8)
            .flat_map(|y| (0..8).map(move |x| ordered_offset([x, y])))
            .collect();
        assert_eq!(offsets.iter().sum::<f32>(), 0.);
        assert!(offsets.iter().all(|offset| offset.abs() <= 63. / 128.));
        let mut ordered_levels = std::collections::BTreeSet::new();
        let mut rounded_levels = std::collections::BTreeSet::new();
        for step in 0..=64 {
            let value = 32. + step as f32 / 64.;
            let pixel = [value / 255., value / 255., value / 255., 1.];
            let sum: u32 = (0..8)
                .flat_map(|y| (0..8).map(move |x| quantize(pixel, Dither::Ordered, [x, y])))
                .map(|pixel| {
                    assert_eq!(pixel[0], pixel[1]);
                    assert_eq!(pixel[0], pixel[2]);
                    assert_eq!(pixel[3], 255);
                    u32::from(pixel[0])
                })
                .sum();
            assert!((sum as f32 / 64. - value).abs() <= 1. / 128.);
            ordered_levels.insert(sum);
            rounded_levels.insert(quantize(pixel, Dither::None, [0, 0])[0]);
        }
        assert_eq!(ordered_levels.len(), 65);
        assert_eq!(rounded_levels.len(), 2);
    }

    #[test]
    fn ordered_dither_keeps_alpha_padding_and_payload_extent() {
        let original = RgbaImage::from_fn(17, 19, |x, y| {
            image::Rgba([96, 96, 96, (x + 17 * y).min(255) as u8])
        });
        let options = Options {
            power_of_two: true,
            format: Format::Rgba8,
            ..Default::default()
        };
        let (rounded_descriptor, rounded) =
            prepare_pixels(original.clone().into(), options, 1.).unwrap();
        let ordered_options = Options {
            dither: Dither::Ordered,
            ..options
        };
        let (descriptor, ordered) =
            prepare_pixels(original.clone().into(), ordered_options, 1.).unwrap();
        assert_eq!(
            descriptor.byte_len().unwrap(),
            rounded_descriptor.byte_len().unwrap()
        );
        assert_eq!(ordered.as_raw().len(), rounded.as_raw().len());
        assert_ne!(ordered, rounded);
        for (x, y, pixel) in ordered.enumerate_pixels() {
            assert_eq!(pixel[3], rounded.get_pixel(x, y)[3]);
            assert_eq!(pixel[0], pixel[1]);
            assert_eq!(pixel[0], pixel[2]);
            assert!(pixel[0] <= pixel[3]);
            assert_eq!(pixel, ordered.get_pixel(x.min(16), y.min(18)));
        }
        for alpha in 0..=255 {
            let a = f32::from(alpha) / 255.;
            for y in 0..8 {
                for x in 0..8 {
                    let pixel = quantize([a, a / 4., 0., a], Dither::Ordered, [x, y]);
                    assert_eq!(pixel[3], alpha);
                    assert_eq!(pixel[0], alpha);
                    assert!(pixel[1] <= alpha);
                    assert_eq!(pixel[2], 0);
                }
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("rounded.reel-texture");
        let second = directory.path().join("ordered.reel-texture");
        let a = prepare_decoded(original.clone().into(), &first, options).unwrap();
        let b = prepare_decoded(original.into(), &second, ordered_options).unwrap();
        assert_eq!(a.payload_len(), b.payload_len());
        assert_eq!(
            std::fs::metadata(first).unwrap().len(),
            std::fs::metadata(second).unwrap().len()
        );
    }

    #[test]
    fn ordered_dither_rejects_block_formats_before_reading_source() {
        for format in [Format::Auto, Format::Bc1, Format::Bc3] {
            let error = prepare(
                Path::new("unused.png"),
                Path::new("unused.reel-texture"),
                Options {
                    format,
                    dither: Dither::Ordered,
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert!(error.contains("requires format rgba8"), "{error}");
        }
    }

    #[test]
    fn scaled_texture_preserves_coordinates_and_filters_premultiplied_colors() {
        let original = RgbaImage::from_fn(2, 1, |x, _| {
            image::Rgba(if x == 0 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 0]
            })
        });
        let (descriptor, pixels) = prepare_pixels(
            original.into(),
            Options {
                max_size: Some([1, 1]),
                power_of_two: true,
                format: Format::Rgba8,
                dither: Dither::None,
            },
            1.,
        )
        .unwrap();
        assert_eq!(descriptor.layout.logical_size, [2, 1]);
        assert_eq!(descriptor.layout.content_size, [1, 1]);
        assert_eq!(descriptor.layout.storage_size, [8, 8]);
        assert!(pixels.pixels().all(|pixel| pixel.0 == [128, 0, 0, 128]));
        assert_eq!(
            content_size([1920, 1080], Some([960, 544])).unwrap(),
            [960, 540]
        );
        assert_eq!(content_size([32, 16], Some([960, 544])).unwrap(), [32, 16]);
        assert_eq!(scaled_content_size([13, 7], None, 0.5).unwrap(), [6, 3]);
    }
}
