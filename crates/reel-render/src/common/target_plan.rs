//! Physical target lifetimes shared by admission and renderer scheduling.
use super::GpuError;
use super::offscreen;
use super::scissor::Scissor;
use reel_ui::frame::{
    FrameCommand, FrameDrawing, FrameGroup, FrameOp, RenderFrame, SceneTransition,
};
use reel_ui::kurbo::Affine;

/// Physical scratch multiplicities from the selected lifetime traversal.
#[derive(Default)]
pub(crate) struct Requirements(Vec<Requirement>);
#[derive(Clone, Copy)]
struct Requirement {
    size: [u32; 2],
    count: u64,
}
impl Requirements {
    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = ([u32; 2], u64)> + '_ {
        self.0.iter().map(|entry| (entry.size, entry.count))
    }
    #[cfg(test)]
    pub(crate) fn capacity_bytes(&self) -> u64 {
        (self.0.capacity() * size_of::<Requirement>()) as u64
    }
    pub(crate) fn count(&self, size: [u32; 2]) -> u64 {
        self.0
            .binary_search_by_key(&size, |bucket| bucket.size)
            .map_or(0, |index| self.0[index].count)
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
        Ok(Self(result))
    }
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
    buckets: Vec<Bucket>,
    local_backdrop: bool,
    events: Option<Vec<super::target_storage::Event>>,
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
    fn composite(
        &mut self,
        size: [u32; 2],
        needed: bool,
        mask_surface: bool,
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        if mask_surface {
            self.acquire(size, reserve)?;
        }
        if needed {
            self.acquire(size, reserve)?;
            self.release(size, reserve)?;
        }
        if mask_surface {
            self.release(size, reserve)?;
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
    clip_child: Option<&'a FrameGroup>,
    retained_source: Option<[u32; 2]>,
    defer: bool,
    shared_source: bool,
    end_batch: bool,
    isolated: bool,
    forward_source: bool,
    clip: bool,
    group: Option<&'a FrameGroup>,
    restore: Option<(Affine, [u32; 2])>,
    parent_scissor: Scissor,
    source_view: bool,
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
    retained_context: usize,
    scheduled: bool,
    deferred: Vec<[u32; 2]>,
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
}

pub(crate) struct Measured {
    pub bytes: u64,
    pub reusable: bool,
    pub local_backdrop: bool,
    scratch: Scratch,
}
impl Measured {
    pub(crate) fn requirements(
        &self,
        reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Requirements, GpuError> {
        Requirements::collect(
            self.scratch
                .buckets
                .iter()
                .map(|bucket| (bucket.size, bucket.peak)),
            reserve,
        )
    }
}

impl Input<'_> {
    pub(crate) fn measure(
        &self,
        reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Measured, GpuError> {
        let scratch = self.walk(false, reserve)?;
        let reusable = scratch
            .buckets
            .iter()
            .filter(|bucket| bucket.size != self.size)
            .take(2)
            .count()
            == 2;
        let bytes = scratch
            .cost(self.cost)?
            .checked_add(self.selection.persistent_bytes())
            .ok_or(GpuError::InvalidSize)?;
        Ok(Measured {
            bytes,
            reusable,
            local_backdrop: scratch.local_backdrop,
            scratch,
        })
    }

    /// Derive a physical storage map from the executor's exact target lifetimes.
    /// Temporary planning tickets end here; mapping tickets remain charged through encoding.
    pub(crate) fn storage<R>(
        &self,
        mut reserve: impl FnMut(u64) -> Result<R, GpuError>,
    ) -> Result<(super::target_storage::Plan, u64, Vec<R>), GpuError> {
        let mut temporary = Vec::new();
        let scratch = self.walk(true, |bytes| {
            temporary.push(reserve(bytes)?);
            Ok(())
        })?;
        let (plan, retained) = super::target_storage::Plan::from_lifetimes(
            scratch
                .events
                .as_deref()
                .expect("recorded target lifetimes"),
            self.size,
            self.cost,
            reserve,
        )?;
        let bytes = plan
            .bytes()
            .checked_add(self.selection.persistent_bytes())
            .ok_or(GpuError::InvalidSize)?;
        Ok((plan, bytes, retained))
    }

    fn walk(
        &self,
        record: bool,
        mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Scratch, GpuError> {
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
                            scratch.composite(parent.size, false, mask_surface, &mut reserve)?;
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
                            scratch.composite(parent.size, backdrop, mask_surface, &mut reserve)?;
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
                                parent_scissor,
                                source_view: false,
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
                        let view = batch.map(|entry| entry.view).or_else(|| {
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
                            forward_source = current.groups.is_source_child(current.next - 1)
                                && current.scopes.last().is_some_and(|scope| scope.isolated)
                                && super::effect_plan::forwards_source(
                                    group,
                                    &source,
                                    current.transform,
                                    current.size,
                                );
                            let restore = (current.transform, current.size);
                            current.transform = source.frame_to_source;
                            current.size = source.size;
                            current.scissor = Scissor::full(source.size);
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
                        if isolated
                            && !forward_source
                            && batch.is_none_or(|entry| !entry.shared || entry.first)
                        {
                            scratch.acquire(current.size, &mut reserve)?;
                        }
                        super::scopes::push_stack(
                            &mut current.scopes,
                            Scope {
                                defer: batch.is_some(),
                                shared_source: batch.is_some_and(|entry| entry.shared),
                                end_batch: batch.is_some_and(|entry| entry.last),
                                isolated,
                                clip: group.clip.is_some(),
                                group: Some(group),
                                clip_child: None,
                                retained_source: None,
                                restore,
                                parent_scissor,
                                source_view: view.is_some(),
                                forward_source,
                            },
                            &mut reserve,
                        )?;
                    }
                    FrameOp::PopClip | FrameOp::PopGroup => {
                        let scope = current.scopes.pop().expect("validated scope");
                        current.scissor = scope.parent_scissor;
                        if !scope.isolated {
                            continue;
                        }
                        if scope.forward_source {
                            let (transform, size) = scope.restore.expect("forwarded source domain");
                            current.transform = transform;
                            current.size = size;
                            scratch.acquire(size, &mut reserve)?;
                            scratch.release(size, &mut reserve)?;
                            continue;
                        }
                        let mut source = scope.retained_source.unwrap_or(current.size);
                        if let Some(group) = scope.group.filter(|group| group.mask_effect.is_some())
                        {
                            // Each effect replaces the complete local source. The
                            // scalar mask writes its covered source in one pass.
                            if group.effect.is_some() {
                                scratch.acquire(source, &mut reserve)?;
                                scratch.release(source, &mut reserve)?;
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
                                            source,
                                            &mut reserve,
                                        )?;
                                    }
                                    if scope.end_batch {
                                        for source in current.deferred.drain(..) {
                                            scratch.release(source, &mut reserve)?;
                                        }
                                    }
                                } else {
                                    let backdrop = scope.group.is_some_and(|group| {
                                        group.composite.blend != Default::default()
                                    });
                                    scratch.local_backdrop |= backdrop;
                                    scratch.composite(
                                        schedule.backdrop_size(source, current.size),
                                        backdrop,
                                        false,
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
                                scratch.release(source, &mut reserve)?;
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
                            if scope.group.is_some_and(|group| {
                                super::effect_plan::terminal_composite(
                                    group,
                                    samples,
                                    capture || !current.drawing.transitions().is_empty(),
                                )
                            }) {
                                scratch.release(source, &mut reserve)?;
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
                                scratch.release(source, &mut reserve)?;
                                source = parent_size;
                            }
                        }
                        let mask = scope.group.and_then(|group| group.mask);
                        let backdrop = scope
                            .group
                            .is_some_and(|group| group.composite.blend != Default::default());
                        if scope.clip {
                            scratch.acquire(current.size, &mut reserve)?;
                            if mask.is_none() {
                                scratch.composite(
                                    current.size,
                                    backdrop,
                                    masks.surface(scope.group, false),
                                    &mut reserve,
                                )?;
                                scratch.release(current.size, &mut reserve)?;
                                scratch.release(source, &mut reserve)?;
                                continue;
                            }
                            scratch.acquire(current.size, &mut reserve)?;
                            scratch.release(current.size, &mut reserve)?;
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
                        } else {
                            scratch.composite(
                                current.size,
                                backdrop,
                                masks.surface(scope.group, true),
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
#[cfg(test)]
mod tests {
    use super::*;
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
