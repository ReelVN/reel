use super::{Gpu, GpuError};
pub(super) use crate::common::resources::ResourceSource;
use crate::common::resources::TextureUploadLayout;
use reel_core::texture::{TextureDescriptor, TextureFormat, TextureLayout};
use reel_ui::frame::RecordLocation;
use reel_ui::peniko::ImageFormat;
#[cfg(test)]
use reel_ui::peniko::{
    ImageAlphaType,
    color::{AlphaColor, LinearSrgb, PremulColor, Srgb},
};
use reel_ui::resources::{ColorEncoding, ResourceData, ResourceLease};

use crate::common::resources::rgba8_linear_f16;

#[cfg(test)]
#[derive(Default)]
pub(super) struct UploadCreations {
    pub(super) textures: std::sync::atomic::AtomicUsize,
    pub(super) staging: std::sync::atomic::AtomicUsize,
    pub(super) copies: std::sync::atomic::AtomicUsize,
    pub(super) draws: std::sync::atomic::AtomicUsize,
    pub(super) mesh_buffers: std::sync::atomic::AtomicUsize,
    pub(super) geometry_retries: std::sync::atomic::AtomicUsize,
    pub(super) geometry_staging: std::sync::atomic::AtomicUsize,
    pub(super) brush_groups: std::sync::atomic::AtomicUsize,
    pub(super) appearance_buffers: std::sync::atomic::AtomicUsize,
}
#[cfg(test)]
impl UploadCreations {
    pub(super) fn snapshot(&self) -> [usize; 3] {
        use std::sync::atomic::Ordering;
        [&self.textures, &self.staging, &self.copies].map(|n| n.load(Ordering::Relaxed))
    }
}

pub struct Image {
    pub view: wgpu::TextureView,
    pub(super) texture: wgpu::Texture,
    pub size: [u32; 2],
    pub(super) layout: TextureLayout,
    pub(super) texture_format: Option<TextureFormat>,
    /// Original channel encoding, retained for packed alpha coverage.
    pub(super) encoding: ColorEncoding,
    pub(super) yuv: Option<super::video::Planes>,
    pub(super) residency: Option<reel_core::budget::BudgetTicket>,
    pub(super) source: Option<ResourceSource>,
}
impl Image {
    pub(super) fn uv_transform(&self) -> reel_ui::kurbo::Affine {
        let [x, y] = self.layout.uv_scale();
        reel_ui::kurbo::Affine::scale_non_uniform(x, y)
    }
}
pub(super) fn image_rows(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    lease: &ResourceLease,
    at: RecordLocation,
    target: &Image,
    rows: std::ops::Range<u32>,
    staging: Option<&super::upload::Buffer>,
) -> Result<(), GpuError> {
    let ResourceData::Image { image, encoding } = lease.data() else {
        return Err(GpuError::Unsupported {
            at,
            record: "non-image resource",
        });
    };
    if image.format != ImageFormat::Rgba8 {
        return Err(GpuError::Unsupported {
            at,
            record: "image format",
        });
    }
    upload_rows_with(gpu, encoder, target, rows, staging, |index| {
        let rgba = &image.data.data()[index * 4..index * 4 + 4];
        rgba8_linear_f16(rgba, *encoding, image.alpha_type)
    })
}
pub use crate::common::resources::upload_cost;

pub(super) fn row_bytes(width: u32) -> u64 {
    (u64::from(width) * 8).div_ceil(256) * 256
}

/// Choose whole source rows without allocating a full-image staging buffer.
pub(super) fn rows_per_upload(size: [u32; 2], first: u32, available: u64) -> u32 {
    (available / row_bytes(size[0])).min(u64::from(size[1] - first)) as u32
}
pub fn upload(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    size: [u32; 2],
    pixels: &[f32],
    available: u64,
) -> Result<(Image, u64), GpuError> {
    upload_with(gpu, encoder, size, available, |index| {
        pixels[index * 4..index * 4 + 4].try_into().unwrap()
    })
}

fn upload_with(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    size: [u32; 2],
    available: u64,
    mut pixel: impl FnMut(usize) -> [f32; 4],
) -> Result<(Image, u64), GpuError> {
    gpu.check()?;
    let max = gpu.device.limits().max_texture_dimension_2d;
    if size.contains(&0) || size.iter().any(|&n| n > max) {
        return Err(GpuError::InvalidSize);
    }
    let cost = upload_cost(size)?;
    if cost > available {
        return Err(GpuError::Budget {
            domain: "image upload",
            requested: cost,
            available,
        });
    }
    let image = create_image(gpu, size)?;
    let bits = |index| pixel(index).map(|value| half::f16::from_f32(value).to_bits());
    upload_rows_with(gpu, encoder, &image, 0..size[1], None, bits)?;
    Ok((image, cost))
}

