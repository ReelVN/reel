use super::{
    Gpu, GpuError, Quality,
    pipeline::{CompositeOperation, Pipelines},
    renderer::{Budgets, Reservation, reserve},
    resources,
    scopes::{Kind, Scopes, push_stack},
    targets::{Scissor, Target, target_bytes},
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
    pub(super) image_cache: &'a super::image_cache::ImageCache,
    pub(super) glyph_cache: &'a super::glyph_cache::GlyphCache,
    pub(super) glyphs: super::glyph_cache::FrameGlyphs,
    pub(super) white: Option<Arc<resources::Image>>,
    pub(super) target_cache: &'a super::target_cache::TargetCache,
    pub(super) target_allocations: Vec<Arc<super::target_cache::Allocation>>,
    pub(super) available_targets: Vec<Target>,
    pub(super) scissor: Scissor,
}
struct Scope<'a> {
    target: Target,
    clip: Option<&'a Clip>,
    mask: Option<reel_ui::frame::MaskUse>,
    image_mask: Option<(&'a reel_ui::frame::FrameImageMask, Affine)>,
    opacity: f32,
    invert: f32,
    blend: reel_ui::peniko::BlendMode,
    scissor: Scissor,
    isolated: bool,
}
struct MaskComposition<'a> {
    image_mask: Option<(&'a reel_ui::frame::FrameImageMask, Affine)>,
    source: Target,
    opacity: f32,
    invert: f32,
    blend: reel_ui::peniko::BlendMode,
    operation: CompositeOperation,
}
struct Stream<'a> {
    commands: &'a [FrameCommand],
    next: usize,
    transform: Affine,
    scopes: Scopes<Scope<'a>>,
    mask: Option<MaskComposition<'a>>,
}
impl Context<'_> {
    pub(super) fn target(&mut self) -> Result<Target, GpuError> {
        let target = if let Some(target) = self.available_targets.pop() {
            target
        } else {
            let cost = target_bytes(self.size, self.quality)?;
            let local = self
                .budget_scope
                .targets
                .reserve(cost)
                .map_err(|_| GpuError::Budget {
                    domain: "targets",
                    requested: cost,
                    available: self
                        .gpu
                        .limits
                        .target_bytes
                        .saturating_sub(self.budget_scope.targets.usage().bytes),
                })?;
            let allocation = self.target_cache.get(
                self.gpu,
                self.size,
                self.quality,
                self.target_allocations.len(),
                &self.budgets.targets,
                cost,
            )?;
            self.tickets.push(Reservation {
                _shared: None,
                _local: local,
            });
            let target = allocation.target.clone();
            let mut allocations = std::mem::take(&mut self.target_allocations);
            let pushed = push_stack(&mut allocations, allocation, |bytes| self.transient(bytes));
            self.target_allocations = allocations;
            pushed?;
            target
        };
        let _pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear isolated target"),
            color_attachments: &[Some(target.attachment(true))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            multiview_mask: None,
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
                self.scissor = parent.scopes.last().scissor;
                self.combine_group(
                    &parent.scopes.last().target,
                    &mask.source.view,
                    &finished.view,
                    reel_ui::imaging::Composite {
                        alpha: mask.opacity,
                        blend: mask.blend,
                    },
                    mask.invert,
                    mask.operation,
                    mask.image_mask,
                )?;
                self.recycle_target(mask.source)?;
                self.recycle_target(finished)?;
                continue;
            };
            current.next += 1;
            let at = command.origin.at;
            let transform = current.transform;
            self.scissor = current.scopes.last().scissor;
            match &command.op {
                FrameOp::PushContext(_) | FrameOp::PopContext => (),
                FrameOp::PushClip(clip) => {
                    let scissor = Scissor::from_clip(clip, transform, self.size);
                    current.scopes.push(
                        Kind::Clip,
                        at,
                        Scope {
                            target: if scissor.is_some() {
                                current.scopes.last().target.clone()
                            } else {
                                self.target()?
                            },
                            clip: if scissor.is_some() { None } else { Some(clip) },
                            mask: None,
                            image_mask: None,
                            opacity: 1.,
                            invert: 0.,
                            blend: Default::default(),
                            scissor: scissor.map_or(self.scissor, |s| self.scissor.intersect(s)),
                            isolated: scissor.is_none(),
                        },
                        |bytes| self.transient(bytes),
                    )?;
                }
                FrameOp::PushGroup(group) => {
                    blend_code(group.composite.blend).ok_or(GpuError::Unsupported {
                        at,
                        record: "group blend mode",
                    })?;
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
                            image_mask: group
                                .image_mask
                                .as_ref()
                                .map(|mask| (mask, transform * group.effect_to_target)),
                            opacity: group.composite.alpha,
                            invert: group.invert,
                            blend: group.composite.blend,
                            scissor: self.scissor,
                            isolated: true,
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
                    self.scissor = current.scopes.last().scissor;
                    if !group.isolated {
                        continue;
                    }
                    let mut source = group.target;
                    if let Some(clip) = group.clip {
                        let coverage = self.target()?;
                        self.clip(&coverage, clip, transform, entry.at)?;
                        if group.mask.is_none() {
                            // Fold coverage and opacity into the final source-over pass.
                            // No intermediate full-size clipped texture is needed.
                            self.combine_group(
                                &current.scopes.last().target,
                                &source.view,
                                &coverage.view,
                                reel_ui::imaging::Composite {
                                    alpha: group.opacity,
                                    blend: group.blend,
                                },
                                group.invert,
                                CompositeOperation::AlphaMask,
                                group.image_mask,
                            )?;
                            self.recycle_target(source)?;
                            self.recycle_target(coverage)?;
                            continue;
                        }
                        let clipped = self.target()?;
                        self.combine(
                            &clipped,
                            &source.view,
                            &coverage.view,
                            1.,
                            0.,
                            CompositeOperation::AlphaMask,
                        )?;
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
                            image_mask: group.image_mask,
                            source,
                            opacity: group.opacity,
                            invert: group.invert,
                            blend: group.blend,
                            operation: match definition.mode {
                                MaskMode::Alpha => CompositeOperation::AlphaMask,
                                MaskMode::Luminance => CompositeOperation::LuminanceMask,
                            },
                        });
                        let child =
                            self.start_stream(&definition.commands, transform * mask.transform)?;
                        push_stack(&mut streams, child, |bytes| self.transient(bytes))?;
                    } else {
                        self.combine_group(
                            &current.scopes.last().target,
                            &source.view,
                            &source.view,
                            reel_ui::imaging::Composite {
                                alpha: group.opacity,
                                blend: group.blend,
                            },
                            group.invert,
                            CompositeOperation::SourceOver,
                            group.image_mask,
                        )?;
                        self.recycle_target(source)?;
                    }
                }
                FrameOp::Draw(draw) => {
                    let consumed = self.image_batch(
                        &current.scopes.last().target,
                        &current.commands[current.next - 1..],
                        transform,
                    )?;
                    if consumed == 0 {
                        self.draw(frame, &current.scopes.last().target, draw, transform, at)?;
                    } else {
                        current.next += consumed - 1;
                    }
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
                image_mask: None,
                opacity: 1.,
                invert: 0.,
                blend: Default::default(),
                scissor: Scissor::full(self.size),
                isolated: true,
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
        invert: f32,
        operation: CompositeOperation,
    ) -> Result<(), GpuError> {
        if self.scissor.empty() {
            return Ok(());
        }
        self.transient(32)?;
        self.pipelines.composite(
            &self.gpu.device,
            &mut self.encoder,
            target,
            super::pipeline::CompositeInputs {
                source,
                mask,
                backdrop: source,
            },
            super::pipeline::CompositeParams {
                scissor: self.scissor,
                opacity: alpha,
                invert,
                operation,
                blend: 0,
            },
        );
        Ok(())
    }
    fn combine_group(
        &mut self,
        target: &Target,
        source: &wgpu::TextureView,
        mask: &wgpu::TextureView,
        composite: reel_ui::imaging::Composite,
        invert: f32,
        operation: CompositeOperation,
        image_mask: Option<(&reel_ui::frame::FrameImageMask, Affine)>,
    ) -> Result<(), GpuError> {
        if self.scissor.empty() {
            return Ok(());
        }
        let blend = blend_code(composite.blend).expect("validated group blend");
        if blend == 0 && image_mask.is_none() {
            return self.combine(target, source, mask, composite.alpha, invert, operation);
        }
        let image_mask = if let Some((definition, transform)) = image_mask {
            if definition.bounds.width() <= 0.
                || definition.bounds.height() <= 0.
                || transform.determinant() == 0.
            {
                return Ok(());
            }
            let uv = Affine::scale_non_uniform(
                1. / definition.bounds.width(),
                1. / definition.bounds.height(),
            ) * Affine::translate((-definition.bounds.x0, -definition.bounds.y0))
                * transform.inverse();
            Some((definition, uv))
        } else {
            None
        };
        // The attached backdrop is copied only for a blend which reads it.
        let backdrop = if blend != 0 {
            let backdrop = self.target()?;
            self.combine(
                &backdrop,
                &target.view,
                &target.view,
                1.,
                0.,
                CompositeOperation::SourceOver,
            )?;
            Some(backdrop)
        } else {
            None
        };
        let params = super::pipeline::CompositeParams {
            scissor: self.scissor,
            opacity: composite.alpha,
            invert,
            operation,
            blend,
        };
        if let Some((definition, uv)) = image_mask {
            self.transient(
                96 + ((8 + definition.images.len()) * size_of::<wgpu::BindGroupEntry>()
                    + definition.images.len() * size_of::<&wgpu::TextureView>())
                    as u64,
            )?;
            let pipeline = self.pipelines.masks.get(
                self.gpu,
                self.quality,
                &definition.program,
                &self.budgets.resources,
            )?;
            let images = definition
                .images
                .iter()
                .map(|handle| &self.images[handle].view)
                .collect();
            pipeline.draw(
                &self.gpu.device,
                &mut self.encoder,
                target,
                super::mask::Inputs {
                    source,
                    mask,
                    backdrop: backdrop.as_ref().map_or(source, |b| &b.view),
                    images,
                },
                params,
                definition,
                uv,
                &self.pipelines.samplers,
            );
        } else {
            self.transient(32)?;
            self.pipelines.composite(
                &self.gpu.device,
                &mut self.encoder,
                target,
                super::pipeline::CompositeInputs {
                    source,
                    mask,
                    backdrop: backdrop.as_ref().map_or(source, |b| &b.view),
                },
                params,
            );
        }
        if let Some(backdrop) = backdrop {
            self.recycle_target(backdrop)?;
        }
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

fn blend_code(blend: reel_ui::peniko::BlendMode) -> Option<u32> {
    use reel_ui::peniko::{Compose, Mix};
    match (blend.mix, blend.compose) {
        (Mix::Normal, Compose::SrcOver) => Some(0),
        (Mix::Normal, Compose::PlusLighter) => Some(1),
        (Mix::Multiply, Compose::SrcOver) => Some(2),
        (Mix::Screen, Compose::SrcOver) => Some(3),
        _ => None,
    }
}
