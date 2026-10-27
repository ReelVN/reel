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
pub(super) struct Context<'a> {
    pub capture: bool,
    pub gpu: &'a native::Context,
    pub programs: &'a programs::Programs,
    pub limits: crate::common::GpuLimits,
    pub size: [u32; 2],
    pub budgets: &'a Budgets,
    pub budget_scope: Budgets,
    pub tickets: Vec<Reservation>,
    pub images: HashMap<ResourceHandle, Rc<Image>>,
    pub image_cache: &'a mut Images,
    pub glyph_cache: &'a mut draw::GlyphCache,
    pub glyphs: Vec<Rc<draw::Glyph>>,
    pub glyph_plan: crate::common::glyph_plan::Plan,
    pub offscreen: crate::common::offscreen::Plan,
    pub effect_cache: &'a mut super::effect::Cache,
    pub mask_cache: &'a mut crate::common::mask::split::Cache,
    pub effects: Vec<Rc<super::effect::Compiled>>,
    pub target_cache: &'a mut Vec<Target>,
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
    _images: HashMap<ResourceHandle, Rc<Image>>,
    _glyphs: Vec<Rc<draw::Glyph>>,
    _effects: Vec<Rc<super::effect::Compiled>>,
    _targets: Vec<Target>,
    buffers: std::cell::RefCell<Vec<super::geometry::Storage>>,
}
pub(super) struct Prepared {
    pub output: Target,
    _work: Work,
}
impl Prepared {
    pub fn recycle_geometry(&self, cache: &mut super::geometry::Cache) {
        for storage in self._work.buffers.borrow_mut().drain(..) {
            cache.put(storage);
        }
    }
}
pub(super) fn prepare(
    gpu: &native::Context,
    programs: &programs::Programs,
    budgets: &Budgets,
    limits: crate::common::GpuLimits,
    image_cache: &mut Images,
    glyph_cache: &mut draw::GlyphCache,
    effect_cache: &mut super::effect::Cache,
    mask_cache: &mut crate::common::mask::split::Cache,
    target_cache: &mut Vec<Target>,
    geometry_cache: &mut super::geometry::Cache,
    images: HashMap<ResourceHandle, Rc<Image>>,
    frame: &RenderFrame,
    size: [u32; 2],
    capture: bool,
) -> Result<Prepared, (GlesError, Work)> {
    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Replay);
    let mut context = Context {
        capture,
        gpu,
        programs,
        limits,
        size,
        budgets,
        budget_scope: Budgets::new(limits),
        tickets: Vec::new(),
        images,
        image_cache,
        glyph_cache,
        glyphs: Vec::new(),
        glyph_plan: Default::default(),
        offscreen: Default::default(),
        effect_cache,
        mask_cache,
        effects: Vec::new(),
        target_cache,
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
        &mut context,
    )
    .and_then(|plan| {
        context.glyph_plan = plan;
        let (schedule, pixels, retained) = crate::common::offscreen::Plan::choose(
            frame,
            None,
            size,
            capture,
            1,
            gpu.capabilities().max_texture_size,
            &context.glyph_plan,
            limits.target_bytes / 4,
            crate::common::target_plan::MaskExecution::Sampled,
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
        if pixels > limits.target_bytes / 4 {
            return Err(GlesError::Budget {
                domain: "offscreen targets",
                requested: pixels.saturating_mul(4),
                available: limits.target_bytes,
            });
        }
        context.offscreen = schedule;
        let result = context.stream(frame.drawing(), transform);
        context.offscreen = Default::default();
        drop(retained);
        result
    });
    // Keep the compatible high-water pool across effect endpoints. Residency
    // pressure and output-size changes retire it at the admission boundary.
    context.glyphs.extend(context.glyph_cache.take_uploads());
    let work = Work {
        _tickets: context.tickets,
        _images: context.images,
        _glyphs: context.glyphs,
        _effects: context.effects,
        _targets: context.targets,
        buffers: std::cell::RefCell::new(context.buffers),
    };
    match output {
        Ok(output) => Ok(Prepared {
            output,
            _work: work,
        }),
        Err(error) => Err((error, work)),
    }
}

