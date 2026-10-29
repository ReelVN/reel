//! Generate stored mip levels before quantization, preserving one base UV mapping.
use super::*;
pub(crate) fn encode(
    mut linear: image::Rgba32FImage,
    descriptor: TextureDescriptor,
    dither: Dither,
) -> Result<Vec<u8>, String> {
    let capacity = descriptor.byte_len().map_err(|e| e.to_string())? as usize;
    let direct = descriptor.mip_count == 1
        && descriptor.storage_layout == reel_core::texture::TextureStorageLayout::Linear;
    let mut bytes = Vec::with_capacity(if direct { 0 } else { capacity });
    if descriptor.mip_count > 1 {
        let size = descriptor.layout.storage_size;
        linear = image::Rgba32FImage::from_fn(size[0], size[1], |x, y| {
            *linear.get_pixel(x.min(linear.width() - 1), y.min(linear.height() - 1))
        });
    }
    for level in 0..descriptor.mip_count {
        let size = descriptor.mip_size(level);
        if level > 0 {
            linear = imageops::resize(&linear, size[0], size[1], imageops::FilterType::Triangle);
        }
        // BC encoders accept whole blocks, including the repeated edge of tiny mip levels.
        let padded = size.map(|n| n.next_multiple_of(descriptor.format.block_size()));
        let layer = TextureDescriptor {
            mip_count: 1,
            storage_layout: Default::default(),
            layout: TextureLayout {
                logical_size: size,
                content_size: if descriptor.mip_count == 1 {
                    descriptor.layout.content_size
                } else {
                    size
                },
                storage_size: padded,
            },
            ..descriptor
        };
        let blocks = match descriptor.format {
            TextureFormat::Rgba8 => quantized_pixels(&linear, layer, dither).into_raw(),
            TextureFormat::Bc1Rgb | TextureFormat::Bc3Rgba => quality::encode(&linear, layer)?,
            _ => unreachable!("offline format"),
        };
        if direct {
            return Ok(blocks);
        }
        if descriptor.storage_layout == reel_core::texture::TextureStorageLayout::GxmBcMorton {
            let start = bytes.len();
            bytes.resize(start + blocks.len(), 0);
            let n = descriptor.format.block_bytes() as usize;
            for y in 0..size[1].div_ceil(4) {
                for x in 0..size[0].div_ceil(4) {
                    let src = (y as usize * size[0].div_ceil(4) as usize + x as usize) * n;
                    let dst = start + descriptor.storage_layout.block_offset(size, n, x, y);
                    bytes[dst..dst + n].copy_from_slice(&blocks[src..src + n]);
                }
            }
        } else {
            bytes.extend_from_slice(&blocks);
        }
    }
    debug_assert_eq!(bytes.len(), capacity);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chain_preserves_premultiplied_edges_and_morton_tail_extents() {
        for format in [Format::Rgba8, Format::Bc3] {
            for gxm_layout in [false, true] {
                let image = DynamicImage::ImageRgba8(RgbaImage::from_fn(13, 7, |_, _| {
                    image::Rgba([255, 0, 0, 128])
                }));
                let options = Options {
                    format,
                    mipmaps: true,
                    gxm_layout,
                    ..Default::default()
                };
                let (d, linear) = prepare_linear(image, options, 1.).unwrap();
                let bytes = encode(linear, d, Dither::None).unwrap();
                d.validate(bytes.len()).unwrap();
                assert_eq!(d.layout.logical_size, [13, 7]);
                assert_eq!(d.layout.content_size, [13, 7]);
                assert_eq!(d.mip_size(d.mip_count - 1), [1, 1]);
                if matches!(format, Format::Rgba8) {
                    for pixel in bytes.as_chunks::<4>().0 {
                        assert_eq!(pixel, &[128, 0, 0, 128]);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[test]
fn transparent_rgb_does_not_bleed_into_coarser_levels() {
    let image = DynamicImage::ImageRgba8(RgbaImage::from_fn(2, 1, |x, _| {
        image::Rgba(if x == 0 {
            [255, 0, 0, 255]
        } else {
            [0, 0, 255, 0]
        })
    }));
    let (d, linear) = prepare_linear(
        image,
        Options {
            format: Format::Rgba8,
            mipmaps: true,
            ..Default::default()
        },
        1.,
    )
    .unwrap();
    let bytes = encode(linear, d, Dither::None).unwrap();
    assert_eq!(
        &bytes[d.mip_offset(1).unwrap() as usize..],
        &[128, 0, 0, 128]
    );
}
