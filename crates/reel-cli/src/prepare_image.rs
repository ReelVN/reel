//! Offline preparation of original images; runtime reads only individual tiles.
use image::{ImageReader, RgbaImage, imageops};
use reel_core::prepared_image::{PreparedImage, TileEncoding};
use std::{fs, io::Write, path::Path};

/// Offline tile file encoding. Compressed textures are selected by default.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum Encoding {
    /// Preserve exact sRGB PNG samples.
    Png,
    /// Encode opaque tiles as BC1 and translucent tiles as BC3.
    #[default]
    ReelTexture,
}
/// Prepare and publish a complete v1 pyramid with an explicit tile encoding.
pub fn prepare(
    source: &Path,
    output: &Path,
    tile_edge: u32,
    encoding: Encoding,
) -> Result<PreparedImage, String> {
    if output
        .extension()
        .is_none_or(|extension| extension != "reel-image")
    {
        return Err("output must end in .reel-image".into());
    }
    let output_str = output.to_str().ok_or("output path must be UTF-8")?;
    let directory = format!("{output_str}.tiles");
    if output.exists() || Path::new(&directory).exists() {
        return Err("output or tile directory already exists; choose a new output path".into());
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
    let mut pixels = reader.decode().map_err(|e| e.to_string())?.into_rgba8();
    let manifest = PreparedImage::new(
        pixels.width(),
        pixels.height(),
        tile_edge,
        match encoding {
            Encoding::Png => TileEncoding::Png,
            Encoding::ReelTexture => TileEncoding::ReelTexture,
        },
    )?;
    fs::create_dir(&directory).map_err(|e| e.to_string())?;
    for level in 0..manifest.levels() {
        fs::create_dir(Path::new(&directory).join(level.to_string())).map_err(|e| e.to_string())?;
        for y in 0..pixels.height().div_ceil(tile_edge) {
            for x in 0..pixels.width().div_ceil(tile_edge) {
                let [width, height] = manifest.tile_dimensions(level, x, y).unwrap();
                let tile = RgbaImage::from_fn(width, height, |tx, ty| {
                    let sx = (i64::from(x) * i64::from(tile_edge) + i64::from(tx) - 1)
                        .clamp(0, i64::from(pixels.width()) - 1)
                        as u32;
                    let sy = (i64::from(y) * i64::from(tile_edge) + i64::from(ty) - 1)
                        .clamp(0, i64::from(pixels.height()) - 1)
                        as u32;
                    *pixels.get_pixel(sx, sy)
                });
                let path = manifest.tile_source(output_str, level, x, y);
                match encoding {
                    Encoding::Png => tile.save(&path).map_err(|e| e.to_string())?,
                    Encoding::ReelTexture => {
                        crate::prepare_texture::prepare_decoded(
                            tile.into(),
                            Path::new(&path),
                            Default::default(),
                        )?;
                    }
                }
            }
        }
        if level + 1 < manifest.levels() {
            let [width, height] = manifest.dimensions(level + 1);
            pixels = downsample(pixels, width, height)?;
        }
    }
    // Publish last. A partial tile directory can never be mistaken for a ready image.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .and_then(|mut file| file.write_all(&manifest.encode()))
        .map_err(|e| e.to_string())?;
    Ok(manifest)
}

fn downsample(pixels: RgbaImage, width: u32, height: u32) -> Result<RgbaImage, String> {
    use image::{ColorType, DynamicImage, metadata::Cicp};
    let mut linear = DynamicImage::ImageRgba8(pixels);
    linear
        .convert_color_space(Cicp::SRGB_LINEAR, Default::default(), ColorType::Rgba32F)
        .map_err(|e| e.to_string())?;
    let mut linear = linear.into_rgba32f();
    for pixel in linear.pixels_mut() {
        for channel in 0..3 {
            pixel[channel] *= pixel[3];
        }
    }
    let mut reduced = imageops::resize(&linear, width, height, imageops::FilterType::Triangle);
    drop(linear);
    for pixel in reduced.pixels_mut() {
        if pixel[3] > 0. {
            for channel in 0..3 {
                pixel[channel] /= pixel[3];
            }
        }
    }
    reduced
        .set_color_space(Cicp::SRGB_LINEAR)
        .map_err(|e| e.to_string())?;
    let mut output = DynamicImage::ImageRgba32F(reduced);
    output
        .convert_color_space(Cicp::SRGB, Default::default(), ColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(output.into_rgba8())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compressed_tiles_preserve_gutters_and_store_native_bc_payloads() {
        use reel_core::texture::{ColorEncoding, TextureFormat, TextureHeader};
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/prepared-image-tool")
            .join(format!(
                "bc-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(&root).unwrap();
        let original = root.join("original.png");
        let output = root.join("blocks.reel-image");
        RgbaImage::from_fn(17, 9, |x, _| {
            image::Rgba([90, 60, 30, if x < 9 { 255 } else { 128 }])
        })
        .save(&original)
        .unwrap();
        let manifest = prepare(&original, &output, 8, Encoding::ReelTexture).unwrap();
        assert_eq!(manifest.tile_encoding, TileEncoding::ReelTexture);
        assert_eq!(
            PreparedImage::decode(&fs::read(&output).unwrap()),
            Ok(manifest)
        );
        let mut formats = Vec::new();
        for level in 0..manifest.levels() {
            let [width, height] = manifest.dimensions(level);
            for y in 0..height.div_ceil(8) {
                for x in 0..width.div_ceil(8) {
                    let path = manifest.tile_source(output.to_str().unwrap(), level, x, y);
                    assert!(path.ends_with(".reel-texture"));
                    let bytes = fs::read(path).unwrap();
                    let (header, _) = TextureHeader::parse(&bytes).unwrap();
                    let descriptor = header.descriptor();
                    assert_eq!(
                        descriptor.layout.logical_size,
                        manifest.tile_dimensions(level, x, y).unwrap()
                    );
                    assert_eq!(
                        descriptor.layout.content_size,
                        descriptor.layout.logical_size
                    );
                    assert_eq!(descriptor.source_encoding, ColorEncoding::Srgb);
                    assert_eq!(
                        descriptor.layout.storage_size,
                        descriptor
                            .layout
                            .content_size
                            .map(|axis| axis.div_ceil(4) * 4)
                    );
                    formats.push(descriptor.format);
                }
            }
        }
        assert!(formats.contains(&TextureFormat::Bc1Rgb));
        assert!(formats.contains(&TextureFormat::Bc3Rgba));
    }
    #[test]
    fn pyramid_filter_uses_linear_premultiplied_colors() {
        let pixels = RgbaImage::from_fn(2, 1, |x, _| {
            if x == 0 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 0])
            }
        });
        let reduced = downsample(pixels, 1, 1).unwrap();
        let pixel = reduced.get_pixel(0, 0);
        assert!(
            pixel[0] > 250 && pixel[2] < 2,
            "transparent blue must not bleed: {pixel:?}"
        );
        assert!((127..=128).contains(&pixel[3]));
    }
    #[test]
    fn original_larger_than_texture_limit_prepares_exact_neighbor_gutters() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/prepared-image-tool");
        fs::create_dir_all(&root).unwrap();
        let unique = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let original = root.join(format!("original-{unique}.png"));
        let output = root.join(format!("prepared-{unique}.reel-image"));
        let pixels = RgbaImage::from_fn(40001, 17, |x, y| {
            image::Rgba([(x % 251) as u8, (y % 251) as u8, 117, 255])
        });
        pixels.save(&original).unwrap();
        let started = std::time::Instant::now();
        let manifest = prepare(&original, &output, 512, Encoding::Png).unwrap();
        assert_eq!(
            PreparedImage::decode(&fs::read(&output).unwrap()).unwrap(),
            manifest
        );
        let a = image::open(manifest.tile_source(output.to_str().unwrap(), 0, 0, 0))
            .unwrap()
            .into_rgba8();
        let b = image::open(manifest.tile_source(output.to_str().unwrap(), 0, 1, 0))
            .unwrap()
            .into_rgba8();
        assert_eq!(a.dimensions(), (514, 19));
        for y in 0..19 {
            assert_eq!(a.get_pixel(512, y), b.get_pixel(0, y));
            assert_eq!(a.get_pixel(513, y), b.get_pixel(1, y));
        }
        let last = image::open(manifest.tile_source(
            output.to_str().unwrap(),
            manifest.levels() - 1,
            0,
            0,
        ))
        .unwrap();
        assert_eq!((last.width(), last.height()), (3, 3));
        assert!(
            prepare(&original, &output, 512, Encoding::Png)
                .unwrap_err()
                .contains("already exists")
        );
        eprintln!(
            "prepared original=40001x17 exceeds 32768 texture dimension; levels={} elapsed={:?} output={}",
            manifest.levels(),
            started.elapsed(),
            output.display()
        );
    }
}
