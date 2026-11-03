mod effect_cache;
mod effect_inputs;
mod effect_resolution;
mod effect_tail;
mod source_crop;
mod targets;
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
mod backdrop;
mod schedule;
mod source_free;
mod transitions;
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
    pub(super) offscreen: crate::common::offscreen::Plan,
    pub(super) effects: Vec<Arc<super::effect::Compiled>>,
    pub(super) effect_results: &'a crate::common::effect_result::Cache<super::retained::Output>,
    pub(super) effect_admission: crate::common::effect_result::Admission,
    pub(super) effect_pending: Vec<crate::common::effect_result::Pending<super::retained::Output>>,
    pub(super) effect_pins: Vec<Target>,
    pub(super) effect_tails: Vec<effect_tail::Tail>,
    pub(super) effect_inputs: Vec<(crate::common::effect_result::Token, super::retained::Output)>,
    pub(super) effect_uniform_cache: &'a super::effect_uniform::Cache,
    pub(super) effect_uniform_slots: Vec<super::effect_uniform::Storage>,
    pub(super) white: Option<Arc<resources::Image>>,
    pub(super) target_cache: &'a super::target_cache::TargetCache,
    pub(super) coverage_target_cache: &'a super::target_cache::TargetCache,
    pub(super) coverage_quality: Option<Quality>,
    pub(super) coverage_pipeline: Option<Arc<wgpu::RenderPipeline>>,
    pub(super) retained_cache: &'a crate::common::retained::Cache<super::retained::Output, Quality>,
    pub(super) selection: Arc<crate::common::retained::Plan>,
    pub(super) retained_inputs: Vec<Arc<super::target_cache::Allocation>>,
    pub(super) warm: Option<usize>,
    pub(super) warmed: bool,
    pub(super) target_allocations: Vec<Arc<super::target_cache::Allocation>>,
    pub(super) available_targets: Vec<Target>,
    pub(super) target_writes: crate::common::target_writes::TargetWrites,
    pub(super) scissor: Scissor,
    pub(super) mesh_clip: Option<crate::common::mesh::RectClip>,
    pub(super) instance_batch: Option<super::draw::InstanceBatch>,
    #[cfg(test)]
    pub(super) sprite_instances_enabled: bool,
    pub(super) paint_batch: Option<super::draw::PaintBatch>,
    pub(super) geometry_policy: &'a super::geometry::UploadPolicy,
    pub(super) geometry_cache: &'a super::geometry::Cache,
    pub(super) geometry: Vec<super::geometry::Storage>,
}
struct Scope<'a> {
    source_input: Option<(usize, Affine, Scissor)>,
    clip_child: Option<&'a reel_ui::frame::FrameGroup>,
    delayed: Option<(&'a reel_ui::frame::FrameEffect, Affine, RecordLocation, f32)>,
    group: Option<&'a reel_ui::frame::FrameGroup>,
    source_effect_transform: Option<Affine>,
    scheduled: Option<bool>,
    shared_source: bool,
    target: Option<Target>,
    effect_input: Option<usize>,
    clip: Option<&'a Clip>,
    mask: Option<reel_ui::frame::MaskUse>,
    image_mask: Option<(&'a reel_ui::frame::FrameImageMask, Affine)>,
    effect: Option<(&'a reel_ui::frame::FrameEffect, Affine)>,
    mask_effect: Option<(&'a reel_ui::frame::FrameEffect, Affine)>,
    direct_mask_projection: bool,
    source_parent: Option<(Affine, [u32; 2], Scissor)>,
    source_view: Option<crate::common::source_view::SourceView>,
    opacity: f32,
    invert: f32,
    blend: reel_ui::frame::BlendMode,
    scissor: Scissor,
    mesh_clip: Option<crate::common::mesh::RectClip>,
    isolated: bool,
    forward_source: bool,
    early_reusable: bool,
}
impl Scope<'_> {
    fn target(&self) -> &Target {
        self.target.as_ref().expect("resolved source input")
    }
}
struct MaskComposition<'a> {
    terminal_effect: Option<(&'a reel_ui::frame::FrameEffect, Affine, RecordLocation)>,
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
        entry: Option<usize>,
        cached: bool,
    },
}
#[derive(Clone)]
struct DeferredComposite<'a> {
    push: usize,
    source: Target,
    view: Option<crate::common::source_view::SourceView>,
    effect: Option<(&'a reel_ui::frame::FrameEffect, Affine, RecordLocation)>,
    size: [u32; 2],
    mesh_clip: Option<crate::common::mesh::RectClip>,
    opacity: f32,
    invert: f32,
    scissor: Scissor,
}
struct Stream<'a> {
    source_crop: Option<crate::common::source_crop::Planned>,
    retained_context: usize,
    scheduled: bool,
    deferred: Vec<DeferredComposite<'a>>,
    drawing: &'a FrameDrawing,
    // Masks have their own command space; frame exclusions apply to drawing ranges.
    offset: Option<usize>,
    transition_next: usize,
    transitions_enabled: bool,
    commands: &'a [FrameCommand],
    groups: crate::common::scopes::Proof,
    next: usize,
    size: [u32; 2],
    transform: Affine,
    scopes: Scopes<Scope<'a>>,
    pending: Option<Pending<'a>>,
}
impl Context<'_> {
    pub(super) fn stream(
        &mut self,
        frame: &FrameDrawing,
        transform: Affine,
    ) -> Result<Target, GpuError> {
        self.geometry_cache.begin_replay();
        let mut first = self.start_stream(
            frame,
            frame.commands(),
            Some(0),
            true,
            transform,
            Scissor::full(self.size),
        )?;
        first.scheduled = !self.offscreen.is_empty();
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
                let retained = self
                    .selection
                    .entry(current.retained_context, transition)
                    .map(|(index, entry)| {
                        (
                            index,
                            entry
                                .input
                                .as_ref()
                                .map(|input| self.retained_cache.output(input)),
                        )
                    });
                let (entry, cached) =
                    retained.map_or((None, None), |(index, output)| (Some(index), output));
                if let Some(previous) = cached {
                    let target = previous.target;
                    let mut inputs = std::mem::take(&mut self.retained_inputs);
                    let result = push_stack(&mut inputs, previous.allocation, |bytes| {
                        self.transient(bytes)
                    });
                    self.retained_inputs = inputs;
                    result?;
                    let child = self.start_stream(
                        current.drawing,
                        &current.drawing.commands()[transition.commands.clone()],
                        Some(transition.commands.start),
                        false,
                        current.transform,
                        self.scissor,
                    )?;
                    self.combine(
                        child.scopes.last().target(),
                        &current.scopes.last().target().view,
                        &current.scopes.last().target().view,
                        1.,
                        0.,
                        CompositeOperation::Transition(0.),
                    )?;
                    current.pending = Some(Pending::Transition {
                        definition: transition,
                        previous: Some(target),
                        entry,
                        cached: true,
                    });
                    push_stack(&mut streams, child, |bytes| self.transient(bytes))?;
                    continue;
                }
                let mut child = self.start_stream(
                    transition.previous.drawing(),
                    transition.previous.drawing().commands(),
                    Some(0),
                    true,
                    current.transform
                        * transition.parent_to_window
                        * transition.previous.to_parent(),
                    self.scissor,
                )?;
                child.retained_context = entry.map_or(0, |index| index + 1);
                self.combine(
                    child.scopes.last().target(),
                    &current.scopes.last().target().view,
                    &current.scopes.last().target().view,
                    1.,
                    0.,
                    CompositeOperation::Transition(0.),
                )?;
                current.pending = Some(Pending::Transition {
                    definition: transition,
                    previous: None,
                    entry,
                    cached: false,
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
                let finished = streams
                    .pop()
                    .unwrap()
                    .scopes
                    .finish()?
                    .target
                    .expect("completed frame source");
                self.ensure_sampled(&finished.view);
                let Some(parent) = streams.last_mut() else {
                    return Ok(finished);
                };
                self.size = parent.size;
                self.scissor = parent.scopes.last().scissor;
                match parent.pending.take().expect("suspended composition") {
                    Pending::Mask(mask) => {
                        self.scissor = self.scissor.intersect(mask.scissor);
                        if let Some((effect, transform, at)) = mask.terminal_effect {
                            self.effect_into(
                                parent.drawing,
                                effect,
                                transform,
                                &mask.source,
                                parent.scopes.last().target(),
                                mask.opacity,
                                Some((&finished, false)),
                                at,
                            )?;
                        } else {
                            self.combine_group(
                                parent.scopes.last().target(),
                                &mask.source.view,
                                &finished.view,
                                reel_ui::frame::GroupComposite {
                                    alpha: mask.opacity,
                                    blend: mask.blend,
                                },
                                mask.invert,
                                mask.operation,
                                mask.image_mask,
                                [0; 2],
                            )?;
                        }
                        self.recycle_target(mask.source)?;
                        self.recycle_target(finished)?;
                    }
                    Pending::Transition {
                        definition,
                        previous: None,
                        entry,
                        cached,
                    } => {
                        if entry.is_some() && entry == self.selection.warm {
                            self.warmed = true;
                            return Ok(finished);
                        }
                        let child = self.start_stream(
                            parent.drawing,
                            &parent.drawing.commands()[definition.commands.clone()],
                            Some(definition.commands.start),
                            false,
                            parent.transform,
                            self.scissor,
                        )?;
                        self.combine(
                            child.scopes.last().target(),
                            &parent.scopes.last().target().view,
                            &parent.scopes.last().target().view,
                            1.,
                            0.,
                            CompositeOperation::Transition(0.),
                        )?;
                        parent.pending = Some(Pending::Transition {
                            definition,
                            previous: Some(finished),
                            entry,
                            cached,
                        });
                        push_stack(&mut streams, child, |bytes| self.transient(bytes))?;
                    }
                    Pending::Transition {
                        definition,
                        previous: Some(previous),
                        cached,
                        ..
                    } => {
                        self.combine_transition(
                            parent.scopes.last().target(),
                            &previous.view,
                            &finished.view,
                            definition,
                            parent.transform,
                        )?;
                        if !cached {
                            self.recycle_target(previous)?;
                        }
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
                            source_input: None,
                            group: None,
                            clip_child: isolated
                                .then(|| {
                                    crate::common::effect_plan::clipped_effect(
                                        current.drawing,
                                        current.commands,
                                        current.next,
                                        self.quality.samples,
                                        self.capture || !current.drawing.transitions().is_empty(),
                                    )
                                })
                                .flatten(),
                            delayed: None,
                            scheduled: None,
                            shared_source: false,
                            target: if !isolated {
                                current.scopes.last().target.clone()
                            } else {
                                Some(self.target()?)
                            },
                            effect_input: None,
                            clip: isolated.then_some(clip),
                            mesh_clip: mesh_clip.or(self.mesh_clip),
                            mask: None,
                            image_mask: None,
                            effect: None,
                            mask_effect: None,
                            direct_mask_projection: false,
                            source_parent: None,
                            source_effect_transform: None,
                            source_view: None,
                            opacity: 1.,
                            invert: 0.,
                            blend: Default::default(),
                            scissor: scissor
                                .or_else(|| Scissor::clip_bounds(clip, transform, self.size))
                                .map_or(self.scissor, |s| self.scissor.intersect(s)),
                            isolated,
                            forward_source: false,
                            early_reusable: false,
                        },
                        |bytes| self.transient(bytes),
                    )?;
                }
                FrameOp::PushGroup(group) => {
                    if let Some(consumed) = crate::common::scopes::invisible_group(
                        group,
                        &current.commands[current.next..],
                        transform,
                        self.size,
                        self.scissor,
                        current.transitions_enabled && !current.drawing.transitions().is_empty()
                            || self.capture && !current.drawing.capture_exclusions().is_empty(),
                    ) {
                        current.next += consumed;
                        continue;
                    }
                    if let Some(offset) = current.offset
                        && let Some(end) = self
                            .selection
                            .sources
                            .end(current.drawing, offset + current.next - 1)
                    {
                        self.source_free_group(current, group, at)?;
                        current.next = end - offset;
                        continue;
                    }
                    if let Some(fill) = crate::common::scopes::inline_group_fill(
                        group,
                        &current.commands[current.next..],
                        transform,
                        self.size,
                        self.quality.samples,
                    ) && !fill.crosses_boundary(
                        current.drawing,
                        current.offset,
                        current.next,
                        current.transitions_enabled,
                        self.capture,
                    ) {
                        if !current.draw_selected(&self.selection, current.next + fill.command) {
                            current.next += fill.consumed;
                            continue;
                        }
                        self.scissor = self.scissor.intersect(fill.scissor);
                        self.draw(
                            current.drawing,
                            current.scopes.last().target(),
                            &fill.draw,
                            transform,
                            reel_ui::frame::CommandOrigin {
                                source: 0,
                                local: reel_ui::frame::LocalId::None,
                                at: fill.at,
                            },
                        )?;
                        current.next += fill.consumed;
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
                    let source_push = current.next - 1;
                    let isolated = !current.groups.can_inline(source_push);
                    let batch = current
                        .scheduled
                        .then(|| self.offscreen.entry(current.next - 1))
                        .flatten()
                        .copied();
                    if let Some(entry) = batch
                        && let Some(canonical) = entry.canonical_push
                    {
                        let mut item = current
                            .deferred
                            .iter()
                            .find(|item| item.push == canonical)
                            .expect("planned source remains live through its window")
                            .clone();
                        item.push = entry.push;
                        current.next = entry.end;
                        self.defer_composite(current, item, entry.last, false)?;
                        continue;
                    }
                    let ink = self.glyph_plan.group_ink(group);
                    let effect_transform = crate::common::effect_plan::output_transform(
                        group,
                        current
                            .scopes
                            .last()
                            .group
                            .zip(current.scopes.last().source_effect_transform),
                        transform,
                    );
                    let output_scissor = Scissor::group_bounds(
                        group,
                        transform,
                        self.size,
                        current.transitions_enabled && !current.drawing.transitions().is_empty(),
                    )
                    .map_or(self.scissor, |bounds| self.scissor.intersect(bounds));
                    let mut source_view = batch.and_then(|entry| entry.view).or_else(|| {
                        isolated
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
                            .flatten()
                    });
                    let mut forward_source = false;
                    let mut effect_source_scissor = None;
                    let mut source_crop = None;
                    let mut defer_source = false;
                    let mut source_effect_transform = None;
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
                        let parent = current.scopes.last();
                        let source = if crate::common::effect_plan::same_source_domain(
                            group,
                            parent.group,
                        ) {
                            crate::common::effect_plan::SourceLayout {
                                size: self.size,
                                frame_to_source: transform,
                                effect_to_source: effect_transform,
                            }
                        } else {
                            crate::common::effect_plan::source_layout(
                                effect,
                                group.effect_to_target,
                                transform,
                                self.gpu.device.limits().max_texture_dimension_2d,
                                at,
                            )?
                        };
                        effect_source_scissor =
                            Some(crate::common::effect_plan::roi::source_scissor(
                                current.drawing,
                                group,
                                effect_transform,
                                self.size,
                                output_scissor,
                                source.size,
                                self.offscreen.storage_size(source.size),
                            ));
                        forward_source = current.groups.is_source_child(current.next - 1)
                            && current.scopes.last().isolated
                            && crate::common::effect_plan::forwards_source(
                                group,
                                &source,
                                transform,
                                self.size,
                                current.scopes.last().shared_source,
                            );
                        (defer_source, source_crop) = self.source_crop(
                            current,
                            &source,
                            effect_source_scissor.unwrap(),
                            isolated && !forward_source && batch.is_none(),
                        );
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
                        self.enter_source_raster(
                            current,
                            &source,
                            source_crop,
                            &mut effect_source_scissor,
                        );
                        source_effect_transform =
                            Some(source_crop.map_or(source.effect_to_source, |crop| {
                                crop.transform(source.effect_to_source)
                            }));
                        Some(parent)
                    } else {
                        None
                    };
                    let source_scissor = if let Some(view) = source_view {
                        view.source_rect
                    } else if source_parent.is_some() {
                        effect_source_scissor.unwrap_or(Scissor::full(self.size))
                    } else {
                        output_scissor
                    };
                    let source_key = current.offset.map(|offset| {
                        crate::common::effect_inputs::Key::new(
                            current.drawing,
                            offset + source_push,
                            crate::common::effect_inputs::Raster::new(
                                current.transform,
                                self.size,
                                self.offscreen.storage_size(self.size),
                                source_scissor,
                            ),
                        )
                    });
                    let effect_input = current
                        .scopes
                        .last()
                        .effect_input
                        .filter(|_| forward_source)
                        .or_else(|| {
                            source_key.and_then(|key| self.offscreen.effect_inputs().root(&key))
                        });
                    if effect_input.is_some() {
                        // The selected content replaces this source subtree,
                        // including a deferred physical crop's original writer.
                        current.source_crop = None;
                    }
                    let selected_input = effect_input.and_then(|index| {
                        let entry = self.offscreen.effect_inputs().entry(index);
                        (Some(entry.input) == source_key)
                            .then(|| (self.effect_input(entry), entry.resume))
                    });
                    let cached_source = if effect_input.is_none()
                        && forward_source
                        && current.source_crop.is_none()
                    {
                        self.cached_source(
                            current,
                            group,
                            source_parent.unwrap(),
                            source_scissor,
                            source_crop,
                        )?
                    } else {
                        Default::default()
                    };
                    if cached_source.hit {
                        continue;
                    }
                    if let Some((_, resume)) = &selected_input {
                        current.next = resume - current.offset.unwrap();
                        #[cfg(feature = "profiling")]
                        {
                            crate::profiling::record_effect_input_event(
                                crate::profiling::EffectInputEvent::InputInstalls,
                                1,
                            );
                            let entry = self.offscreen.effect_inputs().entry(effect_input.unwrap());
                            if let FrameOp::PushGroup(producer) =
                                &current.drawing.commands()[entry.commands.start].op
                            {
                                crate::profiling::record_effect(crate::profiling::EffectSample {
                                    definition: producer.effect.as_ref().unwrap().definition,
                                    variant: 3,
                                    hits: 1,
                                    early_hits: 1,
                                    ..Default::default()
                                });
                            }
                        }
                    }
                    current.scopes.push(
                        Kind::Group,
                        at,
                        Scope {
                            source_input: current.offset.map(|offset| {
                                (offset + source_push, current.transform, source_scissor)
                            }),
                            group: Some(group),
                            clip_child: None,
                            delayed: None,
                            scheduled: batch.map(|entry| entry.last),
                            shared_source: batch.is_some_and(|entry| entry.shared),
                            forward_source,
                            early_reusable: cached_source.early_reusable
                                || effect_input.is_some() && forward_source,
                            effect_input,
                            target: if effect_input.is_some() {
                                selected_input.map(|(target, _)| target)
                            } else if defer_source {
                                current.scopes.last().target.clone()
                            } else if let Some(crop) = source_crop {
                                Some(self.cropped_target(crop)?)
                            } else if forward_source {
                                current.scopes.last().target.clone()
                            } else if batch.is_some_and(|entry| entry.shared && !entry.first) {
                                Some(
                                    current
                                        .deferred
                                        .last()
                                        .expect("active source atlas")
                                        .source
                                        .clone(),
                                )
                            } else if isolated {
                                Some(self.target()?)
                            } else {
                                current.scopes.last().target.clone()
                            },
                            mesh_clip: self.mesh_clip,
                            source_parent,
                            source_effect_transform,
                            source_view,
                            clip: group.clip.as_ref(),
                            mask: group.mask,
                            effect: group
                                .effect
                                .as_ref()
                                .map(|effect| (effect, effect_transform)),
                            mask_effect: group
                                .mask_effect
                                .as_ref()
                                .map(|effect| (effect, effect_transform)),
                            direct_mask_projection:
                                crate::common::effect_plan::direct_mask_projection(
                                    group,
                                    current.transitions_enabled
                                        && !current.drawing.transitions().is_empty(),
                                ),
                            image_mask: group
                                .image_mask
                                .as_ref()
                                .map(|mask| (mask, effect_transform)),
                            opacity: group.composite.alpha,
                            invert: group.invert,
                            blend: group.composite.blend,
                            scissor: source_scissor,
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
                    let source_commands = current.source_commands(&group, self.capture);
                    if group.isolated {
                        self.flush_draws()?;
                    }
                    let terminal_tail = group.group.and_then(|definition| {
                        crate::common::effect_plan::terminal_tail(
                            current.drawing,
                            definition,
                            self.quality.samples,
                            self.capture || !current.drawing.transitions().is_empty(),
                        )
                    });
                    let mut terminal_effect = group
                        .delayed
                        .map(|(effect, transform, at, _)| (effect, transform, at));
                    let terminal_opacity = group
                        .delayed
                        .map_or(group.opacity, |(_, _, _, opacity)| opacity);
                    let nested = group.delayed.is_some();
                    let mut source = group.target.expect("completed group source");
                    if let Some((coverage, to_parent)) = group.mask_effect {
                        let view = group.source_view.expect("local coverage source");
                        self.scissor = group.scissor;
                        if let Some((effect, to_parent)) = group.effect {
                            let processed = self.effect(
                                current.drawing,
                                effect,
                                view.parent_to_source * to_parent,
                                &source,
                                None,
                                entry.at,
                            )?;
                            self.recycle_target(source)?;
                            source = processed;
                        }
                        let covered = self.effect_mask(
                            current.drawing,
                            coverage,
                            view.parent_to_source * to_parent,
                            &source,
                            Affine::IDENTITY,
                            entry.at,
                        )?;
                        self.recycle_target(source)?;
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
                    if group.forward_source {
                        let (effect, effect_transform) = group.effect.expect("forwarded effect");
                        let processed = self.effect(
                            current.drawing,
                            effect,
                            effect_transform,
                            &source,
                            source_commands,
                            entry.at,
                        )?;
                        self.recycle_target(source)?;
                        current.scopes.last_mut().target = Some(processed);
                        continue;
                    }
                    if let Some(view) = group.source_view.filter(|_| group.mask_effect.is_none()) {
                        if let Some(last) = group.scheduled {
                            self.defer_composite(
                                current,
                                DeferredComposite {
                                    push: group.source_input.expect("scheduled source").0,
                                    source,
                                    view: Some(view),
                                    effect: None,
                                    size: self.size,
                                    mesh_clip: self.mesh_clip,
                                    opacity: group.opacity,
                                    invert: group.invert,
                                    scissor: self.scissor,
                                },
                                last,
                                group.shared_source,
                            )?;
                            continue;
                        }
                        if group.blend == Default::default() {
                            self.combine_view(
                                current.scopes.last().target(),
                                &source,
                                view,
                                group.opacity,
                                group.invert,
                            )?;
                        } else {
                            self.combine_cropped(
                                current.scopes.last().target(),
                                &source,
                                view,
                                reel_ui::frame::GroupComposite {
                                    alpha: group.opacity,
                                    blend: group.blend,
                                },
                                group.invert,
                            )?;
                        }
                        self.recycle_target(source)?;
                        continue;
                    }
                    if let Some(view) = group.source_view.filter(|_| group.mask_effect.is_some()) {
                        if group.direct_mask_projection {
                            self.project_source(
                                current.scopes.last().target(),
                                &source,
                                view.parent_to_source,
                                group.opacity,
                                entry.at,
                            )?;
                            self.recycle_target(source)?;
                            continue;
                        }
                        let projected = self.target()?;
                        self.project_source(
                            &projected,
                            &source,
                            view.parent_to_source,
                            1.,
                            entry.at,
                        )?;
                        self.recycle_target(source)?;
                        source = projected;
                    } else if let Some((effect, transform)) = group.effect {
                        if group.group.is_some_and(|definition| {
                            current
                                .scopes
                                .last()
                                .clip_child
                                .is_some_and(|child| std::ptr::eq(child, definition))
                        }) && crate::common::effect_plan::coverage_retention(
                            source.size,
                            self.size,
                        ) {
                            let parent = current.scopes.last_mut();
                            let empty = parent.target.replace(source).expect("clip source");
                            parent.delayed = Some((effect, transform, entry.at, group.opacity));
                            self.recycle_target(empty)?;
                            continue;
                        }

                        if group.group.is_some_and(|definition| {
                            crate::common::effect_plan::terminal_composite(
                                definition,
                                self.quality.samples,
                                self.capture || !current.drawing.transitions().is_empty(),
                            )
                        }) {
                            if let Some(last) = group.scheduled {
                                self.defer_composite(
                                    current,
                                    DeferredComposite {
                                        push: group
                                            .source_input
                                            .expect("scheduled effect source")
                                            .0,
                                        source,
                                        view: None,
                                        effect: Some((effect, transform, entry.at)),
                                        size: self.size,
                                        mesh_clip: self.mesh_clip,
                                        opacity: group.opacity,
                                        invert: group.invert,
                                        scissor: self.scissor,
                                    },
                                    last,
                                    false,
                                )?;
                                continue;
                            }
                            self.effect_into(
                                current.drawing,
                                effect,
                                transform,
                                &source,
                                current.scopes.last().target(),
                                group.opacity,
                                None,
                                entry.at,
                            )?;
                            self.recycle_target(source)?;
                            continue;
                        }
                        if terminal_tail == Some(crate::effect::OutputTail::AlphaCoverage)
                            && crate::common::effect_plan::coverage_retention(
                                source.size,
                                self.size,
                            )
                        {
                            terminal_effect = Some((effect, transform, entry.at));
                        } else {
                            let processed = self.effect(
                                current.drawing,
                                effect,
                                transform,
                                &source,
                                source_commands,
                                entry.at,
                            )?;
                            self.recycle_target(source)?;
                            source = processed;
                        }
                    }
                    if let Some(clip) = group.clip {
                        let single = self.offscreen.coverage_enabled()
                            && crate::common::target_plan::single_clip(
                                group.group,
                                group.delayed.is_some(),
                            );
                        let coverage = if single {
                            self.coverage_target()?
                        } else {
                            self.target()?
                        };
                        self.clip(&coverage, clip, transform, entry.at)?;
                        if let Some((effect, effect_transform, at)) = terminal_effect {
                            if nested {
                                self.scissor = self.scissor.intersect(group.scissor);
                            }
                            self.effect_into(
                                current.drawing,
                                effect,
                                effect_transform,
                                &source,
                                current.scopes.last().target(),
                                terminal_opacity,
                                Some((&coverage, nested)),
                                at,
                            )?;
                            self.recycle_target(source)?;
                            self.recycle_target(coverage)?;
                            continue;
                        }

                        if group.mask.is_none() {
                            // Fold coverage and opacity into the final source-over pass.
                            // No intermediate full-size clipped texture is needed.
                            self.combine_group(
                                current.scopes.last().target(),
                                &source.view,
                                &coverage.view,
                                reel_ui::frame::GroupComposite {
                                    alpha: group.opacity,
                                    blend: group.blend,
                                },
                                group.invert,
                                CompositeOperation::AlphaMask,
                                group.image_mask,
                                [0; 2],
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
                            terminal_effect,
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
                            current.scopes.last().target(),
                            &source.view,
                            &source.view,
                            reel_ui::frame::GroupComposite {
                                alpha: group.opacity,
                                blend: group.blend,
                            },
                            group.invert,
                            CompositeOperation::SourceOver,
                            group.image_mask,
                            [0; 2],
                        )?;
                        self.recycle_target(source)?;
                    }
                }
                FrameOp::Draw(draw) => {
                    if !current.draw_selected(&self.selection, current.next - 1) {
                        continue;
                    }
                    if current.scheduled && self.offscreen.defer_draw(current.next - 1) {
                        continue;
                    }
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
                    let batch_end =
                        current.draw_batch_end(&self.selection, current.next - 1, batch_end);
                    let consumed = self.image_batch(
                        current.scopes.last().target(),
                        &current.commands[current.next - 1..batch_end],
                        transform,
                    )?;
                    if consumed == 0 {
                        self.draw(
                            current.drawing,
                            current.scopes.last().target(),
                            draw,
                            transform,
                            command.origin,
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
        let groups = if let Some(groups) = self.offscreen.groups(commands, transitions_enabled) {
            groups
        } else {
            crate::common::scopes::Proof::Owned(crate::common::scopes::GroupPlan::new(
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
            )?)
        };
        Ok(Stream {
            source_crop: None,
            retained_context: 0,
            scheduled: false,
            deferred: Vec::new(),
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
                source_input: None,
                group: None,
                clip_child: None,
                delayed: None,
                scheduled: None,
                shared_source: false,
                target: Some(self.target()?),
                effect_input: None,
                mesh_clip: None,
                clip: None,
                mask: None,
                image_mask: None,
                effect: None,
                mask_effect: None,
                direct_mask_projection: false,
                source_parent: None,
                source_effect_transform: None,
                source_view: None,
                opacity: 1.,
                invert: 0.,
                blend: Default::default(),
                scissor,
                isolated: true,
                forward_source: false,
                early_reusable: false,
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
        self.transient(super::pipeline::COMPOSITE_BYTES)?;
        self.ensure_sampled(source);
        if !matches!(operation, CompositeOperation::SourceOver) {
            self.ensure_sampled(mask);
        }
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
                clear: self.target_writes.take(target.slot),
                scissor: self.scissor,
                opacity: alpha,
                invert,
                operation,
                blend: 0,
                source_origin: [0; 2],
                backdrop_origin: [0; 2],
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
        source_origin: [i32; 2],
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
            if !self.copy_backdrop(target, &backdrop, self.scissor, self.scissor) {
                self.combine(
                    &backdrop,
                    &target.view,
                    &target.view,
                    1.,
                    0.,
                    CompositeOperation::SourceOver,
                )?;
            }
            Some(backdrop)
        } else {
            None
        };
        self.ensure_sampled(source);
        self.ensure_sampled(mask);
        let params = super::pipeline::CompositeParams {
            clear: self.target_writes.take(target.slot),
            scissor: self.scissor,
            opacity: composite.alpha,
            invert,
            operation,
            blend,
            source_origin,
            backdrop_origin: [0; 2],
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
            self.transient(super::pipeline::COMPOSITE_BYTES)?;
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
}

pub(super) use crate::common::blend::code as blend_code;
