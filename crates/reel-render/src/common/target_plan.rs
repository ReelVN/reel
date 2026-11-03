//! Physical target lifetimes shared by admission and renderer scheduling.
use super::GpuError;
use super::offscreen;
use super::scissor::Scissor;
use reel_ui::frame::{
    FrameCommand, FrameDrawing, FrameGroup, FrameOp, RenderFrame, SceneTransition,
};
use reel_ui::kurbo::Affine;
mod inputs;
#[cfg(any(feature = "gxm", test))]
pub(crate) mod macrotile;
mod outputs;
mod resolution;
mod source_free;

/// Physical scratch multiplicities from the selected lifetime traversal.
#[derive(Default)]
pub(crate) struct Requirements(Vec<Requirement>, Vec<[u32; 2]>);
#[derive(Clone, Copy)]
struct Requirement {
    size: [u32; 2],
    count: u64,
}
impl Requirements {
    pub(crate) fn exact(&self) -> &[[u32; 2]] {
        &self.1
    }

    pub(crate) fn with_exact(
        mut self,
        exact: &[[u32; 2]],
        mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Self, GpuError> {
        for &size in exact {
            if !self.1.contains(&size) {
                super::scopes::push_stack(&mut self.1, size, &mut reserve)?;
            }
        }
        Ok(self)
    }
    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = ([u32; 2], u64)> + '_ {
        self.0.iter().map(|entry| (entry.size, entry.count))
    }
    #[cfg(test)]
    pub(crate) fn capacity_bytes(&self) -> u64 {
        (self.0.capacity() * size_of::<Requirement>() + self.1.capacity() * size_of::<[u32; 2]>())
            as u64
    }
    pub(crate) fn count(&self, size: [u32; 2]) -> u64 {
        self.0
            .binary_search_by_key(&size, |bucket| bucket.size)
            .map_or(0, |index| self.0[index].count)
    }

    /// Relocate one input class within the preallocated class-count capacity.
    pub(crate) fn relocate(&mut self, from: [u32; 2], to: [u32; 2], count: u64) {
        let old = self
            .0
            .binary_search_by_key(&from, |entry| entry.size)
            .unwrap();
        self.0[old].count -= count;
        if self.0[old].count == 0 {
            self.0.remove(old);
        }
        match self.0.binary_search_by_key(&to, |entry| entry.size) {
            Ok(index) => self.0[index].count += count,
            Err(index) => {
                // Each physical class has at least one of the original input
                // classes. Relocation cannot exceed collect's input capacity.
                assert!(self.0.len() < self.0.capacity());
                self.0.insert(index, Requirement { size: to, count });
            }
        }
    }

    pub(crate) fn collect(
        buckets: impl ExactSizeIterator<Item = ([u32; 2], u64)>,
        mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Self, GpuError> {
        let mut result = Vec::new();
        if buckets.len() != 0 {
            let bytes = buckets
                .len()
                .checked_mul(size_of::<Requirement>())
                .ok_or(GpuError::InvalidSize)?;
            reserve(bytes as u64)?;
            result.reserve_exact(buckets.len());
            result.extend(buckets.map(|(size, count)| Requirement { size, count }));
            result.sort_unstable_by_key(|bucket| bucket.size);
            let mut count = 0;
            for index in 0..result.len() {
                let bucket = result[index];
                if bucket.count == 0 {
                    continue;
                }
                if count != 0 && result[count - 1].size == bucket.size {
                    result[count - 1].count = result[count - 1]
                        .count
                        .checked_add(bucket.count)
                        .ok_or(GpuError::InvalidSize)?;
                } else {
                    result[count] = bucket;
                    count += 1;
                }
            }
            result.truncate(count);
        }
        Ok(Self(result, Vec::new()))
    }
}

/// A clip can use a scalar surface only if no precompiled effect tail consumes it.
pub(crate) fn single_clip(group: Option<&FrameGroup>, delayed: bool) -> bool {
    !delayed && group.is_none_or(|group| group.effect.is_none() && group.mask_effect.is_none())
}

#[derive(Clone, Copy)]
pub(crate) enum MaskExecution {
    #[cfg_attr(
        not(feature = "wgpu"),
        allow(dead_code, reason = "selected by the WGPU executor")
    )]
    Inline,
    #[cfg_attr(
        not(feature = "gxm"),
        allow(dead_code, reason = "selected by the GXM executor")
    )]
    Sampled,
}
impl MaskExecution {
    fn surface(self, group: Option<&FrameGroup>, source_over: bool) -> bool {
        matches!(self, Self::Sampled)
            && group.is_some_and(|group| {
                group.image_mask.as_ref().is_some_and(|mask| {
                    !source_over
                        || group.invert != 0.
                        || group.composite.blend != Default::default()
                        || super::mask::plan(&mask.program)
                            .and_then(|plan| plan.evaluate(&mask.program, mask.parameters))
                            .is_none()
                            && super::mask::paired::Plan::new(&mask.program)
                                .and_then(|plan| plan.evaluate(&mask.program, mask.parameters))
                                .is_none()
                })
            })
    }
}
#[cfg(test)]
#[path = "../gpu/target_plan_tests.rs"]
mod plan_tests;

