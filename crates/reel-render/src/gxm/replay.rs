use super::*;
use super::{programs::Values, resources::Target};
use crate::common::{
    renderer::{Budgets, Reservation, reserve},
    scissor::Scissor,
    scopes::{Kind, Scopes, push_stack},
};
use reel_ui::{
    frame::{FrameCommand, FrameDrawing, FrameOp, RecordLocation, SceneTransition},
    imaging::{Composite, MaskMode, record::Clip},
    kurbo::Affine,
};
#[derive(Clone, Copy)]
pub(super) enum CompositeOperation {
    SourceOver,
    AlphaMask,
    LuminanceMask,
    Transition(f32),
    MaskedTransition(f32),
}
struct Composition<'a> {
    source: &'a Target,
    mask: &'a Target,
    backdrop: &'a Target,
    alpha: f32,
    invert: f32,
    operation: CompositeOperation,
    blend: u32,
    expression: Option<&'a Target>,
    source_origin: [i32; 2],
}
struct GroupPaint<'a> {
    source_origin: [i32; 2],
    composite: reel_ui::frame::GroupComposite,
    invert: f32,
    operation: CompositeOperation,
    image_mask: Option<(&'a reel_ui::frame::FrameImageMask, Affine)>,
}

pub(super) struct Context<'a> {
    pub capture: bool,
    pub gpu: &'a super::ownership::Device,
    pub programs: &'a programs::Programs,
    pub limits: crate::common::GpuLimits,
    pub size: [u32; 2],
    pub budgets: &'a Budgets,
    pub budget_scope: Budgets,
    pub tickets: Vec<Reservation>,
    pub failed_native: Vec<super::ownership::NativeTicket>,
    pub images: HashMap<ResourceHandle, Rc<Image>>,
    pub image_cache: &'a mut Images,
    pub glyph_cache: &'a mut draw::GlyphCache,
    pub glyphs: Vec<Rc<draw::Glyph>>,
    pub glyph_uploads: crate::common::glyph_upload::Batch<Rc<draw::Glyph>>,
    pub glyph_plan: crate::common::glyph_plan::Plan,
    pub offscreen: crate::common::offscreen::Plan,
    pub effect_cache: &'a mut super::effect::Cache,
    pub mask_cache: &'a mut crate::common::mask::split::Cache,
    pub effects: Vec<Rc<super::effect::Compiled>>,
    pub target_cache: &'a mut super::target_cache::Cache,
    pub retained_cache: &'a crate::common::retained::Cache<Target, u8>,
    pub selection: std::sync::Arc<crate::common::retained::Plan>,
    pub retained_inputs: Vec<Target>,
    pub warm: Option<usize>,
    pub warmed: bool,
    pub targets: Vec<Target>,
    pub available_targets: Vec<Target>,
    pub target_writes: crate::common::target_writes::TargetWrites,
    pub geometry_cache: &'a mut super::geometry::Cache,
    pub buffers: Vec<super::geometry::Storage>,
    pub paint_batch: Option<draw::PaintBatch>,
    pub scissor: Scissor,
    pub mesh_clip: Option<crate::common::mesh::RectClip>,
    pub at: Option<RecordLocation>,
}
pub(super) struct Work {
    _tickets: Vec<Reservation>,
    _failed_native: Vec<super::ownership::NativeTicket>,
    _images: HashMap<ResourceHandle, Rc<Image>>,
    _glyphs: Vec<Rc<draw::Glyph>>,
    _effects: Vec<Rc<super::effect::Compiled>>,
    _targets: Vec<Target>,
    _retained_inputs: Vec<Target>,
    buffers: std::cell::RefCell<Vec<super::geometry::Storage>>,
}
pub(super) struct Prepared {
    pub output: Target,
    pub retained: Option<usize>,
    _work: Work,
}
impl Prepared {
    pub fn targets(&self) -> &[Target] {
        &self._work._targets
    }
    pub fn recycle_geometry(&self, cache: &mut super::geometry::Cache) {
        for storage in self._work.buffers.borrow_mut().drain(..) {
            cache.put(storage);
        }
    }
}
pub(super) struct Caches<'a> {
    pub images: &'a mut Images,
    pub glyphs: &'a mut draw::GlyphCache,
    pub effects: &'a mut super::effect::Cache,
    pub masks: &'a mut crate::common::mask::split::Cache,
    pub targets: &'a mut super::target_cache::Cache,
    pub geometry: &'a mut super::geometry::Cache,
    pub retained: &'a crate::common::retained::Cache<Target, u8>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "one immutable frame and its native preparation owners"
)]
pub(super) fn prepare(
    device: DeviceResources<'_>,
    programs: &programs::Programs,
    caches: Caches<'_>,
    images: HashMap<ResourceHandle, Rc<Image>>,
    frame: &RenderFrame,
    size: [u32; 2],
    capture: bool,
    selection: std::sync::Arc<crate::common::retained::Plan>,
    warm: Option<usize>,
) -> Result<Prepared, (GpuError, Work)> {
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Replay);
    let DeviceResources {
        gpu,
        budgets,
        limits,
    } = device;
    let Caches {
        images: image_cache,
        glyphs: glyph_cache,
        effects: effect_cache,
        masks: mask_cache,
        targets: target_cache,
        geometry: geometry_cache,
        retained: retained_cache,
    } = caches;
    glyph_cache.begin_preparation();
    let mut context = Context {
        capture,
        gpu,
        programs,
        limits,
        size,
        budgets,
        budget_scope: Budgets::new(limits),
        tickets: Vec::new(),
        failed_native: Vec::new(),
        images,
        image_cache,
        glyph_cache,
        glyphs: Vec::new(),
        glyph_uploads: Default::default(),
        glyph_plan: Default::default(),
        offscreen: Default::default(),
        effect_cache,
        mask_cache,
        effects: Vec::new(),
        target_cache,
        retained_cache,
        selection: selection.clone(),
        retained_inputs: Vec::new(),
        warm,
        warmed: false,
        targets: Vec::new(),
        available_targets: Vec::new(),
        target_writes: Default::default(),
        geometry_cache,
        buffers: Vec::new(),
        paint_batch: None,
        scissor: Scissor::full(size),
        mesh_clip: None,
        at: None,
    };
    let transform = Affine::scale_non_uniform(
        f64::from(size[0]) / frame.info().logical_size[0],
        f64::from(size[1]) / frame.info().logical_size[1],
    );
    let output = crate::common::glyph_plan::Plan::collect(
        std::iter::once(frame),
        transform,
        gpu.capabilities().max_texture_size,
        capture,
        &selection,
        &mut context,
    )
    .and_then(|plan| {
        context.glyph_plan = plan;
        let groups = context
            .target_cache
            .groups
            .get(frame.drawing(), capture, &budgets.transient);
        let (schedule, bytes, retained) = crate::common::offscreen::Plan::choose_with_groups(
            frame,
            None,
            size,
            capture,
            1,
            gpu.capabilities().max_texture_size,
            &context.glyph_plan,
            limits.target_bytes,
            &|extent| Ok(gpu.target_layout(extent)?.texture_bytes),
            crate::common::target_plan::MaskExecution::Sampled,
            &selection,
            groups,
            |bytes| {
                reserve(
                    &budgets.transient,
                    &context.budget_scope.transient,
                    bytes,
                    limits.transient_bytes,
                    "transient",
                )
            },
        )?;
        if bytes > limits.target_bytes {
            return Err(GpuError::Budget {
                domain: "offscreen targets",
                requested: bytes,
                available: limits.target_bytes,
            });
        }
        context.offscreen = schedule;
        let result = context.stream(frame.drawing(), transform);
        drop(std::mem::take(&mut context.offscreen));
        drop(retained);
        result
    });
    // Keep the compatible high-water pool across effect endpoints. Residency
    // pressure and output-size changes retire it at the admission boundary.
    context.glyphs.extend(context.glyph_cache.take_uploads());
    let work = Work {
        _tickets: context.tickets,
        _failed_native: context.failed_native,
        _images: context.images,
        _glyphs: context.glyphs,
        _effects: context.effects,
        _targets: context.targets,
        _retained_inputs: context.retained_inputs,
        buffers: std::cell::RefCell::new(context.buffers),
    };
    match output {
        Ok(output) => Ok(Prepared {
            output,
            retained: context.warmed.then_some(context.warm).flatten(),
            _work: work,
        }),
        Err(error) => Err((error, work)),
    }
}

