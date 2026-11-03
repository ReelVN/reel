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
    backdrop_origin: [i32; 2],
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
    pub effect_results: &'a crate::common::effect_result::Cache<Target>,
    pub effect_admission: crate::common::effect_result::Admission,
    pub effect_pending: Vec<crate::common::effect_result::Pending<Target>>,
    effect_tails: Vec<effect_tail::Tail>,
    pub effect_pins: Vec<Target>,
    effect_inputs: Vec<(crate::common::effect_result::Token, Target)>,
    pub effect_replacements: super::targets::Replacements,
    pub macrotile_outputs: Vec<(usize, Target)>,
    macrotile_sources: Vec<macrotile::Source>,
    pub planned_targets: Option<crate::common::target_plan::Requirements>,
    pub preparing_targets: bool,
    pub staged_targets: Vec<Target>,
    pub target_cache: &'a mut super::target_cache::Cache,
    pub coverage_target_cache: &'a mut super::target_cache::Cache,
    pub retained_cache: &'a crate::common::retained::Cache<Target, u8>,
    pub selection: std::sync::Arc<crate::common::retained::Plan>,
    pub retained_inputs: Vec<Target>,
    pub warm: Option<usize>,
    pub warmed: bool,
    pub targets: Vec<Target>,
    pub available_targets: Vec<Target>,
    pub target_writes: crate::common::target_writes::TargetWrites,
    pub target_storage: native::TargetStorage,
    pub geometry_cache: &'a mut super::geometry::Cache,
    pub buffers: Vec<super::geometry::Storage>,
    pub paint_batch: Option<draw::PaintBatch>,
    pub instance_batch: Option<draw::InstanceBatch>,
    pub scissor: Scissor,
    pub mesh_clip: Option<crate::common::mesh::RectClip>,
    pub at: Option<RecordLocation>,
}
pub(super) struct Work {
    pub stream_started: bool,
    _tickets: Vec<Reservation>,
    _failed_native: Vec<super::ownership::NativeTicket>,
    _images: HashMap<ResourceHandle, Rc<Image>>,
    _glyphs: Vec<Rc<draw::Glyph>>,
    _effects: Vec<Rc<super::effect::Compiled>>,
    effect_pending: std::cell::RefCell<Vec<crate::common::effect_result::Pending<Target>>>,
    _effect_pins: Vec<Target>,
    _effect_inputs: Vec<(crate::common::effect_result::Token, Target)>,
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
    pub fn images(&self) -> &HashMap<ResourceHandle, Rc<Image>> {
        &self._work._images
    }
    pub fn take_effect_pending(&self) -> Vec<crate::common::effect_result::Pending<Target>> {
        std::mem::take(&mut *self._work.effect_pending.borrow_mut())
    }
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
    pub effect_results: &'a crate::common::effect_result::Cache<Target>,
    pub effect_results_enabled: bool,
    pub masks: &'a mut crate::common::mask::split::Cache,
    pub targets: &'a mut super::target_cache::Cache,
    pub coverage_targets: &'a mut super::target_cache::Cache,
    pub geometry: &'a mut super::geometry::Cache,
    pub retained: &'a crate::common::retained::Cache<Target, u8>,
}

