use super::{GpuError, Quality};
use reel_ui::frame::{FrameCommand, FrameOp, RenderFrame};

/// Count simultaneously live targets using the same lifetime boundaries as replay.
/// Masks use an explicit stack; nesting does not consume the Rust call stack.
pub(super) fn peak_targets(
    frame: &RenderFrame,
    mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<u64, GpuError> {
    struct Stream<'a> {
        commands: &'a [FrameCommand],
        next: usize,
        live: u64,
        peak: u64,
    }
    let stream = |commands| Stream {
        commands,
        next: 0,
        live: 1,
        peak: 1,
    };
    let mut stack = Vec::new();
    super::scopes::push_stack(&mut stack, stream(frame.commands()), &mut reserve)?;
    loop {
        let current = stack.last_mut().unwrap();
        let Some(command) = current.commands.get(current.next) else {
            let finished = stack.pop().unwrap();
            let Some(parent) = stack.last_mut() else {
                return Ok(finished.peak);
            };
            parent.peak = parent.peak.max(parent.live + finished.peak);
            continue;
        };
        current.next += 1;
        match &command.op {
            FrameOp::PushClip(_) => {
                current.live += 1;
                current.peak = current.peak.max(current.live + 1);
            }
            FrameOp::PushGroup(group) => {
                current.live += 1;
                let clip_targets = if group.clip.is_none() {
                    0
                } else if group.mask.is_some() {
                    2
                } else {
                    1
                };
                current.peak = current.peak.max(current.live + clip_targets);
                if let Some(mask) = group.mask {
                    let definition =
                        frame
                            .masks()
                            .get(mask.mask as usize)
                            .ok_or(GpuError::Unsupported {
                                at: command.origin.at,
                                record: "missing mask",
                            })?;
                    super::scopes::push_stack(
                        &mut stack,
                        stream(&definition.commands),
                        &mut reserve,
                    )?;
                }
            }
            FrameOp::PopClip | FrameOp::PopGroup => {
                current.live -= 1;
            }
            _ => {}
        }
    }
}
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

pub struct Target {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub multisample: Option<wgpu::TextureView>,
}
impl Target {
    pub fn new(device: &wgpu::Device, size: [u32; 2], quality: Quality) -> Self {
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
