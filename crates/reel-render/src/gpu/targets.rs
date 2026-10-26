use super::{GpuError, Quality};
pub(super) use crate::common::scissor::Scissor;
use reel_ui::frame::{
    FrameCommand, FrameDrawing, FrameGroup, FrameOp, RenderFrame, SceneTransition,
};
use reel_ui::kurbo::Affine;
#[cfg(test)]
#[path = "target_plan_tests.rs"]
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
    fn backdrop(
        &mut self,
        size: [u32; 2],
        needed: bool,
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        if needed {
            self.acquire(size, reserve)?;
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
    isolated: bool,
    clip: bool,
    group: Option<&'a FrameGroup>,
    restore: Option<(Affine, [u32; 2])>,
    parent_scissor: Scissor,
    source_view: bool,
}
enum Pending<'a> {
    Old(&'a SceneTransition),
    New([u32; 2]),
    Mask { source: [u32; 2], backdrop: bool },
}
struct Stream<'a> {
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
pub(super) fn target_pixels(
    frame: &RenderFrame,
    second: Option<&RenderFrame>,
    size: [u32; 2],
    capture: bool,
    samples: u32,
    max_dimension: u32,
    glyph_plan: &crate::common::glyph_plan::Plan,
    mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<u64, GpuError> {
    let mut scratch = Scratch::default();
    for frame in [Some(frame), second].into_iter().flatten() {
        let transform = Affine::scale_non_uniform(
            f64::from(size[0]) / frame.info().logical_size[0],
            f64::from(size[1]) / frame.info().logical_size[1],
        );
        let first = stream(
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
                        parent.pending = Some(Pending::New(finished.size));
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
                    Pending::New(previous) => {
                        scratch.release(previous);
                        scratch.release(finished.size);
                    }
                    Pending::Mask { source, backdrop } => {
                        scratch.backdrop(parent.size, backdrop, &mut reserve)?;
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
                            isolated,
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
                    let boundary = current.transitions_enabled
                        && current
                            .drawing
                            .transitions()
                            .get(current.transition_next)
                            .is_some_and(|t| {
                                t.commands.start < current.offset.unwrap() + current.next + 2
                            })
                        || capture
                            && current.offset.is_some_and(|offset| {
                                current
                                    .drawing
                                    .capture_exclusion_end(offset + current.next)
                                    .is_some()
                            });
                    if !boundary
                        && crate::common::scopes::inline_group_fill(
                            group,
                            &current.commands[current.next..],
                        )
                        .is_some()
                    {
                        current.next += 2;
                        continue;
                    }
                    let isolated = !current.groups.can_inline(current.next - 1);
                    let parent_scissor = current.scissor;
                    let ink = glyph_plan.group_ink(group);
                    let view = isolated
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
                        .flatten();
                    let restore = if let Some(view) = view {
                        let restore = (current.transform, current.size);
                        current.transform = view.frame_to_source(current.transform);
                        current.size = view.storage_size;
                        current.scissor = view.source_rect;
                        Some(restore)
                    } else if let Some(effect) =
                        group.effect.as_ref().or(group.mask_effect.as_ref())
                    {
                        let source = crate::common::effect_plan::source_layout(
                            effect,
                            group.effect_to_target,
                            current.transform,
                            max_dimension,
                            at,
                        )?;
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
                    if isolated {
                        scratch.acquire(current.size, &mut reserve)?;
                    }
                    super::scopes::push_stack(
                        &mut current.scopes,
                        Scope {
                            isolated,
                            clip: group.clip.is_some(),
                            group: Some(group),
                            restore,
                            parent_scissor,
                            source_view: view.is_some(),
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
                    let mut source = current.size;
                    if let Some(group) = scope.group.filter(|group| group.mask_effect.is_some()) {
                        // Color effects, coverage and their product are produced
                        // in the complete local domain before one projection.
                        if group.effect.is_some() {
                            scratch.acquire(source, &mut reserve)?;
                            scratch.release(source);
                        }
                        scratch.acquire(source, &mut reserve)?;
                        scratch.acquire(source, &mut reserve)?;
                        scratch.release(source);
                        scratch.release(source);
                    }
                    if let Some((transform, parent_size)) = scope.restore {
                        current.transform = transform;
                        current.size = parent_size;
                        if scope.source_view {
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
                            scratch.backdrop(current.size, backdrop, &mut reserve)?;
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
                        current.pending = Some(Pending::Mask { source, backdrop });
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
                        scratch.backdrop(current.size, backdrop, &mut reserve)?;
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
pub fn target_bytes(size: [u32; 2], quality: Quality) -> Result<u64, GpuError> {
    let bytes = match quality.format {
        wgpu::TextureFormat::Rgba16Float => 8,
        wgpu::TextureFormat::Rgba8Unorm => 4,
        _ => return Err(GpuError::InvalidSize),
    };
    u64::from(size[0])
        .checked_mul(u64::from(size[1]))
        .and_then(|n| n.checked_mul(bytes))
        .and_then(|n| {
            n.checked_mul(if quality.samples > 1 {
                u64::from(quality.samples) + 1
            } else {
                1
            })
        })
        .ok_or(GpuError::InvalidSize)
}

#[derive(Clone)]
pub struct Target {
    pub size: [u32; 2],
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub multisample: Option<wgpu::TextureView>,
}
impl Target {
    pub fn new(device: &wgpu::Device, size: [u32; 2], quality: Quality) -> Self {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::TargetCreate);
        let descriptor = wgpu::TextureDescriptor {
            label: Some("reel linear target"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: quality.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        };
        let texture = device.create_texture(&descriptor);
        let view = texture.create_view(&Default::default());
        let multisample = (quality.samples > 1).then(|| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    sample_count: quality.samples,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    ..descriptor
                })
                .create_view(&Default::default())
        });
        Self {
            size,
            texture,
            view,
            multisample,
        }
    }
    pub fn attachment(&self, clear: bool) -> wgpu::RenderPassColorAttachment<'_> {
        wgpu::RenderPassColorAttachment {
            view: self.multisample.as_ref().unwrap_or(&self.view),
            resolve_target: self.multisample.as_ref().map(|_| &self.view),
            depth_slice: None,
            ops: wgpu::Operations {
                load: if clear {
                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                } else {
                    wgpu::LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            },
        }
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
    #[test]
    fn resolve_and_msaa_are_both_charged() {
        assert_eq!(
            target_bytes(
                [1920, 1080],
                Quality {
                    format: wgpu::TextureFormat::Rgba16Float,
                    samples: 4,
                    degraded: false
                }
            )
            .unwrap(),
            82_944_000
        );
    }
}