struct Scope<'a> {
    group: Option<&'a reel_ui::frame::FrameGroup>,
    scheduled: Option<bool>,
    shared_source: bool,
    target: Target,
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
        entry: Option<usize>,
        cached: bool,
    },
}
struct DeferredComposite {
    source: Target,
    view: crate::common::source_view::SourceView,
    opacity: f32,
    invert: f32,
    scissor: Scissor,
}
struct Stream<'a> {
    retained_context: usize,
    scheduled: bool,
    deferred: Vec<DeferredComposite>,
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
                    let mut inputs = std::mem::take(&mut self.retained_inputs);
                    let result =
                        push_stack(&mut inputs, previous.clone(), |bytes| self.transient(bytes));
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
                        &child.scopes.last().target,
                        &current.scopes.last().target,
                        &current.scopes.last().target,
                        1.,
                        0.,
                        CompositeOperation::Transition(0.),
                    )?;
                    current.pending = Some(Pending::Transition {
                        definition: transition,
                        previous: Some(previous),
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
                    &child.scopes.last().target,
                    &current.scopes.last().target,
                    &current.scopes.last().target,
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
                let finished = streams.pop().unwrap().scopes.finish()?.target;
                self.ensure_sampled(&finished.view)?;
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
                            &mask.source,
                            &finished,
                            GroupPaint {
                                source_origin: [0; 2],
                                composite: reel_ui::frame::GroupComposite {
                                    alpha: mask.opacity,
                                    blend: mask.blend,
                                },
                                invert: mask.invert,
                                operation: mask.operation,
                                image_mask: mask.image_mask,
                            },
                        )?;
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
                            &child.scopes.last().target,
                            &parent.scopes.last().target,
                            &parent.scopes.last().target,
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
                            &parent.scopes.last().target,
                            &previous,
                            &finished,
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
            self.at = Some(at);
            let transform = current.transform;
            self.scissor = current.scopes.last().scissor;
            self.mesh_clip = current.scopes.last().mesh_clip;
            match &command.op {
                FrameOp::PushContext(_) | FrameOp::PopContext => (),
                FrameOp::PushClip(clip) => {
                    let scissor = Scissor::from_clip(clip, transform, self.size, 1);
                    let mesh_clip = scissor
                        .is_none()
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
                            group: None,
                            scheduled: None,
                            shared_source: false,
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
                            direct_mask_projection: false,
                            source_parent: None,
                            source_view: None,
                            opacity: 1.,
                            invert: 0.,
                            blend: Default::default(),
                            scissor: scissor
                                .or_else(|| Scissor::clip_bounds(clip, transform, self.size))
                                .map_or(self.scissor, |s| self.scissor.intersect(s)),
                            isolated,
                            forward_source: false,
                        },
                        |bytes| self.transient(bytes),
                    )?;
                }
                FrameOp::PushGroup(group) => {
                    if let Some(fill) = crate::common::scopes::inline_group_fill(
                        group,
                        &current.commands[current.next..],
                        transform,
                        self.size,
                        1,
                    ) && !fill.crosses_boundary(
                        current.drawing,
                        current.offset,
                        current.next,
                        current.transitions_enabled,
                        self.capture,
                    ) {
                        self.scissor = self.scissor.intersect(fill.scissor);
                        self.draw(
                            current.drawing,
                            &current.scopes.last().target,
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
                    let isolated = !current.groups.can_inline(current.next - 1);
                    let batch = current
                        .scheduled
                        .then(|| self.offscreen.entry(current.next - 1))
                        .flatten()
                        .copied();
                    let ink = self.glyph_plan.group_ink(group);
                    let output_scissor = Scissor::group_bounds(
                        group,
                        transform,
                        self.size,
                        current.transitions_enabled && !current.drawing.transitions().is_empty(),
                    )
                    .map_or(self.scissor, |bounds| self.scissor.intersect(bounds));
                    let mut source_view = batch.map(|entry| entry.view).or_else(|| {
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
                                    self.gpu.capabilities().max_texture_size,
                                )
                            })
                            .flatten()
                    });
                    let mut forward_source = false;
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
                            }
                        } else {
                            crate::common::effect_plan::source_layout(
                                effect,
                                group.effect_to_target,
                                transform,
                                self.gpu.capabilities().max_texture_size,
                                at,
                            )?
                        };
                        forward_source = current.groups.is_source_child(current.next - 1)
                            && current.scopes.last().isolated
                            && crate::common::effect_plan::forwards_source(
                                group, &source, transform, self.size,
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
                            group: Some(group),
                            scheduled: batch.map(|entry| entry.last),
                            shared_source: batch.is_some_and(|entry| entry.shared),
                            forward_source,
                            target: if forward_source {
                                current.scopes.last().target.clone()
                            } else if batch.is_some_and(|entry| entry.shared && !entry.first) {
                                current
                                    .deferred
                                    .last()
                                    .expect("active source atlas")
                                    .source
                                    .clone()
                            } else if isolated {
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
                            direct_mask_projection:
                                crate::common::effect_plan::direct_mask_projection(
                                    group,
                                    current.transitions_enabled
                                        && !current.drawing.transitions().is_empty(),
                                ),
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
                            entry.at,
                        )?;
                        self.recycle_target(source)?;
                        current.scopes.last_mut().target = processed;
                        continue;
                    }
                    if let Some(view) = group.source_view.filter(|_| group.mask_effect.is_none()) {
                        if let Some(last) = group.scheduled {
                            push_stack(
                                &mut current.deferred,
                                DeferredComposite {
                                    source,
                                    view,
                                    opacity: group.opacity,
                                    invert: group.invert,
                                    scissor: self.scissor,
                                },
                                |bytes| self.transient(bytes),
                            )?;
                            if last {
                                let mut pending = std::mem::take(&mut current.deferred);
                                self.combine_views(&current.scopes.last().target, &pending)?;
                                for (index, item) in pending.drain(..).enumerate() {
                                    if !group.shared_source || index == 0 {
                                        self.recycle_target(item.source)?;
                                    }
                                }
                                current.deferred = pending;
                            }
                            continue;
                        }
                        if group.blend == Default::default() {
                            self.combine_view(
                                &current.scopes.last().target,
                                &source,
                                view,
                                group.opacity,
                                group.invert,
                            )?;
                        } else {
                            self.combine_group(
                                &current.scopes.last().target,
                                &source,
                                &source,
                                GroupPaint {
                                    source_origin: view.origin(),
                                    composite: reel_ui::frame::GroupComposite {
                                        alpha: group.opacity,
                                        blend: group.blend,
                                    },
                                    invert: group.invert,
                                    operation: CompositeOperation::SourceOver,
                                    image_mask: None,
                                },
                            )?;
                        }
                        self.recycle_target(source)?;
                        continue;
                    }
                    if let Some(view) = group.source_view.filter(|_| group.mask_effect.is_some()) {
                        if group.direct_mask_projection {
                            self.project_source(
                                &current.scopes.last().target,
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
                                &source,
                                &coverage,
                                GroupPaint {
                                    source_origin: [0; 2],
                                    composite: reel_ui::frame::GroupComposite {
                                        alpha: group.opacity,
                                        blend: group.blend,
                                    },
                                    invert: group.invert,
                                    operation: CompositeOperation::AlphaMask,
                                    image_mask: group.image_mask,
                                },
                            )?;
                            self.recycle_target(source)?;
                            self.recycle_target(coverage)?;
                            continue;
                        }
                        let clipped = self.target()?;
                        self.combine(
                            &clipped,
                            &source,
                            &coverage,
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
                            &source,
                            &source,
                            GroupPaint {
                                source_origin: [0; 2],
                                composite: reel_ui::frame::GroupComposite {
                                    alpha: group.opacity,
                                    blend: group.blend,
                                },
                                invert: group.invert,
                                operation: CompositeOperation::SourceOver,
                                image_mask: group.image_mask,
                            },
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
                group: None,
                scheduled: None,
                shared_source: false,
                target: self.target()?,
                mesh_clip: None,
                clip: None,
                mask: None,
                image_mask: None,
                effect: None,
                mask_effect: None,
                direct_mask_projection: false,
                source_parent: None,
                source_view: None,
                opacity: 1.,
                invert: 0.,
                blend: Default::default(),
                scissor,
                isolated: true,
                forward_source: false,
            }),
            pending: None,
        })
    }

    pub fn combine(
        &mut self,
        target: &Target,
        source: &Target,
        mask: &Target,
        alpha: f32,
        invert: f32,
        operation: CompositeOperation,
    ) -> Result<(), GpuError> {
        self.composition(
            target,
            Composition {
                source,
                mask,
                backdrop: source,
                alpha,
                invert,
                operation,
                blend: 0,
                expression: None,
                source_origin: [0; 2],
            },
        )
    }
    fn combine_views(
        &mut self,
        target: &Target,
        pending: &[DeferredComposite],
    ) -> Result<(), GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::CompositeBatch);
        // Initialize every sampled source before returning to the parent FBO.
        for item in pending {
            self.ensure_sampled(&item.source.view)?;
        }
        let scissor = self.scissor;
        for item in pending {
            self.scissor = item.scissor;
            self.combine_view(target, &item.source, item.view, item.opacity, item.invert)?;
        }
        self.scissor = scissor;
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
        self.composition(
            target,
            Composition {
                source,
                mask: source,
                backdrop: source,
                alpha,
                invert,
                operation: CompositeOperation::SourceOver,
                blend: 0,
                expression: None,
                source_origin: view.origin(),
            },
        )
    }
    fn composition(
        &mut self,
        target: &Target,
        composition: Composition<'_>,
    ) -> Result<(), GpuError> {
        self.flush_draws()?;
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Composite);
        if self.scissor.empty() {
            return Ok(());
        }
        let Composition {
            source,
            mask,
            backdrop,
            alpha,
            invert,
            operation,
            blend,
            expression,
            source_origin,
        } = composition;
        let (mode, progress) = match operation {
            CompositeOperation::SourceOver => (0., 0.),
            CompositeOperation::AlphaMask => (1., 0.),
            CompositeOperation::LuminanceMask => (2., 0.),
            CompositeOperation::Transition(p) => (3., p),
            CompositeOperation::MaskedTransition(p) => (4., p),
        };
        let plain = mode == 0. && invert == 0. && blend == 0 && expression.is_none();
        let coverage = mode == 1. && invert == 0. && blend == 0 && expression.is_none();
        let _kind = reel_core::profile::Scope::new(if plain {
            reel_core::profile::Phase::SourceComposite
        } else if coverage {
            reel_core::profile::Phase::CoverageComposite
        } else {
            reel_core::profile::Phase::EffectComposite
        });
        let shader = if plain {
            &self.programs.source_over
        } else if coverage {
            &self.programs.alpha_mask
        } else if blend > 4 {
            &self.programs.extended_blend
        } else {
            &self.programs.composite
        };
        self.ensure_sampled(&source.view)?;
        if !plain {
            self.ensure_sampled(&mask.view)?;
        }
        if blend != 0 {
            self.ensure_sampled(&backdrop.view)?;
        }
        if let Some(expression) = expression {
            self.ensure_sampled(&expression.view)?;
        }
        self.ensure_target(target)?;
        let mut values = Values::new();
        for (name, unit) in [
            ("u_source", 0),
            ("u_mask", 1),
            ("u_backdrop", 2),
            ("u_expression", 3),
        ] {
            values.extend(shader.int(name, unit));
        }
        values.extend(shader.vec4("u_params", [alpha, mode, progress, 0.]));
        values.extend(shader.vec4("u_source_uv", source.source_uv(self.size, source_origin)));
        for (name, input) in [
            ("u_mask_uv", mask),
            ("u_backdrop_uv", backdrop),
            ("u_expression_uv", expression.unwrap_or(source)),
        ] {
            values.extend(shader.vec4(name, input.frame_uv(self.size)));
        }
        values.extend(shader.float("u_invert", invert));
        values.extend(shader.float("u_blend", blend as f32));
        values.extend(shader.float("u_has_expression", f32::from(expression.is_some())));
        let replacing = matches!(
            operation,
            CompositeOperation::Transition(_) | CompositeOperation::MaskedTransition(_)
        ) || blend != 0;
        self.programs.quad(
            self.gpu,
            programs::Quad {
                target: native::Target::Framebuffer(&target.framebuffer),
                size: self.size,
                shader,
                textures: &[
                    &source.view,
                    &mask.view,
                    &backdrop.view,
                    &expression.unwrap_or(source).view,
                ][..if plain {
                    1
                } else if coverage {
                    2
                } else {
                    4
                }],
                values: &values,
                blend: if replacing {
                    native::Blend::Replace
                } else {
                    native::Blend::Premultiplied
                },
                scissor: self.native_scissor(),
            },
        )
    }
    fn combine_group(
        &mut self,
        target: &Target,
        source: &Target,
        mask: &Target,
        paint: GroupPaint<'_>,
    ) -> Result<(), GpuError> {
        self.flush_draws()?;
        if self.scissor.empty() {
            return Ok(());
        }
        let GroupPaint {
            source_origin,
            composite,
            invert,
            operation,
            image_mask,
        } = paint;
        let blend = blend_code(composite.blend)
            .ok_or(GpuError::Device("unsupported group blend".into()))?;
        if blend == 0
            && invert == 0.
            && matches!(operation, CompositeOperation::SourceOver)
            && let Some((definition, transform)) = image_mask
            && self.composite_sample_mask(target, source, definition, transform, composite.alpha)?
        {
            return Ok(());
        }
        let expression = if let Some((definition, transform)) = image_mask {
            Some(self.mask_expression(definition, transform)?)
        } else {
            None
        };
        let backdrop = if blend != 0 {
            let backdrop = self.target()?;
            self.combine(
                &backdrop,
                target,
                target,
                1.,
                0.,
                CompositeOperation::SourceOver,
            )?;
            Some(backdrop)
        } else {
            None
        };
        self.composition(
            target,
            Composition {
                source,
                mask,
                backdrop: backdrop.as_ref().unwrap_or(source),
                alpha: composite.alpha,
                invert,
                operation,
                blend,
                expression: expression.as_ref(),
                source_origin,
            },
        )?;
        if let Some(backdrop) = backdrop {
            self.recycle_target(backdrop)?;
        }
        if let Some(expression) = expression {
            self.recycle_target(expression)?;
        }
        Ok(())
    }
    fn combine_transition(
        &mut self,
        target: &Target,
        old: &Target,
        new: &Target,
        transition: &SceneTransition,
        outer: Affine,
    ) -> Result<(), GpuError> {
        let expression = if let Some(definition) = transition
            .mask
            .as_ref()
            .filter(|_| transition.progress > 0. && transition.progress < 1.)
        {
            Some(self.mask_expression(definition, outer * transition.mask_to_window)?)
        } else {
            None
        };
        let operation = if expression.is_some() {
            CompositeOperation::MaskedTransition(transition.progress)
        } else {
            CompositeOperation::Transition(transition.progress)
        };
        self.composition(
            target,
            Composition {
                source: old,
                mask: new,
                backdrop: old,
                alpha: 1.,
                invert: 0.,
                operation,
                blend: 0,
                expression: expression.as_ref(),
                source_origin: [0; 2],
            },
        )?;
        if let Some(expression) = expression {
            self.recycle_target(expression)?;
        }
        Ok(())
    }
    pub fn transient(&mut self, bytes: u64) -> Result<(), GpuError> {
        self.tickets.push(reserve(
            &self.budgets.transient,
            &self.budget_scope.transient,
            bytes,
            self.limits.transient_bytes,
            "transient",
        )?);
        Ok(())
    }
    pub fn composite_supported(
        &self,
        composite: Composite,
        at: RecordLocation,
    ) -> Result<(), GpuError> {
        if composite.blend != Composite::default().blend {
            Err(GpuError::Unsupported {
                at,
                record: "blend mode",
            })
        } else {
            Ok(())
        }
    }
    pub fn native_scissor(&self) -> Option<[u32; 4]> {
        Some([
            self.scissor.x,
            self.scissor.y,
            self.scissor.width,
            self.scissor.height,
        ])
    }
}
pub(super) use crate::common::blend::code as blend_code;
