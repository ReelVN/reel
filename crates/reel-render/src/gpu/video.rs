//! Immutable planar frame uploads. Reuse is authorized by ImageCache only when
//! no recorded frame, upload or prepared render can observe the old contents.
use super::{Gpu, GpuError, resources::Image};
use reel_core::image::{Yuv420Image, YuvTransfer};

pub(super) struct Planes {
    textures: [wgpu::Texture; 3],
    pub(super) chroma: [wgpu::TextureView; 2],
    pub(super) color: wgpu::Buffer,
}

pub(super) fn costs(image: &Yuv420Image) -> (u64, u64) {
    let resident = image.byte_len() as u64 + 64;
    let staging = (0..3)
        .map(|i| {
            let [w, h] = image.plane_size(i);
            u64::from(w).div_ceil(256) * 256 * u64::from(h)
        })
        .sum::<u64>()
        + 64;
    (resident, staging)
}

pub(super) fn upload(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    image: &Yuv420Image,
    recycled: Option<Image>,
) -> Result<Image, GpuError> {
    gpu.check()?;
    let size = image.size();
    let mut output = if let Some(image) = recycled {
        debug_assert_eq!(image.size, size);
        debug_assert!(image.yuv.is_some());
        image
    } else {
        let textures = std::array::from_fn(|i| {
            let [width, height] = image.plane_size(i);
            #[cfg(test)]
            gpu.upload_creations
                .textures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("video plane"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        });
        let chroma = std::array::from_fn(|i| textures[i + 1].create_view(&Default::default()));
        let color = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video color"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Image {
            view: textures[0].create_view(&Default::default()),
            texture: textures[0].clone(),
            size,
            yuv: Some(Planes {
                textures,
                chroma,
                color,
            }),
            residency: None,
            source: None,
        }
    };
    let (_, staging_size) = costs(image);
    #[cfg(test)]
    gpu.upload_creations
        .staging
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("planar video upload"),
        size: staging_size,
        usage: wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: true,
    });
    gpu.check()?;
    let mut offset = 0u64;
    let planes = output.yuv.as_mut().unwrap();
    {
        let mut mapped = staging.slice(..).get_mapped_range_mut().map_err(|error| {
            gpu.check()
                .err()
                .unwrap_or_else(|| GpuError::Device(error.to_string()))
        })?;
        for i in 0..3 {
            let [w, h] = image.plane_size(i);
            let row = u64::from(w).div_ceil(256) * 256;
            for y in 0..h as usize {
                let start = offset as usize + y * row as usize;
                mapped
                    .slice(start..start + w as usize)
                    .copy_from_slice(&image.plane(i)[y * w as usize..(y + 1) * w as usize]);
            }
            offset += row * u64::from(h);
        }
        let mut color = [[0f32; 4]; 4];
        color[..3].copy_from_slice(&image.rgb_rows());
        color[3][0] = if image.color().transfer == YuvTransfer::Srgb {
            1.
        } else {
            0.
        };
        mapped
            .slice(offset as usize..offset as usize + 64)
            .copy_from_slice(bytemuck::cast_slice(&color));
    }
    staging.unmap();
    offset = 0;
    for i in 0..3 {
        let [w, h] = image.plane_size(i);
        let row = w.div_ceil(256) * 256;
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture: &planes.textures[i],
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            planes.textures[i].size(),
        );
        offset += u64::from(row) * u64::from(h);
    }
    encoder.copy_buffer_to_buffer(&staging, offset, &planes.color, 0, 64);
    #[cfg(test)]
    gpu.upload_creations
        .copies
        .fetch_add(3, std::sync::atomic::Ordering::Relaxed);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_core::image::{Yuv420Storage, YuvColor, YuvMatrix};
    use std::sync::Arc;

    #[test]
    fn planar_upload_cost_and_shader_validate_without_device() {
        #[derive(Debug)]
        struct Planes([Vec<u8>; 3]);
        impl Yuv420Storage for Planes {
            fn plane(&self, i: usize) -> &[u8] {
                &self.0[i]
            }
        }
        let image = Yuv420Image::new(
            [1920, 1080],
            YuvColor {
                matrix: YuvMatrix::Bt709,
                full_range: false,
                transfer: YuvTransfer::Bt709,
            },
            Arc::new(Planes([
                vec![0; 1920 * 1080],
                vec![128; 960 * 540],
                vec![128; 960 * 540],
            ])),
        )
        .unwrap();
        let (resident, staging) = costs(&image);
        assert_eq!(resident, 1920 * 1080 * 3 / 2 + 64);
        assert_eq!(staging, 2048 * 1080 + 2 * 1024 * 540 + 64);
        assert!(resident + staging < 7 << 20);
        let module = wgpu::naga::front::wgsl::parse_str(include_str!("paint.wgsl")).unwrap();
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        assert!(
            module
                .entry_points
                .iter()
                .any(|entry| entry.name == "fragment_yuv")
        );
    }
}
