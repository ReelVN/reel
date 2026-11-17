//! Offline preparation of original images; runtime reads only individual tiles.
use image::RgbaImage;
use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
use reel_core::prepared_image::{PreparedImage, TileEncoding};
use reel_ui::peniko::color::{ColorSpace, Srgb};
use std::{fs, io::Write, path::Path, sync::LazyLock};

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
    prepare_bounded(
        source,
        output,
        tile_edge,
        encoding,
        0,
        Default::default(),
        crate::resource_conversion::MEMORY_BYTES,
    )
    .map(|(manifest, _)| manifest)
}

/// Prepare stored pyramid levels under one decoded-pixel and codec workspace
/// reservation. Skipped levels retain the original author coordinates. Returns
/// the manifest and reserved peak bytes; tile texture options control padding
/// and encoding through the ordinary texture converter.
pub fn prepare_bounded(
    source: &Path,
    output: &Path,
    tile_edge: u32,
    encoding: Encoding,
    first_level: u32,
    texture_options: crate::prepare_texture::Options,
    memory_bytes: u64,
) -> Result<(PreparedImage, u64), String> {
    if texture_options.mipmaps {
        return Err(
            "mipmaps belongs to texture conversion; tiled images already own a level pyramid"
                .into(),
        );
    }
    if texture_options.dither != crate::prepare_texture::Dither::None {
        return Err(
            "ordered dithering is supported by texture conversion, not tiled images".into(),
        );
    }
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
    let (decoder, codec_workspace) = reel_player::artwork::open(source, memory_bytes)?;
    let dimensions = decoder.dimensions();
    let manifest = PreparedImage::new(
        dimensions.0,
        dimensions.1,
        tile_edge,
        match encoding {
            Encoding::Png => TileEncoding::Png,
            Encoding::ReelTexture => TileEncoding::ReelTexture,
        },
    )?
    .with_first_level(first_level)?;
    let workspace = workspace(manifest, decoder.total_bytes(), encoding, texture_options)?
        .max(decoder.total_bytes() + codec_workspace);
    let pool = BudgetPool::new(
        BudgetCategory::Temporary,
        QueueLimits {
            max_requests: 1,
            max_bytes: memory_bytes,
        },
    );
    let _reservation = pool.reserve(workspace).map_err(|error| format!(
        "image pyramid preparation needs {workspace} workspace bytes; limit is {memory_bytes}: {}",
        error.message,
    ))?;
    let mut pixels = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| e.to_string())?
        .into_rgba8();
    fs::create_dir(&directory).map_err(|e| e.to_string())?;
    for level in 0..manifest.levels() {
        if level >= first_level {
            fs::create_dir(Path::new(&directory).join(level.to_string()))
                .map_err(|e| e.to_string())?;
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
                    let name = manifest.tile_source(
                        output.file_name().and_then(|name| name.to_str()).unwrap(),
                        level,
                        x,
                        y,
                    );
                    let path = output.parent().unwrap().join(name);
                    match encoding {
                        Encoding::Png => tile.save(&path).map_err(|e| e.to_string())?,
                        Encoding::ReelTexture => {
                            crate::prepare_texture::prepare_decoded(
                                tile.into(),
                                &path,
                                texture_options,
                            )?;
                        }
                    }
                }
            }
        }
        if level + 1 < manifest.levels() {
            let [width, height] = manifest.dimensions(level + 1);
            pixels = downsample(pixels, width, height);
        }
    }
    // Publish last. A partial tile directory can never be mistaken for a ready image.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .and_then(|mut file| file.write_all(&manifest.encode()))
        .map_err(|e| e.to_string())?;
    Ok((manifest, workspace))
}