/// Account the physical high-water allocation in each size bucket. Queue-ordered
/// reuse is valid only within a bucket; nonoverlapping differently sized sources
/// still retain separate GPU allocations until the submission completes.
#[derive(Default)]
struct Scratch {
    cropped: bool,
    output_crops: Vec<super::effect_output::Entry>,
    exact: Vec<[u32; 2]>,
    buckets: Vec<Bucket>,
    coverage: Vec<Bucket>,
    local_backdrop: bool,
    sampled_backdrop: bool,
    events: Option<Vec<super::target_storage::Event>>,
    effect_inputs: Vec<super::effect_inputs::Candidate>,
    #[cfg(any(feature = "gxm", test))]
    macrotiles: Vec<macrotile::Candidate>,
}
struct Bucket {
    size: [u32; 2],
    live: u64,
    peak: u64,
}
impl Scratch {
    fn acquire(
        &mut self,
        size: [u32; 2],
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        if let Some(events) = &mut self.events {
            super::scopes::push_stack(
                events,
                super::target_storage::Event {
                    size,
                    acquired: true,
                },
                &mut *reserve,
            )?;
        }
        if let Some(bucket) = self.buckets.iter_mut().find(|bucket| bucket.size == size) {
            bucket.live += 1;
            bucket.peak = bucket.peak.max(bucket.live);
        } else {
            super::scopes::push_stack(
                &mut self.buckets,
                Bucket {
                    size,
                    live: 1,
                    peak: 1,
                },
                reserve,
            )?;
        }
        Ok(())
    }
    fn release(
        &mut self,
        size: [u32; 2],
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        if let Some(events) = &mut self.events {
            super::scopes::push_stack(
                events,
                super::target_storage::Event {
                    size,
                    acquired: false,
                },
                reserve,
            )?;
        }
        let bucket = self
            .buckets
            .iter_mut()
            .find(|bucket| bucket.size == size)
            .expect("allocated target size");
        bucket.live = bucket.live.checked_sub(1).expect("live target");
        Ok(())
    }
    fn coverage(
        &mut self,
        size: [u32; 2],
        acquired: bool,
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        if let Some(bucket) = self.coverage.iter_mut().find(|bucket| bucket.size == size) {
            if acquired {
                bucket.live += 1;
                bucket.peak = bucket.peak.max(bucket.live);
            } else {
                bucket.live = bucket.live.checked_sub(1).expect("live coverage target");
            }
        } else {
            assert!(acquired, "coverage target was acquired");
            super::scopes::push_stack(
                &mut self.coverage,
                Bucket {
                    size,
                    live: 1,
                    peak: 1,
                },
                reserve,
            )?;
        }
        Ok(())
    }
    fn coverage_cost(
        &self,
        cost: Option<&dyn Fn([u32; 2]) -> Result<u64, GpuError>>,
    ) -> Result<u64, GpuError> {
        let Some(cost) = cost else { return Ok(0) };
        self.coverage.iter().try_fold(0u64, |total, bucket| {
            cost(bucket.size)?
                .checked_mul(bucket.peak)
                .and_then(|v| total.checked_add(v))
                .ok_or(GpuError::InvalidSize)
        })
    }
    fn composite(
        &mut self,
        size: [u32; 2],
        needed: bool,
        mask_surface: bool,
        single_mask: bool,
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        self.sampled_backdrop |= needed;
        if mask_surface {
            if single_mask {
                self.coverage(size, true, reserve)?;
            } else {
                self.acquire(size, reserve)?;
            }
        }
        if needed {
            self.acquire(size, reserve)?;
            self.release(size, reserve)?;
        }
        if mask_surface {
            if single_mask {
                self.coverage(size, false, reserve)?;
            } else {
                self.release(size, reserve)?;
            }
        }
        Ok(())
    }
    fn cost(&self, cost: &dyn Fn([u32; 2]) -> Result<u64, GpuError>) -> Result<u64, GpuError> {
        self.buckets.iter().try_fold(0u64, |total, bucket| {
            cost(bucket.size)?
                .checked_mul(bucket.peak)
                .and_then(|v| total.checked_add(v))
                .ok_or(GpuError::InvalidSize)
        })
    }
}
struct Scope<'a> {
    push: Option<usize>,
    macro_source: bool,
    clip_child: Option<&'a FrameGroup>,
    retained_source: Option<[u32; 2]>,
    defer: bool,
    shared_source: bool,
    end_batch: bool,
    isolated: bool,
    forward_source: bool,
    clip: bool,
    group: Option<&'a FrameGroup>,
    source_effect_transform: Option<Affine>,
    restore: Option<(Affine, [u32; 2])>,
    parent_scissor: Scissor,
    source_view: bool,
    source_owner: Option<inputs::Source>,
    input_candidate: Option<usize>,
}
enum Pending<'a> {
    Old(&'a SceneTransition, Option<usize>),
    New {
        source: [u32; 2],
        mask_surface: bool,
        cached: bool,
    },
    Mask {
        source: [u32; 2],
        backdrop: bool,
        mask_surface: bool,
    },
}
struct Stream<'a> {
    source_crop: Option<super::source_crop::Planned>,
    retained_context: usize,
    scheduled: bool,
    deferred: Vec<DeferredSource>,
    drawing: &'a FrameDrawing,
    commands: &'a [FrameCommand],
    offset: Option<usize>,
    transitions_enabled: bool,
    transition_next: usize,
    pending: Option<Pending<'a>>,
    next: usize,
    groups: crate::common::scopes::Proof,
    scopes: Vec<Scope<'a>>,
    transform: Affine,
    size: [u32; 2],
    scissor: Scissor,
    #[cfg(any(feature = "gxm", test))]
    macrotile: Option<macrotile::Open>,
}
struct DeferredSource {
    size: [u32; 2],
    input_candidate: Option<usize>,
    #[cfg(any(feature = "gxm", test))]
    macrotile: Option<macrotile::Candidate>,
}
impl DeferredSource {
    fn ordinary(size: [u32; 2]) -> Self {
        Self {
            size,
            input_candidate: None,
            #[cfg(any(feature = "gxm", test))]
            macrotile: None,
        }
    }
}
impl Scratch {
    fn release_deferred(
        &mut self,
        sources: &mut Vec<DeferredSource>,
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        for source in sources.drain(..) {
            if let Some(index) = source.input_candidate {
                self.effect_inputs[index].release = self.events.as_ref().unwrap().len();
            }
            #[cfg(any(feature = "gxm", test))]
            if let Some(mut candidate) = source.macrotile {
                candidate.release = self.events.as_ref().unwrap().len();
                super::scopes::push_stack(&mut self.macrotiles, candidate, &mut *reserve)?;
            }
            self.release(source.size, reserve)?;
        }
        Ok(())
    }
}
#[expect(
    clippy::too_many_arguments,
    reason = "one existing command stream retains its coordinate and capture domain"
)]
fn stream<'a>(
    drawing: &'a FrameDrawing,
    commands: &'a [FrameCommand],
    offset: Option<usize>,
    transitions_enabled: bool,
    transform: Affine,
    size: [u32; 2],
    scissor: Scissor,
    capture: bool,
    schedule: &offscreen::Plan,
    scratch: &mut Scratch,
    reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<Stream<'a>, GpuError> {
    let groups = if let Some(groups) = schedule.groups(commands, transitions_enabled) {
        groups
    } else {
        super::scopes::Proof::Owned(crate::common::scopes::GroupPlan::new(
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
            &mut *reserve,
        )?)
    };
    scratch.acquire(size, reserve)?;
    Ok(Stream {
        source_crop: None,
        retained_context: 0,
        scheduled: false,
        deferred: Vec::new(),
        drawing,
        commands,
        offset,
        transitions_enabled,
        transition_next: drawing
            .transitions()
            .partition_point(|t| t.commands.start < offset.unwrap_or(0)),
        pending: None,
        next: 0,
        groups,
        scopes: Vec::new(),
        transform,
        size,
        scissor,
        #[cfg(any(feature = "gxm", test))]
        macrotile: None,
    })
}
#[cfg(test)]
#[expect(
    clippy::too_many_arguments,
    reason = "one physical frame, schedule and native mask execution policy"
)]
pub(crate) fn target_pixels(
    frame: &RenderFrame,
    second: Option<&RenderFrame>,
    size: [u32; 2],
    capture: bool,
    samples: u32,
    max_dimension: u32,
    glyph_plan: &crate::common::glyph_plan::Plan,
    schedule: &offscreen::Plan,
    masks: MaskExecution,
    selection: &super::retained::Plan,
    reserve: impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<u64, GpuError> {
    Input {
        frame,
        second,
        size,
        capture,
        samples,
        max_dimension,
        glyphs: glyph_plan,
        schedule,
        masks,
        selection,
        cost: &|size| Ok(u64::from(size[0]) * u64::from(size[1]) * 4),
        coverage_cost: None,
    }
    .measure(reserve)
    .map(|measured| measured.bytes / 4)
}

/// One frame, its ordered schedule and its native target execution policy.
#[derive(Clone, Copy)]
pub(crate) struct Input<'a> {
    pub frame: &'a RenderFrame,
    pub second: Option<&'a RenderFrame>,
    pub size: [u32; 2],
    pub capture: bool,
    pub samples: u32,
    pub max_dimension: u32,
    pub glyphs: &'a super::glyph_plan::Plan,
    pub schedule: &'a offscreen::Plan,
    pub masks: MaskExecution,
    pub selection: &'a super::retained::Plan,
    pub cost: &'a dyn Fn([u32; 2]) -> Result<u64, GpuError>,
    pub coverage_cost: Option<&'a dyn Fn([u32; 2]) -> Result<u64, GpuError>>,
}

