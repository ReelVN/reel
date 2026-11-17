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
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

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
    upload_rows_with(gpu, encoder, target, rows.clone(), staging, |buffer| {
        if target.texture.format() == wgpu::TextureFormat::Rgba8UnormSrgb {
            let packed = image.width as usize * 4;
            let stride = raster_row_bytes(image.width, target.texture.format()) as usize;
            let height = (rows.end - rows.start) as usize;
            let first = rows.start as usize * packed;
            let source = &pixels.bytes()[first..first + height * packed];
            let mut mapping = buffer
                .slice(..(height * stride) as u64)
                .get_mapped_range_mut()
                .map_err(|e| GpuError::Device(e.to_string()))?;
            let mut output = mapping.slice(..);
            if packed == stride {
                output.copy_from_slice(source);
            } else {
                for (y, row) in source.chunks_exact(packed).enumerate() {
                    output
                        .slice(y * stride..y * stride + packed)
                        .copy_from_slice(row);
                }
            }
            Ok(())
        } else {
            let conversion = {
                let _timing =
                    reel_core::profile::Scope::new(reel_core::profile::Phase::ImageConvert);
                Rgba8LinearF16::new(image.encoding, image.alpha_type)
            };
            let (pixels, _) = pixels.bytes().as_chunks::<4>();
            convert_rows(
                gpu,
                buffer,
                image.width,
                rows,
                32 * 1024,
                &|first, output| {
                    convert_rgba8_chunk(&pixels[first..first + output.len()], output, &conversion);
                },
            )
        }
    })
}
#[cfg(test)]
pub use crate::common::resources::upload_cost;

pub(super) fn row_bytes(width: u32) -> u64 {
    raster_row_bytes(width, wgpu::TextureFormat::Rgba16Float)
}

pub(super) fn raster_row_bytes(width: u32, format: wgpu::TextureFormat) -> u64 {
    (u64::from(width) * u64::from(format.block_copy_size(None).expect("raster color format")))
        .next_multiple_of(256)
}

/// Choose whole source rows without allocating a full-image staging buffer.
pub(super) fn rows_per_upload(height: u32, first: u32, available: u64, row_bytes: u64) -> u32 {
    (available / row_bytes).min(u64::from(height - first)) as u32
}
pub(super) struct WhiteTexture {
    pub(super) view: wgpu::TextureView,
    _residency: reel_core::budget::BudgetTicket,
    initialization: AtomicU8,
}
impl WhiteTexture {
    pub(super) const BYTES: u64 = 8;
    const INITIALIZING: u8 = 0;
    const COMPLETE: u8 = 1;
    const OBSERVED: u8 = 2;

    pub(super) fn new(
        gpu: &Gpu,
        residency: reel_core::budget::BudgetTicket,
    ) -> Result<Arc<Self>, GpuError> {
        gpu.check()?;
        let (_, view) = create_sampled_storage(
            gpu,
            &wgpu::TextureDescriptor {
                label: Some("solid brush texture"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        )?;
        #[cfg(test)]
        gpu.upload_creations
            .textures
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("initialize solid brush texture"),
            });
        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("initialize solid brush texture"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        }));
        // Every render submission uses the same queue. Initialization stays
        // ordered even if the first frame using this view is abandoned.
        let texture = Arc::new(Self {
            view,
            _residency: residency,
            initialization: AtomicU8::new(Self::INITIALIZING),
        });
        gpu.submit(encoder);
        let pending = texture.clone();
        gpu.queue.on_submitted_work_done(move || {
            pending
                .initialization
                .store(Self::COMPLETE, Ordering::Release);
        });
        gpu.check()?;
        Ok(texture)
    }

    pub(super) fn initialization_pending(&self) -> bool {
        self.initialization.load(Ordering::Acquire) != Self::OBSERVED
    }

    pub(super) fn poll_initialization(&self) -> bool {
        self.initialization
            .compare_exchange(
                Self::COMPLETE,
                Self::OBSERVED,
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
    }
}

