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
use reel_ui::resources::{ColorEncoding, ResourceLease};

use crate::common::resources::Rgba8LinearF16;

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
    pub(super) effect_uniform_buffers: std::sync::atomic::AtomicUsize,
    pub(super) effect_bind_groups: std::sync::atomic::AtomicUsize,
    pub(super) effect_uniform_bytes: std::sync::atomic::AtomicU64,
    pub(super) backdrop_copies: std::sync::atomic::AtomicUsize,
    pub(super) disable_backdrop_copy: std::sync::atomic::AtomicBool,
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
    pub(super) origin: [u32; 2],
    pub(super) texture_format: Option<TextureFormat>,
    /// Original channel encoding, retained for packed alpha coverage.
    pub(super) encoding: ColorEncoding,
    pub(super) yuv: Option<super::video::Planes>,
    pub(super) residency: Option<reel_core::budget::BudgetTicket>,
    pub(super) source: Option<ResourceSource>,
    pub(super) reuse_count: u8,
    pub(super) discard_pending: std::sync::atomic::AtomicBool,
    pub(super) backing: Option<std::sync::Arc<Image>>,
    pub(super) _view_storage: Option<super::renderer::Reservation>,
}
impl Image {
    pub(super) fn normalized_origin(&self) -> [f64; 2] {
        std::array::from_fn(|i| f64::from(self.origin[i]) / f64::from(self.layout.storage_size[i]))
    }
    pub(super) fn uv_transform(&self) -> reel_ui::kurbo::Affine {
        let [x, y] = self.layout.uv_scale();
        let offset = self.origin.map(|v| v as f64);
        reel_ui::kurbo::Affine::translate((
            offset[0] / f64::from(self.size[0]),
            offset[1] / f64::from(self.size[1]),
        )) * reel_ui::kurbo::Affine::scale_non_uniform(x, y)
    }
    pub(super) fn atlas(
        backing: std::sync::Arc<Image>,
        source: &ResourceLease,
        storage: super::renderer::Reservation,
    ) -> Result<Self, GpuError> {
        let reel_ui::resources::ResourceView::AtlasView {
            image, encoding, ..
        } = source.view()
        else {
            return Err(GpuError::InvalidSize);
        };
        image
            .validate(backing.size)
            .map_err(|_| GpuError::InvalidSize)?;
        Ok(Self {
            view: backing.view.clone(),
            texture: backing.texture.clone(),
            size: backing.size,
            layout: TextureLayout {
                logical_size: image.logical_size,
                content_size: image.content_size,
                storage_size: backing.size,
            },
            origin: image.content_origin,
            texture_format: backing.texture_format,
            encoding,
            yuv: None,
            residency: None,
            source: None,
            reuse_count: 0,
            discard_pending: std::sync::atomic::AtomicBool::new(false),
            backing: Some(backing),
            _view_storage: Some(storage),
        })
    }
}
pub(super) fn image_rows(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    source: (&ResourceLease, &reel_ui::resources::ResourcePixels),
    at: RecordLocation,
    target: &Image,
    rows: std::ops::Range<u32>,
    staging: Option<&super::upload::Buffer>,
) -> Result<(), GpuError> {
    let (lease, pixels) = source;
    let Some(image) = lease.image_descriptor() else {
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
    let conversion = Rgba8LinearF16::new(image.encoding, image.alpha_type);
    upload_rows_with(gpu, encoder, target, rows, staging, |index| {
        let rgba = &pixels.bytes()[index * 4..index * 4 + 4];
        conversion.convert(rgba)
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
    pixel: impl Fn(usize) -> [f32; 4] + Sync,
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
        origin: [0; 2],
        texture_format: None,
        encoding: ColorEncoding::Linear,
        yuv: None,
        residency: None,
        source: None,
        reuse_count: 0,
        discard_pending: std::sync::atomic::AtomicBool::new(false),
        backing: None,
        _view_storage: None,
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
        origin: [0; 2],
        texture_format: Some(descriptor.format),
        encoding: descriptor.source_encoding,
        yuv: None,
        residency: None,
        source: None,
        reuse_count: 0,
        discard_pending: std::sync::atomic::AtomicBool::new(false),
        backing: None,
        _view_storage: None,
    })
}
/// Copy whole format block rows, retaining compressed bytes throughout staging.
pub(super) fn texture_rows(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    source: (&ResourceLease, &reel_ui::resources::ResourcePixels),
    target: &Image,
    upload: TextureUploadLayout,
    rows: std::ops::Range<u32>,
    reusable: Option<&super::upload::Buffer>,
) -> Result<(), GpuError> {
    let (source, pixels) = source;
    let Some(descriptor) = source.texture_descriptor() else {
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
                output.copy_from_slice(&pixels.bytes()[src..src + packed as usize]);
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
                        .copy_from_slice(&pixels.bytes()[src..src + n]);
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
    pixel: impl Fn(usize) -> [u16; 4] + Sync,
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
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::ImageConvert);
        let batch_rows = (128 * 1024 / size[0]).max(1);
        let convert = |batch: u32| {
            let first = batch * batch_rows;
            let end = (first + batch_rows).min(height);
            // WGPU grants each worker its disjoint mapping. WriteOnly slices
            // stay on the thread which obtained the view.
            let mut mapped = staging
                .slice(u64::from(first) * u64::from(row)..u64::from(end) * u64::from(row))
                .get_mapped_range_mut()
                .map_err(|error| {
                    gpu.check()
                        .err()
                        .unwrap_or_else(|| GpuError::Device(error.to_string()))
                })?;
            write_rows(
                mapped.slice(..),
                size[0] as usize,
                row as usize,
                (rows.start + first) as usize,
                &pixel,
            );
            Ok::<_, GpuError>(())
        };
        if height <= batch_rows {
            convert(0)?;
        } else {
            use rayon::prelude::*;
            (0..height.div_ceil(batch_rows))
                .into_par_iter()
                .try_for_each(convert)?;
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

/// Convert in cacheable stack memory before copying into a write-only mapping.
fn write_rows(
    mut mapped: wgpu::WriteOnly<'_, [u8]>,
    width: usize,
    stride: usize,
    first_row: usize,
    pixel: &(impl Fn(usize) -> [u16; 4] + Sync),
) {
    let height = mapped.len() / stride;
    let mut batch = [[0u16; 4]; 128];
    for y in 0..height {
        for x in (0..width).step_by(batch.len()) {
            let count = batch.len().min(width - x);
            let source = (y + first_row) * width + x;
            for (index, converted) in batch[..count].iter_mut().enumerate() {
                *converted = pixel(source + index).map(u16::to_le);
            }
            let start = y * stride + x * 8;
            mapped
                .slice(start..start + count * 8)
                .copy_from_slice(bytemuck::cast_slice(&batch[..count]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_preserves_partial_rows_color_bits_and_padding() {
        let width = 1027;
        let stride = row_bytes(width as u32) as usize;
        let rows = 3..520;
        let mut actual = vec![0xa7; (rows.end - rows.start) * stride];
        let mut expected = actual.clone();
        for encoding in [ColorEncoding::Srgb, ColorEncoding::Linear] {
            for alpha_type in [ImageAlphaType::Alpha, ImageAlphaType::AlphaPremultiplied] {
                let conversion = Rgba8LinearF16::new(encoding, alpha_type);
                let pixel = |index: usize| {
                    conversion.convert(&[
                        index as u8,
                        (index >> 8) as u8,
                        (index * 17) as u8,
                        (index >> 4) as u8,
                    ])
                };
                for (y, row) in expected.chunks_exact_mut(stride).enumerate() {
                    for x in 0..width {
                        let bits = pixel((y + rows.start) * width + x).map(u16::to_le);
                        row[x * 8..x * 8 + 8].copy_from_slice(bytemuck::bytes_of(&bits));
                    }
                }
                write_rows((&mut actual[..]).into(), width, stride, rows.start, &pixel);
                assert_eq!(actual, expected, "{encoding:?} {alpha_type:?}");
            }
        }
    }

    #[test]
    fn rgba8_upload_quantization_matches_color_contract_for_every_channel_and_alpha() {
        for encoding in [ColorEncoding::Srgb, ColorEncoding::Linear] {
            for alpha_type in [ImageAlphaType::Alpha, ImageAlphaType::AlphaPremultiplied] {
                let conversion = Rgba8LinearF16::new(encoding, alpha_type);
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
                            conversion.convert(&rgba),
                            expected,
                            "encoding={encoding:?} alpha={alpha_type:?} rgba={rgba:?}"
                        );
                    }
                }
            }
        }
    }
}