mod measure;

impl Input<'_> {
    fn walk(
        &self,
        record: bool,
        select_macrotiles: bool,
        mut effect_inputs: Option<&mut inputs::Collector<'_>>,
        mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Scratch, GpuError> {
        if !self.schedule.effect_inputs().is_empty() {
            return self.effect_input_scratch(record, select_macrotiles, &mut reserve);
        }
        let _ = select_macrotiles;
        let Self {
            frame,
            second,
            size,
            capture,
            samples,
            max_dimension,
            glyphs: glyph_plan,
            schedule,
            masks,
            selection,
            cost: _,
            coverage_cost,
        } = *self;
        let mut scratch = Scratch {
            events: record.then(Vec::new),
            ..Scratch::default()
        };
        for (index, frame) in [Some(frame), second].into_iter().flatten().enumerate() {
            let transform = Affine::scale_non_uniform(
                f64::from(size[0]) / frame.info().logical_size[0],
                f64::from(size[1]) / frame.info().logical_size[1],
            );
            let mut first = stream(
                frame.drawing(),
                frame.commands(),
                Some(0),
                true,
                transform,
                size,
                Scissor::full(size),
                capture,
                schedule,
                &mut scratch,
                &mut reserve,
            )?;
            first.scheduled = index == 0;
            let mut stack = Vec::new();
            super::scopes::push_stack(&mut stack, first, &mut reserve)?;
            loop {
                let current = stack.last_mut().unwrap();
                if current.transitions_enabled
                    && let Some(transition) =
                        current.drawing.transitions().get(current.transition_next)
                    && transition.commands.start == current.offset.unwrap() + current.next
                {
                    current.transition_next += 1;
                    current.next = transition.commands.end - current.offset.unwrap();
                    if capture && !transition.capture_included {
                        continue;
                    }
                    let retained = selection.entry(current.retained_context, transition);
                    if let Some((_, entry)) = retained
                        && entry.input.is_some()
                    {
                        current.pending = Some(Pending::New {
                            source: current.size,
                            mask_surface: matches!(masks, MaskExecution::Sampled)
                                && transition.mask.is_some()
                                && transition.progress > 0.
                                && transition.progress < 1.,
                            cached: true,
                        });
                        let child = stream(
                            current.drawing,
                            &current.drawing.commands()[transition.commands.clone()],
                            Some(transition.commands.start),
                            false,
                            current.transform,
                            current.size,
                            current.scissor,
                            capture,
                            schedule,
                            &mut scratch,
                            &mut reserve,
                        )?;
                        super::scopes::push_stack(&mut stack, child, &mut reserve)?;
                        continue;
                    }
                    current.pending =
                        Some(Pending::Old(transition, retained.map(|(index, _)| index)));
                    let mut child = stream(
                        transition.previous.drawing(),
                        transition.previous.drawing().commands(),
                        Some(0),
                        true,
                        current.transform
                            * transition.parent_to_window
                            * transition.previous.to_parent(),
                        current.size,
                        current.scissor,
                        capture,
                        schedule,
                        &mut scratch,
                        &mut reserve,
                    )?;
                    child.retained_context = retained.map_or(0, |(index, _)| index + 1);
                    super::scopes::push_stack(&mut stack, child, &mut reserve)?;
                    continue;
                }
                if capture
                    && current.next < current.commands.len()
                    && let Some(offset) = current.offset
                    && let Some(end) = current.drawing.capture_exclusion_end(offset + current.next)
                {
                    current.next = (end - offset).min(current.commands.len());
                    current.transition_next = current
                        .drawing
                        .transitions()
                        .partition_point(|t| t.commands.start < end);
                    continue;
                }
                let Some(command) = current.commands.get(current.next) else {
                    let finished = stack.pop().unwrap();
                    let Some(parent) = stack.last_mut() else {
                        break;
                    };
                    match parent.pending.take().expect("suspended target plan") {
                        Pending::Old(transition, entry) => {
                            if entry.is_some() && entry == selection.warm {
                                return Ok(scratch);
                            }
                            parent.pending = Some(Pending::New {
                                source: finished.size,
                                mask_surface: matches!(masks, MaskExecution::Sampled)
                                    && transition.mask.is_some()
                                    && transition.progress > 0.
                                    && transition.progress < 1.,
                                cached: false,
                            });
                            let child = stream(
                                parent.drawing,
                                &parent.drawing.commands()[transition.commands.clone()],
                                Some(transition.commands.start),
                                false,
                                parent.transform,
                                parent.size,
                                parent.scissor,
                                capture,
                                schedule,
                                &mut scratch,
                                &mut reserve,
                            )?;
                            super::scopes::push_stack(&mut stack, child, &mut reserve)?;
                        }
                        Pending::New {
                            source,
                            mask_surface,
                            cached,
                        } => {
                            scratch.composite(
                                parent.size,
                                false,
                                mask_surface,
                                coverage_cost.is_some(),
                                &mut reserve,
                            )?;
                            if !cached {
                                scratch.release(source, &mut reserve)?;
                            }
                            scratch.release(finished.size, &mut reserve)?;
                        }
                        Pending::Mask {
                            source,
                            backdrop,
                            mask_surface,
                        } => {
                            scratch.composite(
                                parent.size,
                                backdrop,
                                mask_surface,
                                coverage_cost.is_some(),
                                &mut reserve,
                            )?;
                            scratch.release(source, &mut reserve)?;
                            scratch.release(finished.size, &mut reserve)?;
                        }
                    }
                    continue;
                };
                current.next += 1;
                let at = command.origin.at;
                match &command.op {
                    FrameOp::PushClip(clip) => {
                        let parent_scissor = current.scissor;
                        current.scissor =
                            Scissor::from_clip(clip, current.transform, current.size, samples)
                                .or_else(|| {
                                    Scissor::clip_bounds(clip, current.transform, current.size)
                                })
                                .map_or(parent_scissor, |s| parent_scissor.intersect(s));
                        let direct_mesh = samples == 1
                            && crate::common::scopes::flat_clip(
                                current.drawing,
                                current.commands,
                                current.next,
                                current.offset,
                                current.transitions_enabled,
                                capture,
                            )
                            && crate::common::mesh::RectClip::from_clip(
                                clip,
                                current.transform,
                                current.size,
                            )
                            .is_some();
                        let isolated =
                            Scissor::from_clip(clip, current.transform, current.size, samples)
                                .is_none()
                                && !direct_mesh;
                        if isolated {
                            scratch.acquire(current.size, &mut reserve)?;
                        }
                        super::scopes::push_stack(
                            &mut current.scopes,
                            Scope {
                                push: None,
                                macro_source: false,
                                defer: false,
                                shared_source: false,
                                end_batch: false,
                                isolated,
                                forward_source: false,
                                clip: isolated,
                                group: None,
                                clip_child: isolated
                                    .then(|| {
                                        super::effect_plan::clipped_effect(
                                            current.drawing,
                                            current.commands,
                                            current.next,
                                            samples,
                                            capture || !current.drawing.transitions().is_empty(),
                                        )
                                    })
                                    .flatten(),
                                retained_source: None,
                                restore: None,
                                source_effect_transform: None,
                                parent_scissor,
                                source_view: false,
                                source_owner: None,
                                input_candidate: None,
                            },
                            &mut reserve,
                        )?;
                    }
                    FrameOp::PushGroup(group) => {
                        if let Some(consumed) = crate::common::scopes::invisible_group(
                            group,
                            &current.commands[current.next..],
                            current.transform,
                            current.size,
                            current.scissor,
                            current.transitions_enabled
                                && !current.drawing.transitions().is_empty()
                                || capture && !current.drawing.capture_exclusions().is_empty(),
                        ) {
                            current.next += consumed;
                            continue;
                        }
                        if self.source_free(current, group, &mut scratch, &mut reserve, at)? {
                            continue;
                        }
                        if let Some(fill) = crate::common::scopes::inline_group_fill(
                            group,
                            &current.commands[current.next..],
                            current.transform,
                            current.size,
                            samples,
                        ) && !fill.crosses_boundary(
                            current.drawing,
                            current.offset,
                            current.next,
                            current.transitions_enabled,
                            capture,
                        ) {
                            current.next += fill.consumed;
                            continue;
                        }
                        let isolated = !current.groups.can_inline(current.next - 1);
                        let parent_scissor = current.scissor;
                        let ink = glyph_plan.group_ink(group);
                        let batch = current
                            .scheduled
                            .then(|| schedule.entry(current.next - 1))
                            .flatten();
                        if let Some(entry) = batch.filter(|entry| entry.canonical_push.is_some()) {
                            current.next = entry.end;
                            if entry.last {
                                scratch.release_deferred(&mut current.deferred, &mut reserve)?;
                            }
                            continue;
                        }
                        let view = batch.and_then(|entry| entry.view).or_else(|| {
                            isolated
                                .then(|| {
                                    crate::common::source_view::SourceView::group(
                                        group,
                                        ink,
                                        current.transform,
                                        current.size,
                                        parent_scissor,
                                        current.transitions_enabled
                                            && !current.drawing.transitions().is_empty(),
                                        max_dimension,
                                    )
                                })
                                .flatten()
                        });
                        let mut forward_source = false;
                        let mut crop_source = None;
                        let mut defer_source = false;
                        let mut source_effect_transform = None;
                        let effect_transform = super::effect_plan::output_transform(
                            group,
                            current
                                .scopes
                                .last()
                                .and_then(|scope| scope.group.zip(scope.source_effect_transform)),
                            current.transform,
                        );
                        let restore = if let Some(view) = view {
                            let restore = (current.transform, current.size);
                            current.transform = view.frame_to_source(current.transform);
                            current.size = view.storage_size;
                            current.scissor = view.source_rect;
                            Some(restore)
                        } else if let Some(effect) =
                            group.effect.as_ref().or(group.mask_effect.as_ref())
                        {
                            let parent = current.scopes.last().and_then(|scope| scope.group);
                            let source = if super::effect_plan::same_source_domain(group, parent) {
                                super::effect_plan::SourceLayout {
                                    size: current.size,
                                    frame_to_source: current.transform,
                                    effect_to_source: effect_transform,
                                }
                            } else {
                                crate::common::effect_plan::source_layout(
                                    effect,
                                    group.effect_to_target,
                                    current.transform,
                                    max_dimension,
                                    at,
                                )?
                            };
                            let output_scissor = Scissor::group_bounds(
                                group,
                                current.transform,
                                current.size,
                                current.transitions_enabled
                                    && !current.drawing.transitions().is_empty(),
                            )
                            .map_or(parent_scissor, |s| parent_scissor.intersect(s));
                            let source_scissor = super::effect_plan::roi::source_scissor(
                                current.drawing,
                                group,
                                effect_transform,
                                current.size,
                                output_scissor,
                                source.size,
                                schedule.storage_size(source.size),
                            );
                            forward_source = current.groups.is_source_child(current.next - 1)
                                && current.scopes.last().is_some_and(|scope| scope.isolated)
                                && super::effect_plan::forwards_source(
                                    group,
                                    &source,
                                    current.transform,
                                    current.size,
                                    current
                                        .scopes
                                        .last()
                                        .is_some_and(|scope| scope.shared_source),
                                );
                            if current.source_crop.is_none()
                                && isolated
                                && !forward_source
                                && batch.is_none_or(|entry| entry.view.is_none())
                                && schedule.source_crops()
                                && (schedule.crop_backing(source.size).is_some()
                                    || schedule.storage_size(source.size) == source.size)
                            {
                                current.source_crop = super::source_crop::plan(
                                    current.drawing,
                                    current.commands,
                                    current.next - 1,
                                    &current.groups,
                                    &source,
                                    schedule.crop_backing(source.size),
                                    source_scissor,
                                    capture || samples != 1,
                                )
                                .filter(|plan| {
                                    schedule.storage_size(plan.crop.size) == plan.crop.size
                                });
                            }
                            defer_source = current
                                .source_crop
                                .is_some_and(|plan| plan.leaf != current.next - 1);
                            if current
                                .source_crop
                                .is_some_and(|plan| plan.leaf == current.next - 1)
                            {
                                crop_source = current.source_crop.take().map(|plan| plan.crop);
                                scratch.cropped = true;
                                if let Some(crop) = crop_source {
                                    for size in [
                                        crop.physical.map_or(crop.logical, |_| crop.size),
                                        crop.size,
                                    ] {
                                        if !scratch.exact.contains(&size) {
                                            super::scopes::push_stack(
                                                &mut scratch.exact,
                                                size,
                                                &mut reserve,
                                            )?;
                                        }
                                    }
                                }
                            }
                            let restore = (current.transform, current.size);
                            current.transform = crop_source
                                .map_or(source.frame_to_source, |crop| {
                                    crop.transform(source.frame_to_source)
                                });
                            source_effect_transform =
                                Some(crop_source.map_or(source.effect_to_source, |crop| {
                                    crop.transform(source.effect_to_source)
                                }));
                            current.size =
                                crop_source.map_or(source.size, |crop| crop.raster_size());
                            current.scissor = crop_source
                                .map_or(source_scissor, |crop| crop.scissor(source_scissor));
                            Some(restore)
                        } else {
                            current.scissor = Scissor::group_bounds(
                                group,
                                current.transform,
                                current.size,
                                current.transitions_enabled
                                    && !current.drawing.transitions().is_empty(),
                            )
                            .map_or(parent_scissor, |s| parent_scissor.intersect(s));
                            None
                        };
                        #[cfg(any(feature = "gxm", test))]
                        if select_macrotiles && current.macrotile.is_none() {
                            current.macrotile = macrotile::open(
                                current,
                                group,
                                isolated
                                    && !forward_source
                                    && view.is_none()
                                    && batch.is_none_or(|entry| entry.view.is_none()),
                                capture,
                                samples,
                                schedule,
                                scratch.events.as_ref().map_or(0, Vec::len),
                            )?;
                        }
                        let source_begin = scratch.events.as_ref().map_or(0, Vec::len);
                        let macro_source = if forward_source {
                            current
                                .scopes
                                .last()
                                .is_some_and(|scope| scope.macro_source)
                        } else {
                            schedule.collect_output_crops()
                                && current.offset == Some(0)
                                && super::effect_output::macro_source(
                                    current.drawing,
                                    current.next - 1,
                                    &current.groups,
                                    schedule.storage_size(current.size),
                                )
                        };
                        let original_source = crop_source
                            .is_none_or(|crop| crop.original_coordinates())
                            && current
                                .source_crop
                                .is_none_or(|plan| plan.crop.original_coordinates());
                        let source_owner = (effect_inputs.is_some()
                            && isolated
                            && view.is_none()
                            && original_source
                            && inputs::ordinary(group))
                        .then(|| {
                            current.offset.map(|offset| {
                                let key = super::effect_inputs::Key::new(
                                    current.drawing,
                                    offset + current.next - 1,
                                    super::effect_inputs::Raster::new(
                                        current.transform,
                                        current.size,
                                        schedule.storage_size(current.size),
                                        current.scissor,
                                    ),
                                );
                                let parent = current
                                    .scopes
                                    .last()
                                    .filter(|scope| {
                                        super::effect_plan::same_source_domain(group, scope.group)
                                    })
                                    .and_then(|scope| scope.source_owner)
                                    .filter(|_| forward_source);
                                (!forward_source || parent.is_some()).then_some(inputs::Source {
                                    key,
                                    root: parent.map_or(key, |source| source.root),
                                    begin: parent.map_or(source_begin, |source| source.begin),
                                })
                            })
                        })
                        .flatten()
                        .flatten();
                        if let Some(collector) = effect_inputs.as_deref_mut()
                            && forward_source
                            && original_source
                            && let Some(candidate) = collector.candidate(
                                current,
                                group,
                                effect_transform,
                                parent_scissor,
                                schedule,
                            )
                        {
                            let index = scratch.effect_inputs.len();
                            super::scopes::push_stack(
                                &mut scratch.effect_inputs,
                                candidate,
                                &mut reserve,
                            )?;
                            current
                                .scopes
                                .last_mut()
                                .expect("source parent")
                                .input_candidate = Some(index);
                        }
                        if isolated
                            && (!forward_source || crop_source.is_some())
                            && !defer_source
                            && batch.is_none_or(|entry| !entry.shared || entry.first)
                        {
                            scratch.acquire(
                                crop_source.map_or(current.size, |crop| crop.size),
                                &mut reserve,
                            )?;
                        }
                        super::scopes::push_stack(
                            &mut current.scopes,
                            Scope {
                                push: current.offset.map(|offset| offset + current.next - 1),
                                macro_source,
                                defer: batch.is_some(),
                                shared_source: batch.is_some_and(|entry| entry.shared),
                                end_batch: batch.is_some_and(|entry| entry.last),
                                isolated,
                                clip: group.clip.is_some(),
                                group: Some(group),
                                source_effect_transform,
                                clip_child: None,
                                retained_source: crop_source.map(|crop| crop.size),
                                restore,
                                parent_scissor,
                                source_view: view.is_some(),
                                forward_source,
                                source_owner,
                                input_candidate: None,
                            },
                            &mut reserve,
                        )?;
                    }
                    FrameOp::PopClip | FrameOp::PopGroup => {
                        let scope = current.scopes.pop().expect("validated scope");
                        if let Some(index) = scope.input_candidate {
                            let candidate = &mut scratch.effect_inputs[index];
                            candidate.events.end = scratch.events.as_ref().unwrap().len();
                            candidate.entry.resume = current.offset.unwrap() + current.next - 1;
                        }
                        #[cfg(any(feature = "gxm", test))]
                        let macrotile = current
                            .macrotile
                            .take_if(|open| open.depth == current.scopes.len());
                        current.scissor = scope.parent_scissor;
                        if !scope.isolated {
                            continue;
                        }
                        if scope.forward_source {
                            let source_size = current.size;
                            let (transform, size) = scope.restore.expect("forwarded source domain");
                            current.transform = transform;
                            current.size = size;
                            let output = self
                                .output_crop(current, &scope, &mut scratch, &mut reserve)?
                                .map_or(size, |crop| crop.size);
                            let reduced = scratch.begin_resolution(
                                size,
                                scope.group.and_then(|g| g.effect.as_ref()),
                                &mut reserve,
                            )?;
                            scratch.acquire(output, &mut reserve)?;
                            scratch.end_resolution(reduced, &mut reserve)?;
                            scratch.release_input(
                                &scope,
                                scope.retained_source.unwrap_or(source_size),
                                &mut reserve,
                            )?;
                            current
                                .scopes
                                .last_mut()
                                .expect("forwarded parent")
                                .retained_source = (output != size).then_some(output);
                            continue;
                        }
                        let mut source = scope.retained_source.unwrap_or(current.size);
                        if let Some(group) = scope.group.filter(|group| group.mask_effect.is_some())
                        {
                            // Each effect replaces the complete local source. The
                            // scalar mask writes its covered source in one pass.
                            if group.effect.is_some() {
                                let reduced = scratch.begin_resolution(
                                    current.size,
                                    group.effect.as_ref(),
                                    &mut reserve,
                                )?;
                                scratch.acquire(source, &mut reserve)?;
                                scratch.end_resolution(reduced, &mut reserve)?;
                                scratch.release_input(&scope, source, &mut reserve)?;
                            }
                            scratch.acquire(source, &mut reserve)?;
                            scratch.release(source, &mut reserve)?;
                        }
                        if let Some((transform, parent_size)) = scope.restore {
                            current.transform = transform;
                            current.size = parent_size;
                            if scope.source_view {
                                if scope.defer {
                                    if !scope.shared_source || current.deferred.is_empty() {
                                        super::scopes::push_stack(
                                            &mut current.deferred,
                                            DeferredSource::ordinary(source),
                                            &mut reserve,
                                        )?;
                                    }
                                    if scope.end_batch {
                                        // All window sources precede parent replay.
                                        // Interleaved ordinary draws allocate no targets;
                                        // these same deferred lifetimes cover that phase.
                                        scratch.release_deferred(
                                            &mut current.deferred,
                                            &mut reserve,
                                        )?;
                                    }
                                } else {
                                    let backdrop = scope.group.is_some_and(|group| {
                                        schedule.destination().backdrop(
                                            group.composite.blend,
                                            true,
                                            false,
                                        )
                                    });
                                    scratch.local_backdrop |= backdrop;
                                    scratch.composite(
                                        schedule.backdrop_size(source, current.size),
                                        backdrop,
                                        false,
                                        coverage_cost.is_some(),
                                        &mut reserve,
                                    )?;
                                    scratch.release(source, &mut reserve)?;
                                }
                                continue;
                            }
                            if scope.group.is_some_and(|group| {
                                super::effect_plan::direct_mask_projection(
                                    group,
                                    current.transitions_enabled
                                        && !current.drawing.transitions().is_empty(),
                                )
                            }) {
                                scratch.release_input(&scope, source, &mut reserve)?;
                                continue;
                            }
                            if scope.group.is_some_and(|definition| {
                                current
                                    .scopes
                                    .last()
                                    .and_then(|parent| parent.clip_child)
                                    .is_some_and(|child| std::ptr::eq(child, definition))
                            }) && super::effect_plan::coverage_retention(source, parent_size)
                            {
                                scratch.release(parent_size, &mut reserve)?;
                                current
                                    .scopes
                                    .last_mut()
                                    .expect("clip parent")
                                    .retained_source = Some(source);
                                continue;
                            }
                            let reduced = scratch.begin_resolution(
                                parent_size,
                                scope
                                    .group
                                    .filter(|g| g.mask_effect.is_none())
                                    .and_then(|g| g.effect.as_ref()),
                                &mut reserve,
                            )?;
                            if scope.group.is_some_and(|group| {
                                super::effect_plan::terminal_composite(
                                    group,
                                    samples,
                                    capture || !current.drawing.transitions().is_empty(),
                                )
                            }) {
                                scratch.end_resolution(reduced, &mut reserve)?;
                                #[cfg(any(feature = "gxm", test))]
                                let macrotile = macrotile.map(|open| {
                                    open.finish(scratch.events.as_ref().map_or(0, Vec::len))
                                });
                                if scope.defer {
                                    super::scopes::push_stack(
                                        &mut current.deferred,
                                        DeferredSource {
                                            size: source,
                                            input_candidate: scope.input_candidate,
                                            #[cfg(any(feature = "gxm", test))]
                                            macrotile,
                                        },
                                        &mut reserve,
                                    )?;
                                    if scope.end_batch {
                                        scratch.release_deferred(
                                            &mut current.deferred,
                                            &mut reserve,
                                        )?;
                                    }
                                } else {
                                    scratch.release_input(&scope, source, &mut reserve)?;
                                    #[cfg(any(feature = "gxm", test))]
                                    if let Some(candidate) = macrotile {
                                        super::scopes::push_stack(
                                            &mut scratch.macrotiles,
                                            candidate,
                                            &mut reserve,
                                        )?;
                                    }
                                }
                                continue;
                            }
                            let coverage_tail = scope.group.is_some_and(|group| {
                                super::effect_plan::terminal_tail(
                                    current.drawing,
                                    group,
                                    samples,
                                    capture || !current.drawing.transitions().is_empty(),
                                ) == Some(crate::effect::OutputTail::AlphaCoverage)
                            });
                            if !coverage_tail
                                || !super::effect_plan::coverage_retention(source, parent_size)
                            {
                                scratch.acquire(parent_size, &mut reserve)?;
                                scratch.release_input(&scope, source, &mut reserve)?;
                                source = parent_size;
                            }
                            scratch.end_resolution(reduced, &mut reserve)?;
                        }
                        let mask = scope.group.and_then(|group| group.mask);
                        let backdrop = scope.group.is_some_and(|group| {
                            schedule.destination().backdrop(
                                group.composite.blend,
                                !scope.clip && mask.is_none(),
                                group.image_mask.is_some(),
                            )
                        });
                        if scope.clip {
                            let single = coverage_cost.is_some()
                                && single_clip(scope.group, scope.retained_source.is_some());
                            if single {
                                scratch.coverage(current.size, true, &mut reserve)?;
                            } else {
                                scratch.acquire(current.size, &mut reserve)?;
                            }
                            if mask.is_none() {
                                scratch.composite(
                                    current.size,
                                    backdrop,
                                    masks.surface(scope.group, false),
                                    coverage_cost.is_some(),
                                    &mut reserve,
                                )?;
                                if single {
                                    scratch.coverage(current.size, false, &mut reserve)?;
                                } else {
                                    scratch.release(current.size, &mut reserve)?;
                                }
                                scratch.release(source, &mut reserve)?;
                                continue;
                            }
                            scratch.acquire(current.size, &mut reserve)?;
                            if single {
                                scratch.coverage(current.size, false, &mut reserve)?;
                            } else {
                                scratch.release(current.size, &mut reserve)?;
                            }
                            scratch.release(source, &mut reserve)?;
                            source = current.size;
                        }
                        if let Some(mask) = mask {
                            let definition = current.drawing.masks().get(&mask.mask).ok_or(
                                GpuError::Unsupported {
                                    at,
                                    record: "missing mask",
                                },
                            )?;
                            current.pending = Some(Pending::Mask {
                                source,
                                backdrop,
                                mask_surface: masks.surface(scope.group, false),
                            });
                            let child = stream(
                                current.drawing,
                                &definition.commands,
                                None,
                                false,
                                current.transform * mask.transform,
                                current.size,
                                current.scissor,
                                capture,
                                schedule,
                                &mut scratch,
                                &mut reserve,
                            )?;
                            super::scopes::push_stack(&mut stack, child, &mut reserve)?;
                        } else if scope.defer {
                            super::scopes::push_stack(
                                &mut current.deferred,
                                DeferredSource::ordinary(source),
                                &mut reserve,
                            )?;
                            if scope.end_batch {
                                scratch.release_deferred(&mut current.deferred, &mut reserve)?;
                            }
                        } else {
                            scratch.composite(
                                current.size,
                                backdrop,
                                masks.surface(scope.group, true),
                                coverage_cost.is_some(),
                                &mut reserve,
                            )?;
                            scratch.release(source, &mut reserve)?;
                        }
                    }
                    _ => {}
                }
            }
        }
        if second.is_some() {
            scratch.acquire(size, &mut reserve)?;
            scratch.release(size, &mut reserve)?;
            scratch.release(size, &mut reserve)?;
        }
        Ok(scratch)
    }
}