struct Scope<'a> {
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
struct DeferredComposite {
    source: Target,
    view: crate::common::source_view::SourceView,
    opacity: f32,
    invert: f32,
    scissor: Scissor,
}
struct Stream<'a> {
    scheduled: bool,
    deferred: Vec<DeferredComposite>,
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
    pub fn target(&mut self) -> Result<Target, GlesError> {
        self.flush_draws()?;
        let target = if let Some(index) = self
            .available_targets
            .iter()
            .rposition(|target| target.size == self.size)
        {
            self.available_targets.swap_remove(index)
        } else {
            let bytes = u64::from(self.size[0]) * u64::from(self.size[1]) * 4;
            let local =
                self.budget_scope
                    .targets
                    .reserve(bytes)
                    .map_err(|_| GlesError::Budget {
                        domain: "offscreen targets",
                        requested: bytes,
                        available: self
                            .limits
                            .target_bytes
                            .saturating_sub(self.budget_scope.targets.usage().bytes),
                    })?;
            // The previous frame's presentation draw precedes this clear on the
            // same GLES queue. Capture outputs use a separate allocation set.
            let index = self
                .targets
                .iter()
                .filter(|target| target.size == self.size)
                .count();
            let mut target = if let Some(target) = self
                .target_cache
                .iter()
                .filter(|target| target.size == self.size)
                .nth(index)
            {
                target.clone()
            } else {
                // Retain distinct size buckets across animation and allocation
                // order changes. Existing byte budgets bound the idle pool.
                if bytes
                    > self
                        .limits
                        .target_bytes
                        .saturating_sub(self.budgets.targets.usage().bytes)
                {
                    self.target_cache.retain(|target| {
                        self.targets.iter().any(|live| live.same_allocation(target))
                    });
                }
                let target = match Target::new(
                    self.gpu,
                    self.size,
                    &self.budgets.targets,
                    self.limits.target_bytes,
                ) {
                    Ok(target) => target,
                    Err((error, storage)) => {
                        self.tickets.push(Reservation {
                            _shared: storage,
                            _local: local,
                        });
                        return Err(error);
                    }
                };
                self.target_cache.push(target.clone());
                target
            };
            self.tickets.push(Reservation {
                _shared: None,
                _local: local,
            });
            target.slot = self.targets.len();
            self.targets.push(target.clone());
            target
        };
        let mut writes = std::mem::take(&mut self.target_writes);
        writes.begin(target.slot, |bytes| self.transient(bytes))?;
        self.target_writes = writes;
        Ok(target)
    }
    pub(super) fn ensure_target(&mut self, target: &Target) -> Result<(), GlesError> {
        if self.target_writes.take(target.slot) {
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Clear);
            self.gpu
                .clear(native::Target::Framebuffer(&target.framebuffer), [0.; 4])?;
        }
        Ok(())
    }
    pub(super) fn ensure_sampled(&mut self, source: &native::Texture) -> Result<(), GlesError> {
        if let Some(slot) = self
            .targets
            .iter()
            .position(|target| target.view == *source)
            && self.target_writes.take(slot)
        {
            let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Clear);
            self.gpu.clear(
                native::Target::Framebuffer(&self.targets[slot].framebuffer),
                [0.; 4],
            )?;
        }
        Ok(())
    }
    fn recycle_target(&mut self, target: Target) -> Result<(), GlesError> {
        self.available_targets.push(target);
        Ok(())
    }
    pub(super) fn stream(
        &mut self,
        frame: &FrameDrawing,
        transform: Affine,
    ) -> Result<Target, GlesError> {
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
                            fill.at,
                        )?;
                        current.next += fill.consumed;
                        continue;
                    }
                    blend_code(group.composite.blend).ok_or(GlesError::Unsupported {
                        at,
                        record: "group blend mode",
                    })?;
                    if !group.filters.is_empty() {
                        return Err(GlesError::Unsupported {
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
                            self.gpu.capabilities().max_texture_size,
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
                            scheduled: batch.map(|entry| entry.last),
                            shared_source: batch.is_some_and(|entry| entry.shared),
                            target: if batch.is_some_and(|entry| entry.shared && !entry.first) {
                                current.deferred.last().expect("active source atlas").source.clone()
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
                            direct_mask_projection: crate::common::effect_plan::direct_mask_projection(
                                group,
                                current.transitions_enabled && !current.drawing.transitions().is_empty(),
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
                        self.project_source(&projected, &source, view.parent_to_source, 1., entry.at)?;
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
                            GlesError::Unsupported {
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
                    return Err(GlesError::Unsupported {
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
    ) -> Result<Stream<'a>, GlesError> {
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
            }),
            pending: None,
        })
    }

    pub fn combine(
        &mut self,
        target: &Target,
        source: &native::Texture,
        mask: &native::Texture,
        alpha: f32,
        invert: f32,
        operation: CompositeOperation,
    ) -> Result<(), GlesError> {
        self.composition(
            target, source, mask, source, alpha, invert, operation, 0, None, [0; 2],
        )
    }
    fn combine_views(
        &mut self,
        target: &Target,
        pending: &[DeferredComposite],
    ) -> Result<(), GlesError> {
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
    ) -> Result<(), GlesError> {
        self.composition(
            target,
            &source.view,
            &source.view,
            &source.view,
            alpha,
            invert,
            CompositeOperation::SourceOver,
            0,
            None,
            view.origin(),
        )
    }
    fn composition(
        &mut self,
        target: &Target,
        source: &native::Texture,
        mask: &native::Texture,
        backdrop: &native::Texture,
        alpha: f32,
        invert: f32,
        operation: CompositeOperation,
        blend: u32,
        expression: Option<&Target>,
        source_origin: [i32; 2],
    ) -> Result<(), GlesError> {
        self.flush_draws()?;
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Composite);
        if self.scissor.empty() {
            return Ok(());
        }
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
        } else {
            &self.programs.composite
        };
        self.ensure_sampled(source)?;
        if !plain {
            self.ensure_sampled(mask)?;
        }
        if blend != 0 {
            self.ensure_sampled(backdrop)?;
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
        values.extend(shader.vec4(
            "u_source_uv",
            crate::common::source_view::uv_mapping(self.size, source.size(), source_origin),
        ));
        values.extend(shader.float("u_invert", invert));
        values.extend(shader.float("u_blend", blend as f32));
        values.extend(shader.float("u_has_expression", f32::from(expression.is_some())));
        let replacing = matches!(
            operation,
            CompositeOperation::Transition(_) | CompositeOperation::MaskedTransition(_)
        ) || blend != 0;
        self.programs.quad(
            self.gpu,
            native::Target::Framebuffer(&target.framebuffer),
            self.size,
            shader,
            &[
                source,
                mask,
                backdrop,
                expression.map_or(source, |t| &t.view),
            ][..if plain {
                1
            } else if coverage {
                2
            } else {
                4
            }],
            &values,
            if replacing {
                native::Blend::Replace
            } else {
                native::Blend::Premultiplied
            },
            self.gl_scissor(),
        )
    }
    fn combine_group(
        &mut self,
        target: &Target,
        source: &native::Texture,
        mask: &native::Texture,
        composite: reel_ui::frame::GroupComposite,
        invert: f32,
        operation: CompositeOperation,
        image_mask: Option<(&reel_ui::frame::FrameImageMask, Affine)>,
    ) -> Result<(), GlesError> {
        self.flush_draws()?;
        if self.scissor.empty() {
            return Ok(());
        }
        let blend = blend_code(composite.blend)
            .ok_or(GlesError::Device("unsupported group blend".into()))?;
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
        self.composition(
            target,
            source,
            mask,
            backdrop.as_ref().map_or(source, |b| &b.view),
            composite.alpha,
            invert,
            operation,
            blend,
            expression.as_ref(),
            [0; 2],
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
        old: &native::Texture,
        new: &native::Texture,
        transition: &SceneTransition,
        outer: Affine,
    ) -> Result<(), GlesError> {
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
            old,
            new,
            old,
            1.,
            0.,
            operation,
            0,
            expression.as_ref(),
            [0; 2],
        )?;
        if let Some(expression) = expression {
            self.recycle_target(expression)?;
        }
        Ok(())
    }
    pub fn transient(&mut self, bytes: u64) -> Result<(), GlesError> {
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
    ) -> Result<(), GlesError> {
        if composite.blend != Composite::default().blend {
            Err(GlesError::Unsupported {
                at,
                record: "blend mode",
            })
        } else {
            Ok(())
        }
    }
    pub fn gl_scissor(&self) -> Option<[u32; 4]> {
        Some([
            self.scissor.x,
            self.size[1] - self.scissor.y - self.scissor.height,
            self.scissor.width,
            self.scissor.height,
        ])
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
