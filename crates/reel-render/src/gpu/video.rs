//! Immutable YUV frame uploads. Reuse is authorized by ImageCache only when
//! no recorded frame, upload or prepared render can observe the old contents.
use super::{Gpu, GpuError, resources::Image};
use reel_core::image::{Yuv420Image, Yuv420Layout, YuvTransfer};

enum Textures {
    Planar([wgpu::Texture; 3]),
    Nv12([wgpu::Texture; 2]),
}
impl Textures {
    fn as_slice(&self) -> &[wgpu::Texture] {
        match self {
            Self::Planar(textures) => textures,
            Self::Nv12(textures) => textures,
        }
    }
}

pub(super) struct Planes {
    textures: Textures,
    pub(super) layout: Yuv420Layout,
    pub(super) chroma: [wgpu::TextureView; 2],
    pub(super) color: wgpu::Buffer,
}

pub(super) fn costs(image: &Yuv420Image) -> (u64, u64) {
    let resident = (0..image.plane_count())
        .map(|i| u64::from(image.row_bytes(i)) * u64::from(image.plane_size(i)[1]))
        .sum::<u64>()
        + 64;
    let staging = (0..image.plane_count())
        .map(|i| {
            let h = image.plane_size(i)[1];
            u64::from(image.row_bytes(i)).div_ceil(256) * 256 * u64::from(h)
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
    reusable: Option<&super::upload::Buffer>,
) -> Result<Image, GpuError> {
    gpu.check()?;
    let size = image.size();
    let mut output = if let Some(recycled) = recycled {
        debug_assert_eq!(recycled.size, size);
        debug_assert_eq!(recycled.yuv.as_ref().unwrap().layout, image.layout());
        recycled
    } else {
        let create = |i| {
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
                format: if image.layout() == Yuv420Layout::Nv12 && i == 1 {
                    wgpu::TextureFormat::Rg8Unorm
                } else {
                    wgpu::TextureFormat::R8Unorm
                },
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let textures = match image.layout() {
            Yuv420Layout::Planar => Textures::Planar(std::array::from_fn(create)),
            Yuv420Layout::Nv12 => Textures::Nv12(std::array::from_fn(create)),
        };
        // NV12 binds two views of the same UV texture to the existing layout.
        let chroma = std::array::from_fn(|i| {
            let index = if image.layout() == Yuv420Layout::Nv12 {
                1
            } else {
                i + 1
            };
            textures.as_slice()[index].create_view(&Default::default())
        });
        let color = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video color"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Image {
            view: textures.as_slice()[0].create_view(&Default::default()),
            brush: Default::default(),
            texture: textures.as_slice()[0].clone(),
            size,
            layout: reel_core::texture::TextureLayout::identity(size),
            origin: [0; 2],
            texture_format: None,
            encoding: reel_ui::resources::ColorEncoding::Linear,
            yuv: Some(Planes {
                textures,
                layout: image.layout(),
                chroma,
                color,
            }),
            residency: None,
            source: None,
            reuse_count: 0,
            discard_pending: std::sync::atomic::AtomicBool::new(false),
            backing: None,
            _view_storage: None,
        }
    };
    let (_, staging_size) = costs(image);
    let one_off;
    let staging = if let Some(buffer) = reusable {
        &buffer.buffer
    } else {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::ImageStagingCreate);
        #[cfg(test)]
        gpu.upload_creations
            .staging
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        one_off = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("YUV video upload"),
            size: staging_size,
            usage: wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        &one_off
    };
    gpu.check()?;
    let mut offset = 0u64;
    let planes = output.yuv.as_mut().unwrap();
    {
        let mut mapped = staging
            .slice(..staging_size)
            .get_mapped_range_mut()
            .map_err(|error| {
                gpu.check()
                    .err()
                    .unwrap_or_else(|| GpuError::Device(error.to_string()))
            })?;
        for i in 0..image.plane_count() {
            let h = image.plane_size(i)[1];
            let visible = image.row_bytes(i) as usize;
            let stride = image.row_stride(i) as usize;
            let row = (visible as u64).div_ceil(256) * 256;
            for y in 0..h as usize {
                let start = offset as usize + y * row as usize;
                mapped
                    .slice(start..start + visible)
                    .copy_from_slice(&image.plane(i)[y * stride..y * stride + visible]);
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
        color[3][1] = if image.layout() == Yuv420Layout::Nv12 {
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
    for i in 0..image.plane_count() {
        let h = image.plane_size(i)[1];
        let row = u64::from(image.row_bytes(i)).div_ceil(256) * 256;
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset,
                    bytes_per_row: Some(u32::try_from(row).map_err(|_| {
                        GpuError::Device("video upload row exceeds device address space".into())
                    })?),
                    rows_per_image: None,
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture: &planes.textures.as_slice()[i],
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            planes.textures.as_slice()[i].size(),
        );
        offset += row * u64::from(h);
    }
    encoder.copy_buffer_to_buffer(staging, offset, &planes.color, 0, 64);
    if let Some(buffer) = reusable {
        buffer.remap(encoder);
    }
    #[cfg(test)]
    gpu.upload_creations
        .copies
        .fetch_add(image.plane_count(), std::sync::atomic::Ordering::Relaxed);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_core::image::{Yuv420Storage, YuvColor, YuvMatrix};
    use std::sync::Arc;

    #[test]
    fn yuv_upload_cost_and_shader_validate_without_device() {
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
        let packed = Yuv420Image::new(
            [960, 1024],
            image.color(),
            Arc::new(Planes([
                vec![0; 960 * 1024],
                vec![128; 480 * 512],
                vec![128; 480 * 512],
            ])),
        )
        .unwrap();
        assert_eq!(
            costs(&packed),
            (1_474_560 + 64, 1024 * 1024 + 2 * 512 * 512 + 64)
        );
        let nv12 = Yuv420Image::with_layout(
            [960, 544],
            image.color(),
            Yuv420Layout::Nv12,
            [1024, 1024, 0],
            Arc::new(Planes([
                vec![16; 1024 * 544],
                vec![128; 1024 * 272],
                vec![],
            ])),
        )
        .unwrap();
        assert_eq!(
            costs(&nv12),
            (960 * (544 + 272) + 64, 1024 * (544 + 272) + 64)
        );
        assert_eq!(nv12.byte_len(), 1024 * (544 + 272));
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
