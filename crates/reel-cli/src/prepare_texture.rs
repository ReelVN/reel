//! Offline image conversion into portable linear-premultiplied texture blocks.
use image::{ColorType, DynamicImage, ImageReader, RgbaImage, imageops, metadata::Cicp};
use reel_core::texture::{
    ColorEncoding, TextureCodec, TextureDescriptor, TextureFormat, TextureHeader, TextureLayout,
};
use std::path::Path;

/// `Auto` uses BC1 for opaque images and BC3 for images with transparency.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum Format {
    #[default]
    Auto,
    Bc1,
    Bc3,
    /// Uncompressed linear-premultiplied RGBA8, useful for fine UI art and masks.
    Rgba8,
}

/// Conversion changes stored texels while preserving the original author coordinates.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Fit inside this box without enlarging the original image.
    pub max_size: Option<[u32; 2]>,
    /// Pad each stored axis to a power of two, with a minimum of eight texels.
    pub power_of_two: bool,
    pub format: Format,
}

/// Prepare PNG/JPEG artwork and atomically publish a new `.reel-texture` file.
/// Filtering uses linear-premultiplied colors; padding repeats the content edge.
/// BC blocks remain compressed at runtime. An additional lossless BC envelope is
/// used only when it reduces file size. Existing output files are preserved.
pub fn prepare(source: &Path, output: &Path, options: Options) -> Result<TextureHeader, String> {
    if output
        .extension()
        .is_none_or(|extension| extension != "reel-texture")
    {
        return Err("output must end in .reel-texture".into());
    }
    if output.exists() {
        return Err("output already exists; choose a new output path".into());
    }
    let reader = ImageReader::open(source)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)
    ) {
        return Err("original image must be PNG or JPEG".into());
    }
    let pixels = reader.decode().map_err(|e| e.to_string())?;
    let (descriptor, pixels) = prepare_pixels(pixels, options)?;
    let [width, height] = descriptor.layout.storage_size;
    let blocks = match descriptor.format {
        TextureFormat::Rgba8 => pixels.into_raw(),
        TextureFormat::Bc1Rgb | TextureFormat::Bc3Rgba => rgbcx::encode(
            width,
            height,
            pixels.as_raw(),
            if descriptor.format == TextureFormat::Bc1Rgb {
                rgbcx::Format::Bc1
            } else {
                rgbcx::Format::Bc3
            },
            rgbcx::Quality::Balanced,
        )
        .map_err(|e| e.to_string())?,
        _ => unreachable!("offline conversion selects RGBA8, BC1 or BC3"),
    };
    let (codec, payload) = if descriptor.format.is_compressed() {
        let compressed = bc_crunch::compress(
            width,
            height,
            if descriptor.format == TextureFormat::Bc1Rgb {
                bc_crunch::Format::Bc1
            } else {
                bc_crunch::Format::Bc3
            },
            &blocks,
        )
        .map_err(|e| e.to_string())?;
        if compressed.len() < blocks.len() {
            (TextureCodec::BcCrunch, compressed)
        } else {
            (TextureCodec::Raw, blocks)
        }
    } else {
        (TextureCodec::Raw, blocks)
    };
    let header =
        TextureHeader::new(descriptor, codec, payload.len() as u64).map_err(|e| e.to_string())?;
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let target = parent.join(output.file_name().ok_or("output filename is missing")?);
    let published = reel_storage::publish(&target, false, |file| {
        file.write_all(&header.encode())?;
        file.write_all(&payload)
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

fn prepare_pixels(
    mut original: DynamicImage,
    options: Options,
) -> Result<(TextureDescriptor, RgbaImage), String> {
    let logical_size = [original.width(), original.height()];
    if logical_size.contains(&0) {
        return Err("image dimensions must be positive".into());
    }
    let content_size = content_size(logical_size, options.max_size)?;
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
    let pixels = RgbaImage::from_fn(storage_size[0], storage_size[1], |x, y| {
        let pixel = linear.get_pixel(x.min(content_size[0] - 1), y.min(content_size[1] - 1));
        image::Rgba(
            pixel
                .0
                .map(|value| (value.clamp(0., 1.) * 255.).round() as u8),
        )
    });
    Ok((descriptor, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            },
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
    }
}
