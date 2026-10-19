use super::{GpuError, Quality};
use reel_ui::frame::{FrameCommand, FrameOp, RenderFrame};
use reel_ui::{
    imaging::record::{Clip, Geometry},
    kurbo::{Affine, Rect},
};

#[derive(Clone, Copy)]
pub(super) struct Scissor {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl Scissor {
    pub fn full(size: [u32; 2]) -> Self {
        Self {
            x: 0,
            y: 0,
            width: size[0],
            height: size[1],
        }
    }
    pub fn empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
    pub fn intersect(self, other: Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        Self {
            x,
            y,
            width: (self.x + self.width)
                .min(other.x + other.width)
                .saturating_sub(x),
            height: (self.y + self.height)
                .min(other.y + other.height)
                .saturating_sub(y),
        }
    }
    pub fn from_clip(clip: &Clip, outer: Affine, size: [u32; 2]) -> Option<Self> {
        let Clip::Fill {
            transform,
            shape: Geometry::Rect(rect),
            ..
        } = clip
        else {
            return None;
        };
        let transform = outer * *transform;
        let [_, b, c, _, _, _] = transform.as_coeffs();
        if b != 0. || c != 0. {
            return None;
        }
        let Rect { x0, y0, x1, y1 } = transform.transform_rect_bbox(*rect);
        if [x0, y0, x1, y1]
            .iter()
            .any(|n| !n.is_finite() || n.fract() != 0.)
        {
            return None;
        }
        let x0 = x0.clamp(0., size[0] as f64) as u32;
        let x1 = x1.clamp(0., size[0] as f64) as u32;
        let y0 = y0.clamp(0., size[1] as f64) as u32;
        let y1 = y1.clamp(0., size[1] as f64) as u32;
        Some(Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    }
    pub fn apply(self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_scissor_rect(self.x, self.y, self.width, self.height);
    }
}

/// Count simultaneously live targets using the same lifetime boundaries as replay.
/// Masks use an explicit stack; nesting does not consume the Rust call stack.
pub(super) fn peak_targets(
    frame: &RenderFrame,
    size: [u32; 2],
    mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<u64, GpuError> {
    struct Stream<'a> {
        commands: &'a [FrameCommand],
        next: usize,
        live: u64,
        peak: u64,
        transform: Affine,
        isolated: Vec<bool>,
    }
    let stream = |commands, transform| Stream {
        commands,
        next: 0,
        live: 1,
        peak: 1,
        transform,
        isolated: Vec::new(),
    };
    let mut stack = Vec::new();
    let transform = Affine::scale_non_uniform(
        size[0] as f64 / frame.info().logical_size[0],
        size[1] as f64 / frame.info().logical_size[1],
    );
    super::scopes::push_stack(
        &mut stack,
        stream(frame.commands(), transform),
        &mut reserve,
    )?;
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
            FrameOp::PushClip(clip) => {
                let isolated = Scissor::from_clip(clip, current.transform, size).is_none();
                super::scopes::push_stack(&mut current.isolated, isolated, &mut reserve)?;
                if isolated {
                    current.live += 1;
                    current.peak = current.peak.max(current.live + 1);
                }
            }
            FrameOp::PushGroup(group) => {
                super::scopes::push_stack(&mut current.isolated, true, &mut reserve)?;
                current.live += 1;
                let clip_targets = if group.clip.is_none() {
                    0
                } else if group.mask.is_some() {
                    2
                } else {
                    1
                };
                let backdrop = u64::from(group.composite.blend != Default::default());
                let composite_targets = if group.mask.is_some() {
                    // The clipped source replaces the original before mask replay.
                    // The final mask and backdrop coexist during group composition.
                    1 + backdrop
                } else {
                    clip_targets + backdrop
                };
                current.peak = current
                    .peak
                    .max(current.live + clip_targets.max(composite_targets));
                if let Some(mask) = group.mask {
                    let definition =
                        frame
                            .masks()
                            .get(mask.mask as usize)
                            .ok_or(GpuError::Unsupported {
                                at: command.origin.at,
                                record: "missing mask",
                            })?;
                    let transform = current.transform * mask.transform;
                    super::scopes::push_stack(
                        &mut stack,
                        stream(&definition.commands, transform),
                        &mut reserve,
                    )?;
                }
            }
            FrameOp::PopClip | FrameOp::PopGroup => {
                current.live -= u64::from(current.isolated.pop().expect("validated scope"));
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

#[derive(Clone)]
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
