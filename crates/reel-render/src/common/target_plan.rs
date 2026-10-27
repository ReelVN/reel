//! Physical target lifetimes shared by admission and renderer scheduling.
use super::GpuError;
use super::offscreen;
use super::scissor::Scissor;
use reel_ui::frame::{
    FrameCommand, FrameDrawing, FrameGroup, FrameOp, RenderFrame, SceneTransition,
};
use reel_ui::kurbo::Affine;

#[derive(Clone, Copy)]
pub(crate) enum MaskExecution {
    #[cfg_attr(
        not(feature = "wgpu"),
        allow(dead_code, reason = "selected by the WGPU executor")
    )]
    Inline,
    #[cfg_attr(
        not(feature = "gles2"),
        allow(dead_code, reason = "selected by the GLES executor")
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
    fn release(&mut self, size: [u32; 2]) {
        let bucket = self
            .buckets
            .iter_mut()
            .find(|bucket| bucket.size == size)
            .expect("allocated target size");
        bucket.live = bucket.live.checked_sub(1).expect("live target");
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
            self.release(size);
        }
        if mask_surface {
            self.release(size);
        }
        Ok(())
    }
    fn pixels(&self) -> Result<u64, GpuError> {
        self.buckets.iter().try_fold(0u64, |total, bucket| {
            u64::from(bucket.size[0])
                .checked_mul(u64::from(bucket.size[1]))
                .and_then(|v| v.checked_mul(bucket.peak))
                .and_then(|v| total.checked_add(v))
                .ok_or(GpuError::InvalidSize)
        })
    }
}
struct Scope<'a> {
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
    Old(&'a SceneTransition),
    New {
        source: [u32; 2],
        mask_surface: bool,
    },
    Mask {
        source: [u32; 2],
        backdrop: bool,
        mask_surface: bool,
    },
}
struct Stream<'a> {
    scheduled: bool,
    deferred: Vec<[u32; 2]>,
    drawing: &'a FrameDrawing,
    commands: &'a [FrameCommand],
    offset: Option<usize>,
    transitions_enabled: bool,
    transition_next: usize,
    pending: Option<Pending<'a>>,
    next: usize,
    groups: crate::common::scopes::GroupPlan,
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
    scratch: &mut Scratch,
    reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<Stream<'a>, GpuError> {
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
        &mut *reserve,
    )?;
    scratch.acquire(size, reserve)?;
    Ok(Stream {
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
/// Plan the same streams and target lifetimes as replay, in storage pixels.
/// A second frame keeps the first output alive for a complete-frame crossfade.
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
    mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<u64, GpuError> {
    let mut scratch = Scratch::default();
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
            &mut scratch,
            &mut reserve,
        )?;
        first.scheduled = index == 0;
        let mut stack = Vec::new();
        super::scopes::push_stack(&mut stack, first, &mut reserve)?;
        loop {
            let current = stack.last_mut().unwrap();
            if current.transitions_enabled
                && let Some(transition) = current.drawing.transitions().get(current.transition_next)
                && transition.commands.start == current.offset.unwrap() + current.next
            {
                current.transition_next += 1;
                current.next = transition.commands.end - current.offset.unwrap();
                if capture && !transition.capture_included {
                    continue;
                }
                current.pending = Some(Pending::Old(transition));
                let child = stream(
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
                    &mut scratch,
                    &mut reserve,
                )?;
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
                    Pending::Old(transition) => {
                        parent.pending = Some(Pending::New {
                            source: finished.size,
                            mask_surface: matches!(masks, MaskExecution::Sampled)
                                && transition.mask.is_some()
                                && transition.progress > 0.
                                && transition.progress < 1.,
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
                            &mut scratch,
                            &mut reserve,
                        )?;
                        super::scopes::push_stack(&mut stack, child, &mut reserve)?;
                    }
                    Pending::New {
                        source,
                        mask_surface,
                    } => {
                        scratch.composite(parent.size, false, mask_surface, &mut reserve)?;
                        scratch.release(source);
                        scratch.release(finished.size);
                    }
                    Pending::Mask {
                        source,
                        backdrop,
                        mask_surface,
                    } => {
                        scratch.composite(parent.size, backdrop, mask_surface, &mut reserve)?;
                        scratch.release(source);
                        scratch.release(finished.size);
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
                            .or_else(|| Scissor::clip_bounds(clip, current.transform, current.size))
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
                            restore: None,
                            parent_scissor,
                            source_view: false,
                        },
                        &mut reserve,
                    )?;
                }
                FrameOp::PushGroup(group) => {
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
                        scratch.release(size);
                        continue;
                    }
                    let mut source = current.size;
                    if let Some(group) = scope.group.filter(|group| group.mask_effect.is_some()) {
                        // Each effect replaces the complete local source. The
                        // scalar mask writes its covered source in one pass.
                        if group.effect.is_some() {
                            scratch.acquire(source, &mut reserve)?;
                            scratch.release(source);
                        }
                        scratch.acquire(source, &mut reserve)?;
                        scratch.release(source);
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
                                        scratch.release(source);
                                    }
                                }
                            } else {
                                scratch.composite(
                                    current.size,
                                    scope.group.is_some_and(|group| {
                                        group.composite.blend != Default::default()
                                    }),
                                    false,
                                    &mut reserve,
                                )?;
                                scratch.release(source);
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
                            scratch.release(source);
                            continue;
                        }
                        scratch.acquire(parent_size, &mut reserve)?;
                        scratch.release(source);
                        source = parent_size;
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
                            scratch.release(current.size);
                            scratch.release(source);
                            continue;
                        }
                        scratch.acquire(current.size, &mut reserve)?;
                        scratch.release(current.size);
                        scratch.release(source);
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
                        scratch.release(source);
                    }
                }
                _ => {}
            }
        }
    }
    if second.is_some() {
        scratch.acquire(size, &mut reserve)?;
        scratch.release(size);
        scratch.release(size);
    }
    scratch.pixels()
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
        scratch.release([240, 80]);
        scratch.release([160, 120]);
        scratch.acquire([64, 64], &mut reserve).unwrap();
        scratch.acquire([160, 120], &mut reserve).unwrap();
        assert_eq!(
            scratch.pixels().unwrap(),
            2 * 160 * 120 + 240 * 80 + 64 * 64
        );
        scratch.release([64, 64]);
        scratch.release([160, 120]);
        scratch.acquire([240, 80], &mut reserve).unwrap();
        assert_eq!(
            scratch.pixels().unwrap(),
            2 * 160 * 120 + 240 * 80 + 64 * 64
        );
    }
}
