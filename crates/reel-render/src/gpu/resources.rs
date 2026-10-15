use super::{Gpu, GpuError};
use reel_ui::frame::{RecordLocation, RenderFrame};
use reel_ui::peniko::{
    ImageAlphaType, ImageFormat,
    color::{AlphaColor, LinearSrgb, PremulColor, Srgb},
};
use reel_ui::resources::{ColorEncoding, ResourceData, ResourceHandle};
use wgpu::util::DeviceExt;

#[cfg(test)]
#[derive(Default)]
pub(super) struct UploadCreations {
    pub(super) textures: std::sync::atomic::AtomicUsize,
    pub(super) staging: std::sync::atomic::AtomicUsize,
    pub(super) copies: std::sync::atomic::AtomicUsize,
    pub(super) draws: std::sync::atomic::AtomicUsize,
    pub(super) mesh_buffers: std::sync::atomic::AtomicUsize,
    pub(super) brush_groups: std::sync::atomic::AtomicUsize,
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
    pub size: [u32; 2],
    pub(super) residency: Option<reel_core::budget::BudgetTicket>,
    pub(super) source: Option<reel_ui::resources::ResourceLease>,
}
pub fn image(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    frame: &RenderFrame,
    handle: ResourceHandle,
    at: RecordLocation,
    available: u64,
) -> Result<(Image, u64), GpuError> {
    let lease = frame
        .resources()
        .leases()
        .iter()
        .find(|r| r.handle() == handle)
        .ok_or(GpuError::Unsupported {
            at,
            record: "missing image lease",
        })?;
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
    let size = [image.width, image.height];
    let cost = upload_cost(size)?;
    if cost > available {
        return Err(GpuError::Budget {
            domain: "image upload",
            requested: cost,
            available,
        });
    }
    let mut pixels = Vec::with_capacity((u64::from(size[0]) * u64::from(size[1]) * 4) as usize);
    for rgba in image.data.data().chunks_exact(4) {
        let components = std::array::from_fn(|i| f32::from(rgba[i]) / 255.);
        let pixel = if rgba[3] == 0 {
            // Hidden RGB must not become visible in interpolation or masks.
            PremulColor::<LinearSrgb>::TRANSPARENT
        } else {
            match (encoding, image.alpha_type) {
                (ColorEncoding::Srgb, ImageAlphaType::Alpha) => AlphaColor::<Srgb>::new(components)
                    .convert::<LinearSrgb>()
                    .premultiply(),
                (ColorEncoding::Srgb, ImageAlphaType::AlphaPremultiplied) => {
                    PremulColor::<Srgb>::new(components).convert::<LinearSrgb>()
                }
                (ColorEncoding::Linear, ImageAlphaType::Alpha) => {
                    AlphaColor::<LinearSrgb>::new(components).premultiply()
                }
                (ColorEncoding::Linear, ImageAlphaType::AlphaPremultiplied) => {
                    PremulColor::<LinearSrgb>::new(components)
                }
            }
        };
        pixels.extend(pixel.components);
    }
    upload(gpu, encoder, size, &pixels, available)
}
pub fn upload_cost(size: [u32; 2]) -> Result<u64, GpuError> {
    // CPU f32 conversion, padded staging and RGBA16 GPU residency coexist.
    let row = u64::from(size[0])
        .checked_mul(8)
        .ok_or(GpuError::InvalidSize)?;
    let aligned = row.div_ceil(256) * 256;
    (aligned * 2 + row * 3)
        .checked_mul(u64::from(size[1]))
        .ok_or(GpuError::InvalidSize)
}
pub fn upload(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    size: [u32; 2],
    pixels: &[f32],
    available: u64,
) -> Result<(Image, u64), GpuError> {
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
    let row = (size[0] * 8).div_ceil(256) * 256;
    let mut bytes = vec![0u8; row as usize * size[1] as usize];
    for y in 0..size[1] as usize {
        for x in 0..size[0] as usize * 4 {
            let value = half::f16::from_f32(pixels[y * size[0] as usize * 4 + x]).to_le_bytes();
            bytes[y * row as usize + x * 2..y * row as usize + x * 2 + 2].copy_from_slice(&value);
        }
    }
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
    #[cfg(test)]
    gpu.upload_creations
        .staging
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let staging = gpu
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("bounded image upload"),
            contents: &bytes,
            usage: wgpu::BufferUsages::COPY_SRC,
        });
    #[cfg(test)]
    gpu.upload_creations
        .copies
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    encoder.copy_buffer_to_texture(
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: None,
            },
        },
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        texture.size(),
    );
    Ok((
        Image {
            view: texture.create_view(&Default::default()),
            size,
            residency: None,
            source: None,
        },
        cost,
    ))
}
