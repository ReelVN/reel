use super::{GpuError, Quality};
pub(super) use crate::common::scissor::Scissor;
#[cfg(test)]
pub(super) use crate::common::target_plan::target_pixels;

pub fn target_bytes(size: [u32; 2], quality: Quality) -> Result<u64, GpuError> {
    let bytes = match quality.format {
        wgpu::TextureFormat::Rgba16Float => 8,
        wgpu::TextureFormat::Rgba8Unorm => 4,
        wgpu::TextureFormat::R16Float => 2,
        wgpu::TextureFormat::R8Unorm => 1,
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

struct EffectPin {
    allocation: std::sync::Arc<super::target_cache::Allocation>,
    token: crate::common::effect_result::Token,
    _metadata: reel_core::budget::BudgetTicket,
}
impl Drop for EffectPin {
    fn drop(&mut self) {
        self.allocation
            .effect_pins
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
#[derive(Clone)]
pub struct Target {
    pub(super) slot: usize,
    pub size: [u32; 2],
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub multisample: Option<wgpu::TextureView>,
    pub(super) allocation: Option<std::sync::Weak<super::target_cache::Allocation>>,
    effect_pin: Option<std::sync::Arc<EffectPin>>,
}
impl Target {
    pub(super) fn is_effect_pinned(&self) -> bool {
        self.allocation
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|allocation| {
                allocation
                    .effect_pins
                    .load(std::sync::atomic::Ordering::Acquire)
                    != 0
            })
    }
    pub(super) fn effect_token(&self) -> Option<crate::common::effect_result::Token> {
        self.effect_pin.as_ref().map(|pin| pin.token)
    }
    pub(super) fn pin_effect(
        &self,
        token: crate::common::effect_result::Token,
        pool: &reel_core::budget::BudgetPool,
    ) -> Option<Self> {
        let allocation = self.allocation.as_ref()?.upgrade()?;
        let metadata = pool
            .reserve((size_of::<EffectPin>() + 2 * size_of::<usize>()) as u64)
            .ok()?;
        allocation
            .effect_pins
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let mut result = self.clone();
        result.effect_pin = Some(std::sync::Arc::new(EffectPin {
            allocation,
            token,
            _metadata: metadata,
        }));
        Some(result)
    }
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
            allocation: None,
            effect_pin: None,
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
    fn coverage_formats_keep_precision_and_charge_resolve_and_samples() {
        for (format, bytes) in [
            (wgpu::TextureFormat::R8Unorm, 1),
            (wgpu::TextureFormat::R16Float, 2),
        ] {
            for samples in [1, 4] {
                let quality = Quality {
                    format,
                    samples,
                    degraded: false,
                };
                assert_eq!(
                    target_bytes([321, 127], quality).unwrap(),
                    321 * 127 * bytes * if samples == 1 { 1 } else { 5 }
                );
            }
        }
    }
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
