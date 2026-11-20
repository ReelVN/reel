//! Build Vita's linear P8 index plane and a linear-premultiplied RGBA palette.
use super::{Dither, quantize};
use image::Rgba32FImage;
use reel_core::texture::TextureDescriptor;
use std::collections::{BTreeMap, btree_map::Entry};

const MAX_QUANTIZED_PIXELS: u128 = 1 << 20;

/// Additional preparation memory alongside the resized float image.
pub(crate) fn workspace_bytes(content: [u32; 2], storage: [u32; 2], allow_lossy: bool) -> u128 {
    let pixels = u128::from(content[0]) * u128::from(content[1]);
    let payload = (u128::from(storage[0]) * u128::from(storage[1])).next_multiple_of(64)
        + TextureDescriptor::PALETTE_BYTES as u128;
    // The exact dictionary holds at most 256 entries. The pinned single-thread
    // quantizer holds 4-byte input and 16-byte color caches, histogram tables,
    // remapping and contrast buffers. Reserve their worst simultaneous extent.
    let quantizer = if allow_lossy && pixels <= MAX_QUANTIZED_PIXELS {
        pixels * 128 + (4 << 20)
    } else {
        0
    };
    payload + (16 << 10) + quantizer
}

pub(crate) fn encode(
    linear: &Rgba32FImage,
    descriptor: TextureDescriptor,
    allow_lossy: bool,
) -> Result<Option<Vec<u8>>, String> {
    let [width, height] = descriptor.layout.content_size.map(|axis| axis as usize);
    let [stride, stored_height] = descriptor.layout.storage_size.map(|axis| axis as usize);
    debug_assert_eq!((width as u32, height as u32), linear.dimensions());
    let bytes = descriptor.byte_len().map_err(|error| error.to_string())?;
    let palette_offset = descriptor
        .palette_offset()
        .ok_or("expected a P8 descriptor")?;
    let mut payload = vec![0; bytes as usize];
    let (index_region, palette) = payload.split_at_mut(palette_offset as usize);
    let index_plane = &mut index_region[..stride * stored_height];
    if !exact(linear, index_plane, stride, palette) {
        if !allow_lossy || (width as u128 * height as u128) > MAX_QUANTIZED_PIXELS {
            return Ok(None);
        }
        if !quantize_opaque(linear, index_plane, stride, palette)? {
            return Ok(None);
        }
    }

    let (content_rows, padding_rows) = index_plane.split_at_mut(stride * height);
    for row in content_rows.chunks_mut(stride) {
        let edge = row[width - 1];
        row[width..].fill(edge);
    }
    for row in padding_rows.chunks_mut(stride) {
        row.copy_from_slice(&content_rows[(height - 1) * stride..]);
    }
    Ok(Some(payload))
}

fn exact(linear: &Rgba32FImage, indices: &mut [u8], stride: usize, palette: &mut [u8]) -> bool {
    let mut colors = BTreeMap::new();
    for (row, source) in indices.chunks_mut(stride).zip(linear.rows()) {
        for (index, pixel) in row.iter_mut().zip(source) {
            let rgba = quantize(pixel.0, Dither::None, [0, 0]);
            let next = u8::try_from(colors.len());
            *index = match colors.entry(rgba) {
                Entry::Occupied(previous) => *previous.get(),
                Entry::Vacant(new) => {
                    let Ok(next) = next else { return false };
                    let offset = usize::from(next) * 4;
                    palette[offset..offset + 4].copy_from_slice(&rgba);
                    *new.insert(next)
                }
            };
        }
    }
    true
}

fn quantize_opaque(
    linear: &Rgba32FImage,
    index_plane: &mut [u8],
    stride: usize,
    palette: &mut [u8],
) -> Result<bool, String> {
    let pixels = linear
        .pixels()
        .map(|pixel| {
            let [r, g, b, a] = quantize(pixel.0, Dither::None, [0, 0]);
            (a == 255).then_some(libimgquant::RGBA::new(r, g, b, a))
        })
        .collect::<Option<Vec<_>>>();
    let Some(pixels) = pixels else {
        return Ok(false);
    };
    let width = linear.width() as usize;
    let quantize = || -> Result<_, libimgquant::Error> {
        let mut attributes = libimgquant::new();
        attributes.set_speed(6)?;
        // Gamma 1.0 keeps the input and returned palette in linear space.
        let mut image = attributes.new_image(pixels, width, linear.height() as usize, 1.)?;
        attributes.quantize(&mut image)?.remapped(&mut image)
    };
    let (entries, indices) = quantize().map_err(|error| error.to_string())?;
    for (row, source) in index_plane.chunks_mut(stride).zip(indices.chunks(width)) {
        row[..width].copy_from_slice(source);
    }
    palette.fill(0);
    for (entry, color) in palette.as_chunks_mut::<4>().0.iter_mut().zip(entries) {
        *entry = [color.r, color.g, color.b, color.a];
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_core::texture::{ColorEncoding, TextureFormat, TextureLayout, TextureStorageLayout};

    #[test]
    fn exact_palette_preserves_alpha_and_repeats_texture_edges() {
        let linear = Rgba32FImage::from_fn(9, 2, |x, y| {
            image::Rgba([x as f32 / 255., y as f32 / 255., 0., 0.5])
        });
        let descriptor = TextureDescriptor {
            format: TextureFormat::Indexed8Rgba,
            storage_layout: TextureStorageLayout::Linear,
            source_encoding: ColorEncoding::Srgb,
            mip_count: 1,
            layout: TextureLayout {
                storage_size: [16, 3],
                ..TextureLayout::identity([9, 2])
            },
        };
        let payload = encode(&linear, descriptor, false).unwrap().unwrap();
        let (indices, palette) = payload.split_at(descriptor.palette_offset().unwrap() as usize);
        let rgba = palette.as_chunks::<4>().0;
        for (index, pixel) in indices[..48].iter().enumerate() {
            let content_x = (index % 16).min(8) as u8;
            let content_y = (index / 16).min(1) as u8;
            assert_eq!(rgba[usize::from(*pixel)], [content_x, content_y, 0, 128]);
        }
        assert_eq!(&indices[48..], &[0; 16]);
    }
}