pub(super) fn create_image(
    gpu: &Gpu,
    size: [u32; 2],
    format: wgpu::TextureFormat,
) -> Result<Image, GpuError> {
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::ImageTextureCreate);
    gpu.check()?;
    #[cfg(test)]
    gpu.upload_creations
        .textures
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (texture, view) = create_sampled_storage(
        gpu,
        &wgpu::TextureDescriptor {
            label: Some("raster image"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
    )?;
    Ok(Image {
        view,
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
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::ImageTextureCreate);
    #[cfg(test)]
    gpu.upload_creations
        .textures
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (texture, view) = create_sampled_storage(
        gpu,
        &wgpu::TextureDescriptor {
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
        },
    )?;
    Ok(Image {
        view,
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
fn create_sampled_storage(
    gpu: &Gpu,
    descriptor: &wgpu::TextureDescriptor<'_>,
) -> Result<(wgpu::Texture, wgpu::TextureView), GpuError> {
    use reel_core::profile::{Phase, Scope};
    let texture = {
        let _timing = Scope::new(Phase::ImageNativeAllocate);
        gpu.device.create_texture(descriptor)
    };
    gpu.check()?;
    let view = {
        let _timing = Scope::new(Phase::ImageNativeView);
        texture.create_view(&Default::default())
    };
    Ok((texture, view))
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
    let _transfer = reel_core::profile::Scope::new(reel_core::profile::Phase::ImageTransfer);
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
    fill: impl FnOnce(&wgpu::Buffer) -> Result<(), GpuError>,
) -> Result<(), GpuError> {
    gpu.check()?;
    let size = image.size;
    let row = raster_row_bytes(size[0], image.texture.format()) as u32;
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
    let _transfer = reel_core::profile::Scope::new(reel_core::profile::Phase::ImageTransfer);
    fill(staging)?;
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

fn convert_rows(
    gpu: &Gpu,
    staging: &wgpu::Buffer,
    width: u32,
    rows: std::ops::Range<u32>,
    batch_pixels: u32,
    chunk: &(impl Fn(usize, &mut [[u16; 4]]) + Sync),
) -> Result<(), GpuError> {
    let row = row_bytes(width) as u32;
    let height = rows.end - rows.start;

    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::ImageConvert);
    let batch_rows = (batch_pixels / width).max(1);
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
            width as usize,
            row as usize,
            (rows.start + first) as usize,
            chunk,
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
    Ok(())
}

/// Convert in cacheable stack memory before copying into a write-only mapping.
fn write_rows(
    mut mapped: wgpu::WriteOnly<'_, [u8]>,
    width: usize,
    stride: usize,
    first_row: usize,
    chunk: &impl Fn(usize, &mut [[u16; 4]]),
) {
    let height = mapped.len() / stride;
    let mut batch = [[0u16; 4]; 128];
    for y in 0..height {
        for x in (0..width).step_by(batch.len()) {
            let count = batch.len().min(width - x);
            let source = (y + first_row) * width + x;
            chunk(source, &mut batch[..count]);
            let start = y * stride + x * 8;
            mapped
                .slice(start..start + count * 8)
                .copy_from_slice(bytemuck::cast_slice(&batch[..count]));
        }
    }
}

fn convert_rgba8_chunk(pixels: &[[u8; 4]], output: &mut [[u16; 4]], conversion: &Rgba8LinearF16) {
    let first = pixels[0];
    if pixels[1..].iter().all(|&pixel| pixel == first) {
        output.fill(conversion.convert(&first).map(u16::to_le));
    } else {
        for (pixel, target) in pixels.iter().zip(output) {
            *target = conversion.convert(pixel).map(u16::to_le);
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
        let pixels: Vec<_> = (0..rows.end * width)
            .map(|index| {
                let x = index % width;
                let sample = match x {
                    0..=127 => index - x,
                    128..=254 => index - x + 128,
                    1024.. => index - x + 1024,
                    _ => index,
                };
                [
                    sample as u8,
                    (sample >> 8) as u8,
                    (sample * 17) as u8,
                    (sample >> 4) as u8,
                ]
            })
            .collect();
        let mut actual = vec![0xa7; (rows.end - rows.start) * stride];
        let mut expected = actual.clone();
        for encoding in [ColorEncoding::Srgb, ColorEncoding::Linear] {
            for alpha_type in [ImageAlphaType::Alpha, ImageAlphaType::AlphaPremultiplied] {
                let conversion = Rgba8LinearF16::new(encoding, alpha_type);
                for (y, row) in expected.chunks_exact_mut(stride).enumerate() {
                    for x in 0..width {
                        let bits = conversion
                            .convert(&pixels[(y + rows.start) * width + x])
                            .map(u16::to_le);
                        row[x * 8..x * 8 + 8].copy_from_slice(bytemuck::bytes_of(&bits));
                    }
                }
                write_rows(
                    (&mut actual[..]).into(),
                    width,
                    stride,
                    rows.start,
                    &|first, output| {
                        convert_rgba8_chunk(
                            &pixels[first..first + output.len()],
                            output,
                            &conversion,
                        );
                    },
                );
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
