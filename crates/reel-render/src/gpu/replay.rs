use super::{
    Gpu, GpuError, Quality,
    pipeline::{CompositeOperation, Pipelines},
    renderer::{Budgets, Reservation, reserve},
    resources,
    scopes::{Kind, Scopes, push_stack},
    targets::{Scissor, Target, target_bytes},
};
use reel_ui::frame::{FrameCommand, FrameDrawing, FrameOp, RecordLocation, SceneTransition};
use reel_ui::{
    imaging::{Composite, MaskMode, record::Clip},
    kurbo::Affine,
};
use std::{collections::HashMap, sync::Arc};
pub(super) struct Context<'a> {
    pub(super) capture: bool,
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
    pub(super) glyph_plan: crate::common::glyph_plan::Plan,
    pub(super) effects: Vec<Arc<super::effect::Compiled>>,
    pub(super) white: Option<Arc<resources::Image>>,
    pub(super) target_cache: &'a super::target_cache::TargetCache,
    pub(super) target_allocations: Vec<Arc<super::target_cache::Allocation>>,
    pub(super) available_targets: Vec<Target>,
    pub(super) scissor: Scissor,
    pub(super) mesh_clip: Option<crate::common::mesh::RectClip>,
    pub(super) paint_batch: Option<super::draw::PaintBatch>,
    pub(super) geometry_policy: &'a super::geometry::UploadPolicy,
    pub(super) geometry_cache: &'a super::geometry::Cache,
    pub(super) geometry: Vec<super::geometry::Storage>,
}
struct Scope<'a> {
    target: Target,
    clip: Option<&'a Clip>,
    mask: Option<reel_ui::frame::MaskUse>,
    image_mask: Option<(&'a reel_ui::frame::FrameImageMask, Affine)>,
    effect: Option<(&'a reel_ui::frame::FrameEffect, Affine)>,
    mask_effect: Option<(&'a reel_ui::frame::FrameEffect, Affine)>,
    source_parent: Option<(Affine, [u32; 2], Scissor)>,
    source_view: Option<crate::common::source_view::SourceView>,
    opacity: f32,
    invert: f32,
    blend: reel_ui::frame::BlendMode,
    scissor: Scissor,
    mesh_clip: Option<crate::common::mesh::RectClip>,
    isolated: bool,
}
struct MaskComposition<'a> {
    image_mask: Option<(&'a reel_ui::frame::FrameImageMask, Affine)>,
    source: Target,
    opacity: f32,
    invert: f32,
    blend: reel_ui::frame::BlendMode,
    operation: CompositeOperation,
    scissor: Scissor,
}
enum Pending<'a> {
    Mask(MaskComposition<'a>),
    Transition {
        definition: &'a SceneTransition,
        previous: Option<Target>,
    },
}
struct Stream<'a> {
    drawing: &'a FrameDrawing,
    // Masks have their own command space; frame exclusions apply to drawing ranges.
    offset: Option<usize>,
    transition_next: usize,
    transitions_enabled: bool,
    commands: &'a [FrameCommand],
    groups: crate::common::scopes::GroupPlan,
    next: usize,
    size: [u32; 2],
    transform: Affine,
    scopes: Scopes<Scope<'a>>,
    pending: Option<Pending<'a>>,
}
impl Context<'_> {
    pub(super) fn target(&mut self) -> Result<Target, GpuError> {
        self.flush_draws()?;
        let target = if let Some(index) = self
            .available_targets
            .iter()
            .rposition(|target| target.size == self.size)
        {
            self.available_targets.swap_remove(index)
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
                self.target_allocations
                    .iter()
                    .filter(|allocation| allocation.target.size == self.size)
                    .count(),
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
        frame: &FrameDrawing,
        transform: Affine,
    ) -> Result<Target, GpuError> {
        let first = self.start_stream(
            frame,
            frame.commands(),
            Some(0),
            true,
            transform,
            Scissor::full(self.size),
        )?;
        let mut streams = Vec::new();
        push_stack(&mut streams, first, |bytes| self.transient(bytes))?;
        loop {
            let current = streams.last_mut().expect("active replay stream");
            // A transition boundary is a gap, including an empty hidden subtree.
            if current.transitions_enabled
                && let Some(transition) = current.drawing.transitions().get(current.transition_next)
                && transition.commands.start == current.offset.unwrap() + current.next
            {
                current.transition_next += 1;
                current.next = transition.commands.end - current.offset.unwrap();
                if self.capture && !transition.capture_included {
                    continue;
                }
                self.scissor = current.scopes.last().scissor;
                let child = self.start_stream(
                    transition.previous.drawing(),
                    transition.previous.drawing().commands(),
                    Some(0),
                    true,
                    current.transform
                        * transition.parent_to_window
                        * transition.previous.to_parent(),
                    self.scissor,
                )?;
                self.combine(
                    &child.scopes.last().target,
                    &current.scopes.last().target.view,
                    &current.scopes.last().target.view,
                    1.,
                    0.,
                    CompositeOperation::Transition(0.),
                )?;
                current.pending = Some(Pending::Transition {
                    definition: transition,
                    previous: None,
                });
                push_stack(&mut streams, child, |bytes| self.transient(bytes))?;
                continue;
            }
            if self.capture
                && current.next < current.commands.len()
                && let Some(offset) = current.offset
                && let Some(end) = current.drawing.capture_exclusion_end(offset + current.next)
            {
                current.next = (end - offset).min(current.commands.len());
                current.transition_next = current
                    .drawing
                    .transitions()
                    .partition_point(|transition| transition.commands.start < end);
                continue;
            }
            let Some(command) = current.commands.get(current.next) else {
                self.flush_draws()?;
                let finished = streams.pop().unwrap().scopes.finish()?.target;
                let Some(parent) = streams.last_mut() else {
                    return Ok(finished);
                };
                self.size = parent.size;
                self.scissor = parent.scopes.last().scissor;
                match parent.pending.take().expect("suspended composition") {
                    Pending::Mask(mask) => {
                        self.scissor = self.scissor.intersect(mask.scissor);
                        self.combine_group(
                            &parent.scopes.last().target,
                            &mask.source.view,
                            &finished.view,
                            reel_ui::frame::GroupComposite {
                                alpha: mask.opacity,
                                blend: mask.blend,
                            },
                            mask.invert,
                            mask.operation,
                            mask.image_mask,
                        )?;
                        self.recycle_target(mask.source)?;
                        self.recycle_target(finished)?;
                    }
                    Pending::Transition {
                        definition,
                        previous: None,
                    } => {
                        let child = self.start_stream(
                            parent.drawing,
                            &parent.drawing.commands()[definition.commands.clone()],
                            Some(definition.commands.start),
                            false,
                            parent.transform,
                            self.scissor,
                        )?;
                        self.combine(
                            &child.scopes.last().target,
                            &parent.scopes.last().target.view,
                            &parent.scopes.last().target.view,
                            1.,
                            0.,
                            CompositeOperation::Transition(0.),
                        )?;
                        parent.pending = Some(Pending::Transition {
                            definition,
                            previous: Some(finished),
                        });
                        push_stack(&mut streams, child, |bytes| self.transient(bytes))?;
                    }
                    Pending::Transition {
                        definition,
                        previous: Some(previous),
                    } => {
                        self.combine_transition(
                            &parent.scopes.last().target,
                            &previous.view,
                            &finished.view,
                            definition,
                            parent.transform,
                        )?;
                        self.recycle_target(previous)?;
                        self.recycle_target(finished)?;
                    }
                }
                continue;
            };
            current.next += 1;
            let at = command.origin.at;
            let transform = current.transform;
            self.scissor = current.scopes.last().scissor;
            self.mesh_clip = current.scopes.last().mesh_clip;
            match &command.op {
                FrameOp::PushContext(_) | FrameOp::PopContext => (),
                FrameOp::PushClip(clip) => {
                    let scissor =
                        Scissor::from_clip(clip, transform, self.size, self.quality.samples);
                    let mesh_clip = (scissor.is_none() && self.quality.samples == 1)
                        .then(|| {
                            crate::common::scopes::flat_clip(
                                current.drawing,
                                current.commands,
                                current.next,
                                current.offset,
                                current.transitions_enabled,
                                self.capture,
                            )
                            .then(|| {
                                crate::common::mesh::RectClip::from_clip(clip, transform, self.size)
                            })
                            .flatten()
                        })
                        .flatten();
                    let isolated = scissor.is_none() && mesh_clip.is_none();
                    current.scopes.push(
                        Kind::Clip,
                        at,
                        Scope {
                            target: if !isolated {
                                current.scopes.last().target.clone()
                            } else {
                                self.target()?
                            },
                            clip: isolated.then_some(clip),
                            mesh_clip: mesh_clip.or(self.mesh_clip),
                            mask: None,
                            image_mask: None,
                            effect: None,
                            mask_effect: None,
                            source_parent: None,
                            source_view: None,
                            opacity: 1.,
                            invert: 0.,
                            blend: Default::default(),
                            scissor: scissor
                                .or_else(|| Scissor::clip_bounds(clip, transform, self.size))
                                .map_or(self.scissor, |s| self.scissor.intersect(s)),
                            isolated,
                        },
                        |bytes| self.transient(bytes),
                    )?;
                }
                FrameOp::PushGroup(group) => {
                    let boundary = current.transitions_enabled
                        && current
                            .drawing
                            .transitions()
                            .get(current.transition_next)
                            .is_some_and(|t| {
                                t.commands.start < current.offset.unwrap() + current.next + 2
                            })
                        || self.capture
                            && current.offset.is_some_and(|offset| {
                                current
                                    .drawing
                                    .capture_exclusion_end(offset + current.next)
                                    .is_some()
                            });
                    if !boundary
                        && let Some((draw, draw_at)) = crate::common::scopes::inline_group_fill(
                            group,
                            &current.commands[current.next..],
                        )
                    {
                        self.draw(
                            current.drawing,
                            &current.scopes.last().target,
                            &draw,
                            transform,
                            draw_at,
                        )?;
                        current.next += 2;
                        continue;
                    }
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
                    let isolated = !current.groups.can_inline(current.next - 1);
                    let ink = self.glyph_plan.group_ink(group);
                    let output_scissor = Scissor::group_bounds(
                        group,
                        transform,
                        self.size,
                        current.transitions_enabled && !current.drawing.transitions().is_empty(),
                    )
                    .map_or(self.scissor, |bounds| self.scissor.intersect(bounds));
                    let mut source_view = isolated
                        .then(|| {
                            crate::common::source_view::SourceView::group(
                                group,
                                ink,
                                transform,
                                self.size,
                                self.scissor,
                                current.transitions_enabled
                                    && !current.drawing.transitions().is_empty(),
                                self.gpu.device.limits().max_texture_dimension_2d,
                            )
                        })
                        .flatten();
                    let source_parent = if let Some(view) = source_view {
                        self.flush_draws()?;
                        let parent = (current.transform, self.size, view.parent_rect);
                        current.transform = view.frame_to_source(transform);
                        current.size = view.storage_size;
                        self.size = view.storage_size;
                        self.mesh_clip = None;
                        Some(parent)
                    } else if let Some(effect) =
                        group.effect.as_ref().or(group.mask_effect.as_ref())
                    {
                        let source = crate::common::effect_plan::source_layout(
                            effect,
                            group.effect_to_target,
                            transform,
                            self.gpu.device.limits().max_texture_dimension_2d,
                            at,
                        )?;
                        self.flush_draws()?;
                        let parent = (current.transform, self.size, output_scissor);
                        if group.mask_effect.is_some() {
                            source_view = Some(crate::common::source_view::SourceView::local(
                                source.size,
                                if transform.determinant() == 0. {
                                    Affine::scale(0.)
                                } else {
                                    source.frame_to_source * transform.inverse()
                                },
                                output_scissor,
                            ));
                        }
                        current.transform = source.frame_to_source;
                        current.size = source.size;
                        self.size = source.size;
                        self.mesh_clip = None;
                        Some(parent)
                    } else {
                        None
                    };
                    current.scopes.push(
                        Kind::Group,
                        at,
                        Scope {
                            target: if isolated {
                                self.target()?
                            } else {
                                current.scopes.last().target.clone()
                            },
                            mesh_clip: self.mesh_clip,
                            source_parent,
                            source_view,
                            clip: group.clip.as_ref(),
                            mask: group.mask,
                            effect: group
                                .effect
                                .as_ref()
                                .map(|effect| (effect, transform * group.effect_to_target)),
                            mask_effect: group
                                .mask_effect
                                .as_ref()
                                .map(|effect| (effect, transform * group.effect_to_target)),
                            image_mask: group
                                .image_mask
                                .as_ref()
                                .map(|mask| (mask, transform * group.effect_to_target)),
                            opacity: group.composite.alpha,
                            invert: group.invert,
                            blend: group.composite.blend,
                            scissor: if let Some(view) = source_view {
                                view.source_rect
                            } else if source_parent.is_some() {
                                Scissor::full(self.size)
                            } else {
                                output_scissor
                            },
                            isolated,
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
                    if group.isolated {
                        self.flush_draws()?;
                    }
                    let mut source = group.target;
                    if let Some((coverage, to_parent)) = group.mask_effect {
                        let view = group.source_view.expect("local coverage source");
                        self.scissor = group.scissor;
                        if let Some((effect, to_parent)) = group.effect {
                            let processed = self.effect(
                                current.drawing,
                                effect,
                                view.parent_to_source * to_parent,
                                &source,
                                entry.at,
                            )?;
                            self.recycle_target(source)?;
                            source = processed;
                        }
                        let mask = self.effect_coverage(
                            current.drawing,
                            coverage,
                            view.parent_to_source * to_parent,
                            &source,
                            Affine::IDENTITY,
                            entry.at,
                        )?;
                        let covered = self.target()?;
                        self.combine(
                            &covered,
                            &source.view,
                            &mask.view,
                            1.,
                            0.,
                            CompositeOperation::AlphaMask,
                        )?;
                        self.recycle_target(source)?;
                        self.recycle_target(mask)?;
                        source = covered;
                    }
                    if let Some((outer, size, _)) = group.source_parent {
                        current.transform = outer;
                        current.size = size;
                        self.size = size;
                    }
                    let transform = current.transform;
                    self.scissor = current.scopes.last().scissor;
                    self.mesh_clip = current.scopes.last().mesh_clip;
                    if !group.isolated {
                        continue;
                    }
                    self.scissor = self.scissor.intersect(
                        group
                            .source_parent
                            .map_or(group.scissor, |(_, _, scissor)| scissor),
                    );
                    if let Some(view) = group.source_view.filter(|_| group.mask_effect.is_none()) {
                        self.combine_view(
                            &current.scopes.last().target,
                            &source,
                            view,
                            group.opacity,
                            group.invert,
                        )?;
                        self.recycle_target(source)?;
                        continue;
                    }
                    if let Some(view) = group.source_view.filter(|_| group.mask_effect.is_some()) {
                        let projected = self.target()?;
                        self.project_source(&projected, &source, view.parent_to_source, entry.at)?;
                        self.recycle_target(source)?;
                        source = projected;
                    } else if let Some((effect, transform)) = group.effect {
                        let processed =
                            self.effect(current.drawing, effect, transform, &source, entry.at)?;
                        self.recycle_target(source)?;
                        source = processed;
                    }
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
                                reel_ui::frame::GroupComposite {
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
                        let definition = current.drawing.masks().get(&mask.mask).ok_or(
                            GpuError::Unsupported {
                                at,
                                record: "missing mask",
                            },
                        )?;
                        current.pending = Some(Pending::Mask(MaskComposition {
                            scissor: self.scissor,
                            image_mask: group.image_mask,
                            source,
                            opacity: group.opacity,
                            invert: group.invert,
                            blend: group.blend,
                            operation: match definition.mode {
                                MaskMode::Alpha => CompositeOperation::AlphaMask,
                                MaskMode::Luminance => CompositeOperation::LuminanceMask,
                            },
                        }));
                        let child = self.start_stream(
                            current.drawing,
                            &definition.commands,
                            None,
                            false,
                            transform * mask.transform,
                            self.scissor,
                        )?;
                        push_stack(&mut streams, child, |bytes| self.transient(bytes))?;
                    } else {
                        self.combine_group(
                            &current.scopes.last().target,
                            &source.view,
                            &source.view,
                            reel_ui::frame::GroupComposite {
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
                    let mut batch_end = if current.transitions_enabled {
                        current
                            .drawing
                            .transitions()
                            .get(current.transition_next)
                            .map_or(current.commands.len(), |t| {
                                t.commands
                                    .start
                                    .saturating_sub(current.offset.unwrap())
                                    .min(current.commands.len())
                            })
                    } else {
                        current.commands.len()
                    };
                    if self.capture
                        && let Some(offset) = current.offset
                    {
                        let exclusions = current.drawing.capture_exclusions();
                        let next =
                            exclusions.partition_point(|range| range.end < offset + current.next);
                        if let Some(range) = exclusions.get(next) {
                            batch_end = batch_end.min(range.start.saturating_sub(offset));
                        }
                    }
                    let consumed = self.image_batch(
                        &current.scopes.last().target,
                        &current.commands[current.next - 1..batch_end],
                        transform,
                    )?;
                    if consumed == 0 {
                        self.draw(
                            current.drawing,
                            &current.scopes.last().target,
                            draw,
                            transform,
                            at,
                        )?;
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
        drawing: &'a FrameDrawing,
        commands: &'a [FrameCommand],
        offset: Option<usize>,
        transitions_enabled: bool,
        transform: Affine,
        scissor: Scissor,
    ) -> Result<Stream<'a>, GpuError> {
        let capture = self.capture;
        let groups = crate::common::scopes::GroupPlan::new(
            commands,
            offset,
            drawing
                .transitions()
                .iter()
                .filter(|_| transitions_enabled)
                .map(|t| t.commands.clone()),
            drawing
                .capture_exclusions()
                .iter()
                .filter(|_| capture)
                .cloned(),
            |bytes| self.transient(bytes),
        )?;
        Ok(Stream {
            drawing,
            offset,
            transitions_enabled,
            transition_next: drawing
                .transitions()
                .partition_point(|t| t.commands.start < offset.unwrap_or(0)),
            commands,
            groups,
            next: 0,
            size: self.size,
            transform,
            scopes: Scopes::new(Scope {
                target: self.target()?,
                mesh_clip: None,
                clip: None,
                mask: None,
                image_mask: None,
                effect: None,
                mask_effect: None,
                source_parent: None,
                source_view: None,
                opacity: 1.,
                invert: 0.,
                blend: Default::default(),
                scissor,
                isolated: true,
            }),
            pending: None,
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
        self.flush_draws()?;
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
                source_origin: [0; 2],
            },
        );
        Ok(())
    }
    fn combine_view(
        &mut self,
        target: &Target,
        source: &Target,
        view: crate::common::source_view::SourceView,
        alpha: f32,
        invert: f32,
    ) -> Result<(), GpuError> {
        self.flush_draws()?;
        if self.scissor.empty() {
            return Ok(());
        }
        self.transient(32)?;
        self.pipelines.composite(
            &self.gpu.device,
            &mut self.encoder,
            target,
            super::pipeline::CompositeInputs {
                source: &source.view,
                mask: &source.view,
                backdrop: &source.view,
            },
            super::pipeline::CompositeParams {
                scissor: self.scissor,
                opacity: alpha,
                invert,
                operation: CompositeOperation::SourceOver,
                blend: 0,
                source_origin: view.origin(),
            },
        );
        Ok(())
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "one composite pass borrows its target, sampled textures and node properties"
    )]
    fn combine_group(
        &mut self,
        target: &Target,
        source: &wgpu::TextureView,
        mask: &wgpu::TextureView,
        composite: reel_ui::frame::GroupComposite,
        invert: f32,
        operation: CompositeOperation,
        image_mask: Option<(&reel_ui::frame::FrameImageMask, Affine)>,
    ) -> Result<(), GpuError> {
        self.flush_draws()?;
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
            source_origin: [0; 2],
        };
        if let Some((definition, uv)) = image_mask {
            let pipeline = self.pipelines.masks.get(
                self.gpu,
                self.quality,
                &definition.program,
                definition.parameters,
                &self.budgets.resources,
            )?;
            self.transient(
                pipeline.parameter_cost(definition.images.len())
                    + ((8 + definition.images.len()) * size_of::<wgpu::BindGroupEntry>()
                        + definition.images.len() * size_of::<&wgpu::TextureView>())
                        as u64,
            )?;
            let images = definition
                .images
                .iter()
                .map(|handle| self.images[handle].as_ref())
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
    fn combine_transition(
        &mut self,
        target: &Target,
        old: &wgpu::TextureView,
        new: &wgpu::TextureView,
        transition: &SceneTransition,
        outer: Affine,
    ) -> Result<(), GpuError> {
        self.flush_draws()?;
        if self.scissor.empty() {
            return Ok(());
        }
        let Some(definition) = transition
            .mask
            .as_ref()
            .filter(|_| transition.progress > 0. && transition.progress < 1.)
        else {
            return self.combine(
                target,
                old,
                new,
                1.,
                0.,
                CompositeOperation::Transition(transition.progress),
            );
        };
        let transform = outer * transition.mask_to_window;
        if definition.bounds.width() <= 0.
            || definition.bounds.height() <= 0.
            || transform.determinant() == 0.
        {
            return self.combine(target, old, new, 1., 0., CompositeOperation::Transition(0.));
        }
        let uv = Affine::scale_non_uniform(
            1. / definition.bounds.width(),
            1. / definition.bounds.height(),
        ) * Affine::translate((-definition.bounds.x0, -definition.bounds.y0))
            * transform.inverse();
        let pipeline = self.pipelines.masks.get(
            self.gpu,
            self.quality,
            &definition.program,
            definition.parameters,
            &self.budgets.resources,
        )?;
        self.transient(
            pipeline.parameter_cost(definition.images.len())
                + ((8 + definition.images.len()) * size_of::<wgpu::BindGroupEntry>()
                    + definition.images.len() * size_of::<&wgpu::TextureView>())
                    as u64,
        )?;
        let images = definition
            .images
            .iter()
            .map(|handle| self.images[handle].as_ref())
            .collect();
        pipeline.draw(
            &self.gpu.device,
            &mut self.encoder,
            target,
            super::mask::Inputs {
                source: old,
                mask: new,
                backdrop: old,
                images,
            },
            super::pipeline::CompositeParams {
                scissor: self.scissor,
                opacity: 1.,
                invert: 0.,
                operation: CompositeOperation::MaskedTransition(transition.progress),
                blend: 0,
                source_origin: [0; 2],
            },
            definition,
            uv,
            &self.pipelines.samplers,
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

pub(super) fn blend_code(blend: reel_ui::frame::BlendMode) -> Option<u32> {
    use reel_ui::frame::BlendMode;
    use reel_ui::peniko::{Compose, Mix};
    let BlendMode::Standard(blend) = blend else {
        return Some(4);
    };
    match (blend.mix, blend.compose) {
        (Mix::Normal, Compose::SrcOver) => Some(0),
        (Mix::Normal, Compose::PlusLighter) => Some(1),
        (Mix::Multiply, Compose::SrcOver) => Some(2),
        (Mix::Screen, Compose::SrcOver) => Some(3),
        _ => None,
    }
}