fn workspace(
    manifest: PreparedImage,
    decoded_bytes: u64,
    encoding: Encoding,
    options: crate::prepare_texture::Options,
) -> Result<u64, String> {
    let original = u128::from(manifest.width) * u128::from(manifest.height) * 4;
    let quarter = if manifest.levels() > 1 {
        let [width, height] = manifest.dimensions(1);
        u128::from(width) * u128::from(height) * 4
    } else {
        0
    };
    let [width, height] = manifest
        .dimensions(manifest.first_level)
        .map(|axis| axis.min(manifest.tile_edge) + 2);
    let tile_pixels = u128::from(width) * u128::from(height);
    let tile_workspace = match encoding {
        Encoding::Png => tile_pixels * 16,
        Encoding::ReelTexture => {
            let content = [width, height];
            let [width, height] = content.map(|axis| {
                if options.power_of_two || options.gxm_layout {
                    axis.max(8).checked_next_power_of_two()
                } else {
                    axis.checked_add(3).map(|value| value / 4 * 4)
                }
            });
            let storage = [
                width.ok_or("tile storage size overflow")?,
                height.ok_or("tile storage size overflow")?,
            ];
            let stored_pixels = u128::from(storage[0]) * u128::from(storage[1]);
            // Pinned image/texture converter: source RGBA8 + linear RGBA32F
            // conversion, float-to-padded RGBA8, then bounded BC/envelope vectors.
            (tile_pixels * 24)
                .max(tile_pixels * 16 + stored_pixels * 4)
                .max(
                    if matches!(options.format, crate::prepare_texture::Format::Rgba8) {
                        0
                    } else {
                        crate::prepare_texture::bc_workspace(content, storage)
                    },
                )
                .max(stored_pixels * 16)
        }
    };
    // Decode/quantization can overlap native decoded bytes and RGBA8. Each
    // pyramid step holds only its source and RGBA8 destination. The small tile
    // codec reservation also covers PNG/JPEG row state and the transfer LUTs.
    u64::try_from(
        (u128::from(decoded_bytes) + original).max(original + quarter) + tile_workspace + (1 << 20),
    )
    .map_err(|_| "image pyramid workspace size overflow".into())
}

static SRGB_TO_LINEAR: LazyLock<[f64; 256]> = LazyLock::new(|| {
    std::array::from_fn(|index| f64::from(Srgb::to_linear_srgb([index as f32 / 255.; 3])[0]))
});

static LINEAR_TO_SRGB: LazyLock<Box<[u8]>> = LazyLock::new(|| {
    (0..=65535)
        .map(|index| {
            let encoded = Srgb::from_linear_srgb([index as f32 / 65535.; 3])[0];
            (encoded * 255.).round() as u8
        })
        .collect::<Vec<_>>()
        .into_boxed_slice()
});