pub(super) fn create_image(gpu: &Gpu, size: [u32; 2]) -> Result<Image, GpuError> {
    gpu.check()?;
    #[cfg(test)]
    gpu.upload_creations
        .textures
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("linear premultiplied image"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    gpu.check()?;
    Ok(Image {
        view: texture.create_view(&Default::default()),
        texture,
        size,
        layout: TextureLayout::identity(size),
        texture_format: None,
        encoding: ColorEncoding::Linear,
        yuv: None,
        residency: None,
        source: None,
    })
}

pub(super) fn texture_format(format: TextureFormat) -> wgpu::TextureFormat {
    match format {
        TextureFormat::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
        TextureFormat::Bgra8 => wgpu::TextureFormat::Bgra8Unorm,
        TextureFormat::LinearRgba16Float => wgpu::TextureFormat::Rgba16Float,
        TextureFormat::Bc1Rgb => wgpu::TextureFormat::Bc1RgbaUnorm,
        TextureFormat::Bc3Rgba => wgpu::TextureFormat::Bc3RgbaUnorm,
    }
}
pub(super) fn texture_row_bytes(upload: TextureUploadLayout) -> u64 {
    upload.row_bytes.div_ceil(256) * 256
}
pub(super) fn texture_costs(upload: TextureUploadLayout) -> (u64, u64) {
    (
        upload.resident_bytes,
        texture_row_bytes(upload) * u64::from(upload.rows),
    )
}
pub(super) fn create_texture(
    gpu: &Gpu,
    descriptor: TextureDescriptor,
    upload: TextureUploadLayout,
    at: RecordLocation,
) -> Result<Image, GpuError> {
    if descriptor.format.is_compressed()
        && !gpu
            .device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        return Err(GpuError::Unsupported {
            at,
            record: "BC texture compression",
        });
    }
    let size = upload.layout.storage_size;
    if size
        .iter()
        .any(|&n| n > gpu.device.limits().max_texture_dimension_2d)
    {
        return Err(GpuError::InvalidSize);
    }
    #[cfg(test)]
    gpu.upload_creations
        .textures
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("prepared linear premultiplied texture"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: descriptor.mip_count,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: texture_format(descriptor.format),
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    gpu.check()?;
    Ok(Image {
        view: texture.create_view(&Default::default()),
        texture,
        size,
        layout: upload.layout,
        texture_format: Some(descriptor.format),
        encoding: descriptor.source_encoding,
        yuv: None,
        residency: None,
        source: None,
    })
}
/// Copy whole format block rows, retaining compressed bytes throughout staging.
pub(super) fn texture_rows(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    source: &ResourceLease,
    target: &Image,
    upload: TextureUploadLayout,
    rows: std::ops::Range<u32>,
    reusable: Option<&super::upload::Buffer>,
) -> Result<(), GpuError> {
    let ResourceData::Texture { descriptor, bytes } = source.data() else {
        return Err(GpuError::InvalidSize);
    };
    let row = texture_row_bytes(upload);
    let packed = upload.row_bytes;
    let count = rows.end - rows.start;
    #[cfg(test)]
    if reusable.is_none() {
        gpu.upload_creations
            .staging
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let owned;
    let staging = if let Some(buffer) = reusable {
        &buffer.buffer
    } else {
        owned = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bounded texture block upload"),
            size: row * u64::from(count),
            usage: wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        &owned
    };
    gpu.check()?;
    {
        let mut mapping = staging
            .slice(..)
            .get_mapped_range_mut()
            .map_err(|e| GpuError::Device(e.to_string()))?;
        let mut mapping = mapping.slice(..);
        for y in 0..count {
            let dest = u64::from(y) * row;
            let mut output = mapping.slice(dest as usize..(dest + packed) as usize);
            if descriptor.storage_layout == reel_core::texture::TextureStorageLayout::Linear {
                let src =
                    (upload.offset + u64::from(rows.start + y) * upload.source_row_bytes) as usize;
                output.copy_from_slice(&bytes.data()[src..src + packed as usize]);
            } else {
                let n = descriptor.format.block_bytes() as usize;
                for x in 0..upload.layout.storage_size[0].div_ceil(4) {
                    let src = upload.offset as usize
                        + descriptor.storage_layout.block_offset(
                            descriptor.mip_size(upload.mip),
                            n,
                            x,
                            rows.start + y,
                        );
                    output
                        .slice(x as usize * n..(x as usize + 1) * n)
                        .copy_from_slice(&bytes.data()[src..src + n]);
                }
            }
        }
    }
    staging.unmap();
    #[cfg(test)]
    gpu.upload_creations
        .copies
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let edge = descriptor.format.block_size();
    encoder.copy_buffer_to_texture(
        wgpu::TexelCopyBufferInfo {
            buffer: staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row as u32),
                rows_per_image: None,
            },
        },
        wgpu::TexelCopyTextureInfo {
            texture: &target.texture,
            mip_level: upload.mip,
            origin: wgpu::Origin3d {
                x: 0,
                y: rows.start * edge,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d {
            width: upload.layout.storage_size[0].next_multiple_of(edge),
            height: count * edge,
            depth_or_array_layers: 1,
        },
    );
    if let Some(reusable) = reusable {
        reusable.remap(encoder);
    }
    gpu.check()
}

fn upload_rows_with(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    image: &Image,
    rows: std::ops::Range<u32>,
    reusable: Option<&super::upload::Buffer>,
    mut pixel: impl FnMut(usize) -> [u16; 4],
) -> Result<(), GpuError> {
    gpu.check()?;
    let size = image.size;
    let row = row_bytes(size[0]) as u32;
    let height = rows.end - rows.start;
    #[cfg(test)]
    if reusable.is_none() {
        gpu.upload_creations
            .staging
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let owned;
    let staging = if let Some(buffer) = reusable {
        &buffer.buffer
    } else {
        owned = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bounded image upload"),
            size: u64::from(row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        &owned
    };
    gpu.check()?;
    {
        let mut mapped = staging.slice(..).get_mapped_range_mut().map_err(|error| {
            // Prefer the device's original loss/allocation error to a secondary
            // map failure, so the host can recover the correct device generation.
            gpu.check()
                .err()
                .unwrap_or_else(|| GpuError::Device(error.to_string()))
        })?;
        // Resolve the backend's write mapping once per upload. BufferViewMut::slice
        // enters the backend dynamically; slicing the resulting WriteOnly view
        // is ordinary bounds-checked pointer arithmetic for each pixel.
        let mut mapped = mapped.slice(..);
        for y in 0..height as usize {
            for x in 0..size[0] as usize {
                let components =
                    pixel((y + rows.start as usize) * size[0] as usize + x).map(u16::to_le_bytes);
                let start = y * row as usize + x * 8;
                mapped
                    .slice(start..start + 8)
                    .copy_from_slice(components.as_flattened());
            }
        }
    }
    staging.unmap();
    #[cfg(test)]
    gpu.upload_creations
        .copies
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    encoder.copy_buffer_to_texture(
        wgpu::TexelCopyBufferInfo {
            buffer: staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: None,
            },
        },
        wgpu::TexelCopyTextureInfo {
            texture: &image.texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: 0,
                y: rows.start,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d {
            width: size[0],
            height,
            depth_or_array_layers: 1,
        },
    );
    if let Some(reusable) = reusable {
        reusable.remap(encoder);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba8_upload_quantization_matches_color_contract_for_every_channel_and_alpha() {
        for encoding in [ColorEncoding::Srgb, ColorEncoding::Linear] {
            for alpha_type in [ImageAlphaType::Alpha, ImageAlphaType::AlphaPremultiplied] {
                for alpha in 0..=255u8 {
                    for channel in 0..=255u8 {
                        let mut rgba = [channel, 255 - channel, channel.rotate_left(3), alpha];
                        if alpha_type == ImageAlphaType::AlphaPremultiplied && alpha != 0 {
                            for channel in &mut rgba[..3] {
                                *channel = (u16::from(*channel) * u16::from(alpha) / 255) as u8;
                            }
                        }
                        let components = rgba.map(|n| f32::from(n) / 255.);
                        let expected = if alpha == 0 {
                            [0.; 4]
                        } else {
                            match (encoding, alpha_type) {
                                (ColorEncoding::Srgb, ImageAlphaType::Alpha) => {
                                    AlphaColor::<Srgb>::new(components)
                                        .convert::<LinearSrgb>()
                                        .premultiply()
                                        .components
                                }
                                (ColorEncoding::Srgb, ImageAlphaType::AlphaPremultiplied) => {
                                    PremulColor::<Srgb>::new(components)
                                        .convert::<LinearSrgb>()
                                        .components
                                }
                                (ColorEncoding::Linear, ImageAlphaType::Alpha) => {
                                    AlphaColor::<LinearSrgb>::new(components)
                                        .premultiply()
                                        .components
                                }
                                (ColorEncoding::Linear, ImageAlphaType::AlphaPremultiplied) => {
                                    components
                                }
                            }
                        }
                        .map(|value| half::f16::from_f32(value).to_bits());
                        assert_eq!(
                            rgba8_linear_f16(&rgba, encoding, alpha_type),
                            expected,
                            "encoding={encoding:?} alpha={alpha_type:?} rgba={rgba:?}"
                        );
                    }
                }
            }
        }
    }
}
