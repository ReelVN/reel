use super::{
    Gpu, GpuError, Quality,
    pipeline::Pipelines,
    renderer::{Budgets, Reservation, reserve},
    resources,
    scopes::{Kind, Scopes},
    targets::{Target, target_bytes},
};
use reel_ui::frame::{FrameCommand, FrameOp, RecordLocation, RenderFrame};
use reel_ui::{
    imaging::{Composite, MaskMode, record::Clip},
    kurbo::Affine,
};
use std::{collections::HashMap, sync::Arc};
pub(super) struct Context<'a> {
    pub(super) gpu: &'a Gpu,
    pub(super) encoder: wgpu::CommandEncoder,
    pub(super) pipelines: Pipelines,
    pub(super) quality: Quality,
    pub(super) size: [u32; 2],
    pub(super) budgets: &'a Budgets,
    pub(super) budget_scope: Budgets,
    pub(super) tickets: Vec<Reservation>,
    pub(super) images: HashMap<reel_ui::resources::ResourceHandle, Arc<resources::Image>>,
}
struct Scope<'a> {
    target: Target,
    clip: Option<&'a Clip>,
    mask: Option<reel_ui::frame::MaskUse>,
    opacity: f32,
}
impl Context<'_> {
    pub(super) fn target(&mut self) -> Result<Target, GpuError> {
        let cost = target_bytes(self.size, self.quality)?;
        self.tickets.push(reserve(
            &self.budgets.targets,
            &self.budget_scope.targets,
            cost,
            self.gpu.limits.target_bytes,
            "targets",
        )?);
        let target = Target::new(&self.gpu.device, self.size, self.quality);
        let _pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear isolated target"),
            color_attachments: &[Some(target.attachment(true))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        drop(_pass);
        Ok(target)
    }
    pub(super) fn stream(
        &mut self,
        frame: &RenderFrame,
        commands: &[FrameCommand],
        transform: Affine,
        depth: usize,
        origin: RecordLocation,
    ) -> Result<Target, GpuError> {
        let mut stack = Scopes::new(depth, self.gpu.limits.max_depth, origin, || {
            Ok(Scope {
                target: self.target()?,
                clip: None,
                mask: None,
                opacity: 1.,
            })
        })?;
        for command in commands {
            let at = command.origin.at;
            match &command.op {
                FrameOp::PushContext(_) | FrameOp::PopContext => (),
                FrameOp::PushClip(clip) => {
                    stack.push(Kind::Clip, at, || {
                        Ok(Scope {
                            target: self.target()?,
                            clip: Some(clip),
                            mask: None,
                            opacity: 1.,
                        })
                    })?;
                }
                FrameOp::PushGroup(group) => {
                    self.composite_supported(group.composite, at)?;
                    if !group.filters.is_empty() {
                        return Err(GpuError::Unsupported {
                            at,
                            record: "group filters",
                        });
                    }
                    stack.push(Kind::Group, at, || {
                        Ok(Scope {
                            target: self.target()?,
                            clip: group.clip.as_ref(),
                            mask: group.mask,
                            opacity: group.composite.alpha,
                        })
                    })?;
                }
                FrameOp::PopClip | FrameOp::PopGroup => {
                    let kind = if matches!(command.op, FrameOp::PopClip) {
                        Kind::Clip
                    } else {
                        Kind::Group
                    };
                    let entry = stack.pop(kind, at)?;
                    let group = entry.value;
                    let parent = stack.last();
                    let mut source = group.target;
                    if let Some(clip) = group.clip {
                        let coverage = self.target()?;
                        self.clip(&coverage, clip, transform, entry.at)?;
                        let clipped = self.target()?;
                        self.combine(&clipped, &source.view, &coverage.view, 1., 1, 0.)?;
                        source = clipped;
                    }
                    if let Some(mask) = group.mask {
                        let definition =
                            frame
                                .masks()
                                .get(mask.mask as usize)
                                .ok_or(GpuError::Unsupported {
                                    at,
                                    record: "missing mask",
                                })?;
                        let coverage = self.stream(
                            frame,
                            &definition.commands,
                            transform * mask.transform,
                            stack.depth(),
                            entry.at,
                        )?;
                        self.combine(
                            &parent.target,
                            &source.view,
                            &coverage.view,
                            group.opacity,
                            match definition.mode {
                                MaskMode::Alpha => 1,
                                MaskMode::Luminance => 2,
                            },
                            0.,
                        )?;
                    } else {
                        self.combine(
                            &parent.target,
                            &source.view,
                            &source.view,
                            group.opacity,
                            0,
                            0.,
                        )?;
                    }
                }
                FrameOp::Draw(draw) => {
                    self.draw(frame, &stack.last().target, draw, transform, at)?
                }
                FrameOp::External(_) => {
                    return Err(GpuError::Unsupported {
                        at,
                        record: "external producer",
                    });
                }
            }
        }
        Ok(stack.finish()?.target)
    }
    pub(super) fn combine(
        &mut self,
        target: &Target,
        source: &wgpu::TextureView,
        mask: &wgpu::TextureView,
        alpha: f32,
        mode: u32,
        progress: f32,
    ) -> Result<(), GpuError> {
        self.transient(16)?;
        self.pipelines.composite(
            &self.gpu.device,
            &mut self.encoder,
            target,
            source,
            mask,
            super::pipeline::CompositeParams {
                opacity: alpha,
                mode,
                progress,
            },
        );
        Ok(())
    }
    pub(super) fn transient(&mut self, bytes: u64) -> Result<(), GpuError> {
        self.tickets.push(reserve(
            &self.budgets.transient,
            &self.budget_scope.transient,
            bytes,
            self.gpu.limits.transient_bytes,
            "transient",
        )?);
        Ok(())
    }

    pub(super) fn composite_supported(
        &self,
        c: Composite,
        at: RecordLocation,
    ) -> Result<(), GpuError> {
        if c.blend != Composite::default().blend {
            Err(GpuError::Unsupported {
                at,
                record: "blend mode",
            })
        } else {
            Ok(())
        }
    }
}
