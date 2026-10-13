use super::{
    Gpu, GpuError, Quality,
    pipeline::Pipelines,
    renderer::{Budgets, Reservation, reserve},
    resources,
    scopes::{Kind, Scopes, push_stack},
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
    pub(super) pipelines: Arc<Pipelines>,
    pub(super) quality: Quality,
    pub(super) size: [u32; 2],
    pub(super) budgets: &'a Budgets,
    pub(super) budget_scope: Budgets,
    pub(super) tickets: Vec<Reservation>,
    pub(super) images: HashMap<reel_ui::resources::ResourceHandle, Arc<resources::Image>>,
    pub(super) white: Option<Arc<resources::Image>>,
    pub(super) available_targets: Vec<Target>,
}
struct Scope<'a> {
    target: Target,
    clip: Option<&'a Clip>,
    mask: Option<reel_ui::frame::MaskUse>,
    opacity: f32,
}
struct MaskComposition {
    source: Target,
    opacity: f32,
    mode: u32,
}
struct Stream<'a> {
    commands: &'a [FrameCommand],
    next: usize,
    transform: Affine,
    scopes: Scopes<Scope<'a>>,
    mask: Option<MaskComposition>,
}
impl Context<'_> {
    pub(super) fn target(&mut self) -> Result<Target, GpuError> {
        let target = if let Some(target) = self.available_targets.pop() {
            target
        } else {
            let cost = target_bytes(self.size, self.quality)?;
            self.tickets.push(reserve(
                &self.budgets.targets,
                &self.budget_scope.targets,
                cost,
                self.gpu.limits.target_bytes,
                "targets",
            )?);
            Target::new(&self.gpu.device, self.size, self.quality)
        };
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
    /// All reads have been encoded before recycling. Later passes can overwrite
    /// the texture, but its reservation remains live through queue completion.
    fn recycle_target(&mut self, target: Target) -> Result<(), GpuError> {
        if self.available_targets.len() == self.available_targets.capacity() {
            let additional = self.available_targets.capacity().max(1);
            self.transient((additional * size_of::<Target>()) as u64)?;
            self.available_targets.reserve_exact(additional);
        }
        self.available_targets.push(target);
        Ok(())
    }
    pub(super) fn stream(
        &mut self,
        frame: &RenderFrame,
        commands: &[FrameCommand],
        transform: Affine,
    ) -> Result<Target, GpuError> {
        let first = self.start_stream(commands, transform)?;
        let mut streams = Vec::new();
        push_stack(&mut streams, first, |bytes| self.transient(bytes))?;
        loop {
            let current = streams.last_mut().expect("active replay stream");
            let Some(command) = current.commands.get(current.next) else {
                let finished = streams.pop().unwrap().scopes.finish()?.target;
                let Some(parent) = streams.last_mut() else {
                    return Ok(finished);
                };
                let mask = parent.mask.take().expect("suspended mask composition");
                self.combine(
                    &parent.scopes.last().target,
                    &mask.source.view,
                    &finished.view,
                    mask.opacity,
                    mask.mode,
                    0.,
                )?;
                self.recycle_target(mask.source)?;
                self.recycle_target(finished)?;
                continue;
            };
            current.next += 1;
            let at = command.origin.at;
            let transform = current.transform;
            match &command.op {
                FrameOp::PushContext(_) | FrameOp::PopContext => (),
                FrameOp::PushClip(clip) => {
                    current.scopes.push(
                        Kind::Clip,
                        at,
                        Scope {
                            target: self.target()?,
                            clip: Some(clip),
                            mask: None,
                            opacity: 1.,
                        },
                        |bytes| self.transient(bytes),
                    )?;
                }
                FrameOp::PushGroup(group) => {
                    self.composite_supported(group.composite, at)?;
                    if !group.filters.is_empty() {
                        return Err(GpuError::Unsupported {
                            at,
                            record: "group filters",
                        });
                    }
                    current.scopes.push(
                        Kind::Group,
                        at,
                        Scope {
                            target: self.target()?,
                            clip: group.clip.as_ref(),
                            mask: group.mask,
                            opacity: group.composite.alpha,
                        },
                        |bytes| self.transient(bytes),
                    )?;
                }
                FrameOp::PopClip | FrameOp::PopGroup => {
                    let kind = if matches!(command.op, FrameOp::PopClip) {
                        Kind::Clip
                    } else {
                        Kind::Group
                    };
                    let entry = current.scopes.pop(kind, at)?;
                    let group = entry.value;
                    let mut source = group.target;
                    if let Some(clip) = group.clip {
                        let coverage = self.target()?;
                        self.clip(&coverage, clip, transform, entry.at)?;
                        let clipped = self.target()?;
                        self.combine(&clipped, &source.view, &coverage.view, 1., 1, 0.)?;
                        self.recycle_target(source)?;
                        self.recycle_target(coverage)?;
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
                        current.mask = Some(MaskComposition {
                            source,
                            opacity: group.opacity,
                            mode: match definition.mode {
                                MaskMode::Alpha => 1,
                                MaskMode::Luminance => 2,
                            },
                        });
                        let child =
                            self.start_stream(&definition.commands, transform * mask.transform)?;
                        push_stack(&mut streams, child, |bytes| self.transient(bytes))?;
                    } else {
                        self.combine(
                            &current.scopes.last().target,
                            &source.view,
                            &source.view,
                            group.opacity,
                            0,
                            0.,
                        )?;
                        self.recycle_target(source)?;
                    }
                }
                FrameOp::Draw(draw) => {
                    self.draw(frame, &current.scopes.last().target, draw, transform, at)?
                }
                FrameOp::External(_) => {
                    return Err(GpuError::Unsupported {
                        at,
                        record: "external producer",
                    });
                }
            }
        }
    }
    fn start_stream<'a>(
        &mut self,
        commands: &'a [FrameCommand],
        transform: Affine,
    ) -> Result<Stream<'a>, GpuError> {
        Ok(Stream {
            commands,
            next: 0,
            transform,
            scopes: Scopes::new(Scope {
                target: self.target()?,
                clip: None,
                mask: None,
                opacity: 1.,
            }),
            mask: None,
        })
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