#[path = "replay/prepare.rs"]
mod prepare;
pub(super) use prepare::prepare;

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
        self.target.as_ref().expect("materialized replay scope")
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
    effect: Option<(&'a reel_ui::frame::FrameEffect, Affine, RecordLocation)>,
    view: Option<crate::common::source_view::SourceView>,
    blend: reel_ui::frame::BlendMode,
    opacity: f32,
    invert: f32,
    scissor: Scissor,
    size: [u32; 2],
    mesh_clip: Option<crate::common::mesh::RectClip>,
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
                        child.scopes.last().target(),
                        current.scopes.last().target(),
                        current.scopes.last().target(),
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
                    child.scopes.last().target(),
                    current.scopes.last().target(),
                    current.scopes.last().target(),
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
                    .expect("finished replay target");
                self.ensure_sampled(&finished.view)?;
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
                            parent.scopes.last().target(),
                            parent.scopes.last().target(),
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
                            source_input: None,
                            group: None,
                            clip_child: isolated
                                .then(|| {
                                    crate::common::effect_plan::clipped_effect(
                                        current.drawing,
                                        current.commands,
                                        current.next,
                                        1,
                                        self.capture || !current.drawing.transitions().is_empty(),
                                    )
                                })
                                .flatten(),
                            delayed: None,
                            scheduled: None,
                            shared_source: false,
                            effect_input: None,
                            target: Some(if !isolated {
                                current.scopes.last().target().clone()
                            } else {
                                self.target()?
                            }),
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
                        1,
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
                    let isolated = !current.groups.can_inline(current.next - 1);
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
                        #[cfg(feature = "profiling")]
                        item.source.trace_event(
                            crate::profiling::TraceKind::SourceReuse,
                            "same-frame-source",
                        );
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
                                    self.gpu.capabilities().max_texture_size,
                                )
                            })
                            .flatten()
                    });
                    let mut forward_source = false;
                    let mut effect_source_scissor = None;
                    let mut source_initialize = None;
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
                                self.gpu.capabilities().max_texture_size,
                                at,
                            )?
                        };
                        let demand = crate::common::effect_plan::roi::source_demand(
                            current.drawing,
                            group,
                            effect_transform,
                            self.size,
                            output_scissor,
                            source.size,
                            self.offscreen.storage_size(source.size),
                        );
                        effect_source_scissor = Some(
                            demand
                                .as_ref()
                                .map_or(Scissor::full(source.size), |demand| demand.draw),
                        );
                        source_initialize = demand.map(|demand| demand.initialize);
                        forward_source = current.groups.is_source_child(current.next - 1)
                            && current.scopes.last().isolated
                            && crate::common::effect_plan::forwards_source(
                                group,
                                &source,
                                transform,
                                self.size,
                                current.scopes.last().shared_source,
                            );
                        if current.source_crop.is_none()
                            && isolated
                            && !forward_source
                            && batch.is_none_or(|entry| entry.view.is_none())
                            && self.offscreen.source_crops()
                            && (self.offscreen.crop_backing(source.size).is_some()
                                || self.offscreen.storage_size(source.size) == source.size)
                        {
                            current.source_crop = crate::common::source_crop::plan(
                                current.drawing,
                                current.commands,
                                current.next - 1,
                                &current.groups,
                                &source,
                                self.offscreen.crop_backing(source.size),
                                effect_source_scissor.unwrap(),
                                self.capture,
                            )
                            .filter(|plan| {
                                self.offscreen.storage_size(plan.crop.size) == plan.crop.size
                            });
                        }
                        defer_source = current
                            .source_crop
                            .is_some_and(|plan| plan.leaf != current.next - 1);
                        if current
                            .source_crop
                            .is_some_and(|plan| plan.leaf == current.next - 1)
                        {
                            source_crop = current.source_crop.take().map(|plan| plan.crop);
                        }
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
                        current.transform = source_crop.map_or(source.frame_to_source, |crop| {
                            crop.transform(source.frame_to_source)
                        });
                        source_effect_transform =
                            Some(source_crop.map_or(source.effect_to_source, |crop| {
                                crop.transform(source.effect_to_source)
                            }));
                        current.size = source_crop.map_or(source.size, |crop| crop.raster_size());
                        self.size = current.size;
                        if let Some(crop) = source_crop {
                            effect_source_scissor =
                                effect_source_scissor.map(|region| crop.scissor(region));
                        }
                        self.mesh_clip = None;
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
                    let source_push = current.offset.map(|offset| offset + current.next - 1);
                    let (effect_input, selected_input) =
                        self.selected_effect_input(current, forward_source, source_scissor);
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
                    let macrotile_source = if effect_input.is_none()
                        && isolated
                        && source_view.is_none()
                        && batch.is_none_or(|entry| entry.view.is_none())
                    {
                        self.macrotile_source(current)?
                    } else {
                        None
                    };
                    let mut source_target = if effect_input.is_some() {
                        selected_input
                    } else {
                        Some(if defer_source {
                            current.scopes.last().target().clone()
                        } else if let Some(crop) = source_crop {
                            self.cropped_target(crop)?
                        } else if let Some(source) = macrotile_source {
                            source
                        } else if forward_source {
                            current.scopes.last().target().clone()
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
                            current.scopes.last().target().clone()
                        })
                    };
                    if effect_input.is_none()
                        && isolated
                        && batch.is_none()
                        && source_view.is_none()
                        && source_crop.is_none()
                        && !defer_source
                        && let Some(initialize) = source_initialize
                        && let Some(target) = &mut source_target
                    {
                        self.restrict_source_initialization(target, initialize, current)?;
                    }
                    current.scopes.push(
                        Kind::Group,
                        at,
                        Scope {
                            source_input: source_push
                                .map(|push| (push, current.transform, source_scissor)),
                            group: Some(group),
                            clip_child: None,
                            delayed: None,
                            scheduled: batch.map(|entry| entry.last),
                            shared_source: batch.is_some_and(|entry| entry.shared),
                            forward_source,
                            early_reusable: cached_source.early_reusable,
                            effect_input,
                            target: source_target,
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
                            1,
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
                    let mut source = group.target.expect("completed source target");
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
                        if group.scheduled.is_some() {
                            self.defer_composite(
                                current,
                                DeferredComposite {
                                    push: group.source_input.expect("scheduled source").0,
                                    source,
                                    effect: None,
                                    view: Some(view),
                                    blend: group.blend,
                                    opacity: group.opacity,
                                    invert: group.invert,
                                    scissor: self.scissor,
                                    size: self.size,
                                    mesh_clip: self.mesh_clip,
                                },
                                group.scheduled == Some(true),
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
                            let empty = parent.target.replace(source).expect("clip source target");
                            parent.delayed = Some((effect, transform, entry.at, group.opacity));
                            self.recycle_target(empty)?;
                            continue;
                        }

                        if group.group.is_some_and(|definition| {
                            crate::common::effect_plan::terminal_composite(
                                definition,
                                1,
                                self.capture || !current.drawing.transitions().is_empty(),
                            )
                        }) {
                            if group.scheduled.is_some() {
                                self.defer_composite(
                                    current,
                                    DeferredComposite {
                                        push: group.source_input.expect("scheduled source").0,
                                        source,
                                        effect: Some((effect, transform, entry.at)),
                                        view: None,
                                        blend: group.blend,
                                        opacity: group.opacity,
                                        invert: group.invert,
                                        scissor: self.scissor,
                                        size: self.size,
                                        mesh_clip: self.mesh_clip,
                                    },
                                    group.scheduled == Some(true),
                                    false,
                                )?;
                            } else {
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
                            }
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
                        let coverage = self.clip_target(group.group, group.delayed.is_some())?;
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
                    } else if group.scheduled.is_some() {
                        self.defer_composite(
                            current,
                            DeferredComposite {
                                push: group.source_input.expect("scheduled source").0,
                                source,
                                effect: None,
                                view: None,
                                blend: group.blend,
                                opacity: group.opacity,
                                invert: group.invert,
                                scissor: self.scissor,
                                size: self.size,
                                mesh_clip: self.mesh_clip,
                            },
                            group.scheduled == Some(true),
                            group.shared_source,
                        )?;
                    } else {
                        self.combine_group(
                            current.scopes.last().target(),
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

#[path = "replay/composition.rs"]
mod composition;
#[path = "replay/schedule.rs"]
mod schedule;
#[path = "replay/source_free.rs"]
mod source_free;

#[path = "replay/effect_cache.rs"]
mod effect_cache;
#[path = "replay/effect_inputs.rs"]
mod effect_inputs;
#[path = "replay/effect_resolution.rs"]
mod effect_resolution;
#[path = "replay/effect_tail.rs"]
mod effect_tail;
#[path = "replay/macrotile.rs"]
mod macrotile;
#[path = "replay/source_initialization.rs"]
mod source_initialization;