/// Shared mapped high-water recount for selected crops and macrotile spans.
fn recount(
    events: &[super::target_storage::Event],
    selected: impl Iterator<Item = std::ops::Range<usize>>,
    storage: impl Fn([u32; 2]) -> [u32; 2],
    scratch: &mut Scratch,
    reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<(), GpuError> {
    let mut selected = selected.peekable();
    for bucket in &mut scratch.buckets {
        bucket.live = 0;
        bucket.peak = 0;
    }
    for (index, event) in events.iter().enumerate() {
        while selected.peek().is_some_and(|r| r.end <= index) {
            selected.next();
        }
        if selected.peek().is_some_and(|r| r.contains(&index)) {
            continue;
        }
        let size = storage(event.size);
        if event.acquired {
            scratch.acquire(size, reserve)?;
        } else {
            scratch.release(size, reserve)?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disjoint_color_and_coverage_lifetimes_keep_distinct_physical_peaks() {
        let mut scratch = Scratch::default();
        let mut reserve = |_| Ok(());
        let size = [64, 32];
        for _ in 0..2 {
            scratch.acquire(size, &mut reserve).unwrap();
        }
        for _ in 0..2 {
            scratch.release(size, &mut reserve).unwrap();
        }
        for _ in 0..3 {
            scratch.coverage(size, true, &mut reserve).unwrap();
        }
        for _ in 0..3 {
            scratch.coverage(size, false, &mut reserve).unwrap();
        }
        let color = scratch
            .cost(&|s| Ok(u64::from(s[0]) * u64::from(s[1]) * 4))
            .unwrap();
        let coverage = scratch
            .coverage_cost(Some(&|s| Ok(u64::from(s[0]) * u64::from(s[1]))))
            .unwrap();
        assert_eq!((color, coverage), (64 * 32 * 4 * 2, 64 * 32 * 3));
    }
    #[test]
    fn sampled_scalar_mask_keeps_a_distinct_coverage_peak() {
        let mut scratch = Scratch::default();
        let mut reserve = |_| Ok(());
        let size = [63, 31];
        scratch.acquire(size, &mut reserve).unwrap();
        scratch
            .composite(size, true, true, true, &mut reserve)
            .unwrap();
        let color = scratch
            .cost(&|s| Ok(u64::from(s[0]) * u64::from(s[1]) * 4))
            .unwrap();
        let alpha = scratch
            .coverage_cost(Some(&|s| {
                Ok(u64::from(s[0].next_multiple_of(4)) * u64::from(s[1]))
            }))
            .unwrap();
        assert_eq!(color, 63 * 31 * 4 * 2);
        assert_eq!(alpha, 64 * 31);
    }
    #[test]
    fn different_source_sizes_keep_independent_storage_high_water() {
        let mut scratch = Scratch::default();
        let mut reserve = |_| Ok(());
        scratch.acquire([160, 120], &mut reserve).unwrap();
        scratch.acquire([240, 80], &mut reserve).unwrap();
        scratch.acquire([160, 120], &mut reserve).unwrap();
        scratch.release([240, 80], &mut reserve).unwrap();
        scratch.release([160, 120], &mut reserve).unwrap();
        scratch.acquire([64, 64], &mut reserve).unwrap();
        scratch.acquire([160, 120], &mut reserve).unwrap();
        assert_eq!(
            scratch
                .cost(&|size| Ok(u64::from(size[0]) * u64::from(size[1])))
                .unwrap(),
            2 * 160 * 120 + 240 * 80 + 64 * 64
        );
        scratch.release([64, 64], &mut reserve).unwrap();
        scratch.release([160, 120], &mut reserve).unwrap();
        scratch.acquire([240, 80], &mut reserve).unwrap();
        assert_eq!(
            scratch
                .cost(&|size| Ok(u64::from(size[0]) * u64::from(size[1])))
                .unwrap(),
            2 * 160 * 120 + 240 * 80 + 64 * 64
        );
    }
}