fn downsample(pixels: RgbaImage, width: u32, height: u32) -> RgbaImage {
    let source_width = u64::from(pixels.width());
    let source_height = u64::from(pixels.height());
    let target_width = u64::from(width);
    let target_height = u64::from(height);
    let total_weight = (source_width * source_height) as f64;
    let decode = &*SRGB_TO_LINEAR;
    let encode = &*LINEAR_TO_SRGB;
    // Exact integer overlap lengths avoid losing odd edge pixels. For the
    // ordinary even pyramid step each output visits four source pixels.
    RgbaImage::from_fn(width, height, |x, y| {
        let left = u64::from(x) * source_width;
        let top = u64::from(y) * source_height;
        let right = left + source_width;
        let bottom = top + source_height;
        let mut sum = [0.; 4];
        for sy in top / target_height..bottom.div_ceil(target_height) {
            let dy = bottom.min((sy + 1) * target_height) - top.max(sy * target_height);
            for sx in left / target_width..right.div_ceil(target_width) {
                let dx = right.min((sx + 1) * target_width) - left.max(sx * target_width);
                let pixel = pixels.get_pixel(sx as u32, sy as u32);
                let weight = (dx * dy) as f64 * f64::from(pixel[3]) / 255.;
                for channel in 0..3 {
                    sum[channel] += decode[usize::from(pixel[channel])] * weight;
                }
                sum[3] += weight;
            }
        }
        let mut pixel = [0; 4];
        if sum[3] > 0. {
            for channel in 0..3 {
                let index = ((sum[channel] / sum[3]).clamp(0., 1.) * 65535.).round() as usize;
                pixel[channel] = encode[index];
            }
        }
        pixel[3] = (sum[3] / total_weight * 255.).round() as u8;
        image::Rgba(pixel)
    })
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
        let reduced = downsample(pixels, 1, 1);
        let pixel = reduced.get_pixel(0, 0);
        assert!(
            pixel[0] > 250 && pixel[2] < 2,
            "transparent blue must not bleed: {pixel:?}"
        );
        assert!((127..=128).contains(&pixel[3]));
    }
    #[test]
    fn pyramid_filter_keeps_odd_edge_area_and_linear_light() {
        let pixels = RgbaImage::from_fn(3, 3, |x, y| {
            let value = if x == 2 && y == 2 { 255 } else { 0 };
            image::Rgba([value, value, value, 255])
        });
        let reduced = downsample(pixels, 2, 2);
        assert_eq!(reduced.get_pixel(0, 0).0, [0, 0, 0, 255]);
        assert_eq!(reduced.get_pixel(1, 0).0, [0, 0, 0, 255]);
        assert_eq!(reduced.get_pixel(0, 1).0, [0, 0, 0, 255]);
        assert_eq!(reduced.get_pixel(1, 1).0, [178, 178, 178, 255]);
        let pixels = RgbaImage::from_fn(3, 1, |x, _| {
            image::Rgba(if x == 1 {
                [0, 255, 0, 0]
            } else {
                [17, 65, 199, 255]
            })
        });
        let reduced = downsample(pixels, 2, 1);
        assert!(reduced.pixels().all(|pixel| pixel.0 == [17, 65, 199, 170]));
        let pixels = RgbaImage::from_fn(2, 2, |x, _| {
            image::Rgba(if x == 0 {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            })
        });
        assert_eq!(
            downsample(pixels, 1, 1).get_pixel(0, 0).0,
            [188, 188, 188, 255]
        );
    }

    #[test]
    fn tall_atlas_workspace_fits_half_gib_without_float_source_storage() {
        let manifest = PreparedImage::new(1920, 29616, 510, TileEncoding::ReelTexture)
            .unwrap()
            .with_first_level(1)
            .unwrap();
        let options = crate::prepare_texture::Options {
            gxm_layout: false,
            mipmaps: false,
            power_of_two: true,
            ..Default::default()
        };
        let decoded = u64::from(manifest.width) * u64::from(manifest.height) * 4;
        let bytes = workspace(manifest, decoded, Encoding::ReelTexture, options).unwrap();
        assert_eq!(bytes, decoded * 2 + 512 * 512 * 24 + (1 << 20));
        assert!(bytes <= 512 << 20);
    }

    #[test]
    fn bounded_pyramid_skips_levels_preserves_tile_size_and_checks_budget_before_decode() {
        use reel_core::texture::{TextureFormat, TextureHeader};
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/prepared-image-tool");
        fs::create_dir_all(&root).unwrap();
        let temp = tempfile::tempdir_in(root).unwrap();
        let original = temp.path().join("original.png");
        let output = temp.path().join("atlas.reel-image");
        RgbaImage::from_fn(17, 9, |x, y| {
            image::Rgba([(x * 7) as u8, (y * 11) as u8, 40, 255])
        })
        .save(&original)
        .unwrap();
        let options = crate::prepare_texture::Options {
            gxm_layout: false,
            mipmaps: false,
            power_of_two: true,
            format: crate::prepare_texture::Format::Rgba8,
            max_size: None,
            dither: crate::prepare_texture::Dither::None,
        };
        let expected = PreparedImage::new(17, 9, 6, TileEncoding::ReelTexture)
            .unwrap()
            .with_first_level(1)
            .unwrap();
        let bytes = workspace(expected, 17 * 9 * 4, Encoding::ReelTexture, options).unwrap();
        let error = prepare_bounded(
            &original,
            &output,
            6,
            Encoding::ReelTexture,
            1,
            options,
            bytes - 1,
        )
        .unwrap_err();
        assert!(error.contains(&format!("needs {bytes} workspace bytes")));
        assert!(!output.exists());
        assert!(!Path::new(&format!("{}.tiles", output.display())).exists());
        let (manifest, reserved) = prepare_bounded(
            &original,
            &output,
            6,
            Encoding::ReelTexture,
            1,
            options,
            bytes,
        )
        .unwrap();
        assert_eq!(reserved, bytes);
        assert_eq!(manifest, expected);
        assert_eq!(fs::read(&output).unwrap().len(), 32);
        assert!(!Path::new(&format!("{}.tiles/0", output.display())).exists());
        for level in manifest.first_level..manifest.levels() {
            let [width, height] = manifest.dimensions(level);
            for y in 0..height.div_ceil(manifest.tile_edge) {
                for x in 0..width.div_ceil(manifest.tile_edge) {
                    let bytes =
                        fs::read(manifest.tile_source(output.to_str().unwrap(), level, x, y))
                            .unwrap();
                    let (header, _) = TextureHeader::parse(&bytes).unwrap();
                    let descriptor = header.descriptor();
                    assert_eq!(descriptor.format, TextureFormat::Rgba8);
                    assert_eq!(
                        descriptor.layout.logical_size,
                        manifest.tile_dimensions(level, x, y).unwrap()
                    );
                    assert_eq!(
                        descriptor.layout.content_size,
                        descriptor.layout.logical_size
                    );
                    assert_eq!(
                        descriptor.layout.storage_size,
                        descriptor
                            .layout
                            .content_size
                            .map(|axis| axis.max(8).next_power_of_two())
                    );
                }
            }
        }
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
