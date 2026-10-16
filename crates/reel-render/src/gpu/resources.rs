use super::{Gpu, GpuError};
use reel_ui::frame::RecordLocation;
use reel_ui::peniko::{
    ImageAlphaType, ImageFormat,
    color::{AlphaColor, LinearSrgb, PremulColor, Srgb},
};
use reel_ui::resources::{
    ColorEncoding, ResourceData, ResourceLease, ResourceVersion, ResourceWeak,
};
use std::sync::LazyLock;

static SRGB_TO_LINEAR: LazyLock<[f32; 256]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        AlphaColor::<Srgb>::new([i as f32 / 255., 0., 0., 1.])
            .convert::<LinearSrgb>()
            .components[0]
    })
});

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
    pub(super) source: Option<ImageSource>,
}
pub(super) struct ImageSource {
    pub(super) version: ResourceVersion,
    pub(super) lease: ResourceWeak,
}
pub fn image(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    lease: &ResourceLease,
    at: RecordLocation,
    available: u64,
) -> Result<(Image, u64), GpuError> {
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
    upload_with(gpu, encoder, size, available, |index| {
        let rgba = &image.data.data()[index * 4..index * 4 + 4];
        let components = std::array::from_fn(|i| f32::from(rgba[i]) / 255.);
        if rgba[3] == 0 {
            // Hidden RGB must not become visible in interpolation or masks.
            [0.; 4]
        } else {
            match (encoding, image.alpha_type) {
                (ColorEncoding::Srgb, ImageAlphaType::Alpha) => {
                    let alpha = components[3];
                    [
                        SRGB_TO_LINEAR[rgba[0] as usize] * alpha,
                        SRGB_TO_LINEAR[rgba[1] as usize] * alpha,
                        SRGB_TO_LINEAR[rgba[2] as usize] * alpha,
                        alpha,
                    ]
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
                (ColorEncoding::Linear, ImageAlphaType::AlphaPremultiplied) => components,
            }
        }
    })
}
pub fn upload_cost(size: [u32; 2]) -> Result<u64, GpuError> {
    // Convert directly into mapped staging. Only staging and GPU residency coexist.
    let row = u64::from(size[0])
        .checked_mul(8)
        .ok_or(GpuError::InvalidSize)?;
    let aligned = row.div_ceil(256) * 256;
    (aligned + row)
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
    let row = (size[0] * 8).div_ceil(256) * 256;
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
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bounded image upload"),
        size: u64::from(row) * u64::from(size[1]),
        usage: wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: true,
    });
    gpu.check()?;
    {
        let mut mapped = staging.slice(..).get_mapped_range_mut().map_err(|error| {
            // Prefer the device's original loss/allocation error to a secondary
            // map failure, so the host can recover the correct device generation.
            gpu.check()
                .err()
                .unwrap_or_else(|| GpuError::Device(error.to_string()))
        })?;
        for y in 0..size[1] as usize {
            for x in 0..size[0] as usize {
                let components = pixel(y * size[0] as usize + x)
                    .map(|value| half::f16::from_f32(value).to_le_bytes());
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
