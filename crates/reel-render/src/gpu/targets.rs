use super::{GpuError, Quality};
pub(super) use crate::common::scissor::Scissor;
#[cfg(test)]
pub(super) use crate::common::target_plan::target_pixels;

pub fn target_bytes(size: [u32; 2], quality: Quality) -> Result<u64, GpuError> {
    let bytes = match quality.format {
        wgpu::TextureFormat::Rgba16Float => 8,
        wgpu::TextureFormat::Rgba8Unorm => 4,
        _ => return Err(GpuError::InvalidSize),
    };
    u64::from(size[0])
        .checked_mul(u64::from(size[1]))
        .and_then(|n| n.checked_mul(bytes))
        .and_then(|n| {
            n.checked_mul(if quality.samples > 1 {
                u64::from(quality.samples) + 1
            } else {
                1
            })
        })
        .ok_or(GpuError::InvalidSize)
}

#[derive(Clone)]
pub struct Target {
    pub(super) slot: usize,
    pub size: [u32; 2],
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub multisample: Option<wgpu::TextureView>,
}
impl Target {
    pub fn storage_size(&self) -> [u32; 2] {
        let size = self.texture.size();
        [size.width, size.height]
    }
    pub fn new(device: &wgpu::Device, size: [u32; 2], quality: Quality) -> Self {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::TargetCreate);
        let descriptor = wgpu::TextureDescriptor {
            label: Some("reel linear target"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: quality.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        };
        let texture = device.create_texture(&descriptor);
        let view = texture.create_view(&Default::default());
        let multisample = (quality.samples > 1).then(|| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    sample_count: quality.samples,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    ..descriptor
                })
                .create_view(&Default::default())
        });
        Self {
            slot: 0,
            size,
            texture,
            view,
            multisample,
        }
    }
    pub fn attachment(&self, clear: bool) -> wgpu::RenderPassColorAttachment<'_> {
        wgpu::RenderPassColorAttachment {
            view: self.multisample.as_ref().unwrap_or(&self.view),
            resolve_target: self.multisample.as_ref().map(|_| &self.view),
            depth_slice: None,
            ops: wgpu::Operations {
                load: if clear {
                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                } else {
                    wgpu::LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolve_and_msaa_are_both_charged() {
        assert_eq!(
            target_bytes(
                [1920, 1080],
                Quality {
                    format: wgpu::TextureFormat::Rgba16Float,
                    samples: 4,
                    degraded: false
                }
            )
            .unwrap(),
            82_944_000
        );
    }
}
