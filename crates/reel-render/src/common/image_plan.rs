//! CPU-only image and mask demand for the commands that a preparation replays.
use super::{GpuError, scopes::push_stack};
use reel_ui::{
    frame::{FrameDraw, FrameImageMask, FrameOp, RecordLocation, RenderFrame},
    peniko::{Brush, ImageFormat},
    resources::{ResourceHandle, ResourceLease, ResourceView},
};
use std::collections::{HashMap, HashSet};

pub(crate) struct Required<'a> {
    pub(crate) source: &'a ResourceLease,
    pub(crate) at: RecordLocation,
    pub(crate) texture: Option<super::resources::TextureUploadLayout>,
}

pub(crate) struct Mask<'a> {
    pub(crate) definition: &'a FrameImageMask,
    pub(crate) at: RecordLocation,
}

pub(crate) struct Plan<'a> {
    pub(crate) images: Vec<Required<'a>>,
    pub(crate) masks: Vec<Mask<'a>>,
}

pub(crate) fn collect<'a>(
    frames: impl Iterator<Item = &'a RenderFrame>,
    max_dimension: u32,
    capture: bool,
    selection: &super::retained::Plan,
    mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<Plan<'a>, GpuError> {
    let mut plan = Plan {
        images: Vec::new(),
        masks: Vec::new(),
    };
    let mut seen = HashSet::new();
    let mut drawings = Vec::new();
    let mut visited_drawings = HashSet::new();
    for frame in frames {
        let at = frame.commands().first().map_or(
            RecordLocation {
                fragment: 0,
                command: 0,
                mask_depth: 0,
                owner: frame.geometry().root_id(),
            },
            |c| c.origin.at,
        );
        push_stack(&mut drawings, (frame.drawing(), at), &mut reserve)?;
    }
    while let Some((frame, fallback_at)) = drawings.pop() {
        if !visited_drawings.insert(frame as *const _) {
            continue;
        }
        let leases = frame.resources().leases();
        reserve(
            (leases.len()
                * (size_of::<Required<'_>>()
                    + 4 * size_of::<ResourceHandle>()
                    + 2 * size_of::<&ResourceLease>())
                + (frame.masks().len() + frame.transitions().len() + 1) * 96) as u64,
        )?;
        let sources: HashMap<_, _> = leases.iter().map(|lease| (lease.handle(), lease)).collect();
        let mut visited = HashSet::new();
        let mut used = Vec::new();
        let mut used_handles = HashSet::new();
        let mut streams = Vec::new();
        push_stack(&mut streams, (frame.commands(), Some(0)), &mut reserve)?;
        for transition in frame.transitions() {
            if !selection.transition_needed(transition) || capture && !transition.capture_included {
                continue;
            }
            let at = frame
                .commands()
                .get(transition.commands.start)
                .map_or(fallback_at, |c| c.origin.at);
            if selection.old_needed(transition) {
                push_stack(
                    &mut drawings,
                    (transition.previous.drawing(), at),
                    &mut reserve,
                )?;
            }
            if let Some(mask) = &transition.mask {
                // Progress zero still requires both images before the target is accepted.
                push_stack(
                    &mut plan.masks,
                    Mask {
                        definition: mask,
                        at,
                    },
                    &mut reserve,
                )?;
                for &handle in &mask.images {
                    if used_handles.insert(handle) {
                        push_stack(&mut used, (handle, at), &mut reserve)?;
                    }
                }
            }
        }
        while let Some((commands, offset)) = streams.pop() {
            let mut index = 0;
            while index < commands.len() {
                if capture
                    && let Some(offset) = offset
                    && let Some(end) = frame.capture_exclusion_end(offset + index)
                {
                    index = (end - offset).min(commands.len());
                    continue;
                }
                let command = &commands[index];
                index += 1;
                if let Some(offset) = offset
                    && !selection.selected(frame, offset + index - 1)
                {
                    continue;
                }
                let at = command.origin.at;
                let handles: &[ResourceHandle] = match &command.op {
                    FrameOp::Draw(
                        FrameDraw::Fill {
                            brush: Brush::Image(brush),
                            ..
                        }
                        | FrameDraw::Stroke {
                            brush: Brush::Image(brush),
                            ..
                        },
                    ) => std::slice::from_ref(&brush.image.resource),
                    FrameOp::Draw(FrameDraw::Sprites(s)) => {
                        std::slice::from_ref(&s.brush.image.resource)
                    }
                    FrameOp::Draw(FrameDraw::GlyphRun(g)) => match &g.brush {
                        Brush::Image(brush) => std::slice::from_ref(&brush.image.resource),
                        _ => &[],
                    },
                    FrameOp::PushGroup(group) => {
                        for effect in group.effect.iter().chain(group.mask_effect.iter()) {
                            for &handle in &effect.images {
                                if used_handles.insert(handle) {
                                    push_stack(&mut used, (handle, at), &mut reserve)?;
                                }
                            }
                        }
                        if let Some(mask) = group.mask
                            && visited.insert(mask.mask)
                        {
                            let definition =
                                frame.masks().get(&mask.mask).ok_or(GpuError::Unsupported {
                                    at,
                                    record: "missing mask",
                                })?;
                            push_stack(&mut streams, (&definition.commands, None), &mut reserve)?;
                        }
                        if let Some(mask) = &group.image_mask {
                            push_stack(
                                &mut plan.masks,
                                Mask {
                                    definition: mask,
                                    at,
                                },
                                &mut reserve,
                            )?;
                            &mask.images
                        } else {
                            &[]
                        }
                    }
                    _ => continue,
                };
                for &handle in handles {
                    if used_handles.insert(handle) {
                        push_stack(&mut used, (handle, at), &mut reserve)?;
                    }
                }
            }
        }
        for (handle, at) in used {
            if !seen.insert(handle) {
                continue;
            }
            let source = *sources.get(&handle).ok_or(GpuError::Unsupported {
                at,
                record: "missing image lease",
            })?;
            let mut texture = None;
            let size = if let Some(descriptor) = source.texture_descriptor() {
                let upload = super::resources::TextureUploadLayout::new(descriptor)?;
                texture = Some(upload);
                upload.layout.storage_size
            } else if let Some(image) = source.image_descriptor() {
                if image.format != ImageFormat::Rgba8 {
                    return Err(GpuError::Unsupported {
                        at,
                        record: "image format",
                    });
                }
                [image.width, image.height]
            } else if let ResourceView::Yuv420(image) = source.view() {
                image.size()
            } else {
                return Err(GpuError::Unsupported {
                    at,
                    record: "non-image resource",
                });
            };
            if size.contains(&0) || size.iter().any(|&n| n > max_dimension) {
                return Err(GpuError::InvalidSize);
            }
            push_stack(
                &mut plan.images,
                Required {
                    source,
                    at,
                    texture,
                },
                &mut reserve,
            )?;
        }
    }
    Ok(plan)
}

/// Retain exact missing identities and charge their full vector capacity.
pub(crate) fn missing_pixels<'a>(
    sources: impl Iterator<Item = &'a ResourceLease> + Clone,
    pool: &reel_core::budget::BudgetPool,
    limit: u64,
) -> Result<Option<crate::PixelDemand>, GpuError> {
    let missing =
        |source: &&ResourceLease| source.pixel_byte_len().is_some() && !source.pixels_resident();
    if !sources.clone().any(|source| missing(&source)) {
        return Ok(None);
    }
    pixel_demand(sources, pool, limit, missing)
}

fn pixel_demand<'a>(
    sources: impl Iterator<Item = &'a ResourceLease>,
    pool: &reel_core::budget::BudgetPool,
    limit: u64,
    include: impl FnMut(&&ResourceLease) -> bool,
) -> Result<Option<crate::PixelDemand>, GpuError> {
    // Residency can change between the scan and collection. Reserve the
    // iterator's full bound so a concurrent eviction cannot grow the vector.
    let capacity = sources.size_hint().1.ok_or(GpuError::InvalidSize)?;
    let bytes = capacity
        .checked_mul(size_of::<ResourceLease>())
        .ok_or(GpuError::InvalidSize)?;
    let mut storage = pool.reserve(bytes as u64).map_err(|_| GpuError::Pressure {
        domain: "pixel demand metadata",
        requested: bytes as u64,
        available: limit.saturating_sub(pool.usage().bytes),
    })?;
    let mut resources = Vec::new();
    resources
        .try_reserve_exact(capacity)
        .map_err(|_| GpuError::InvalidSize)?;
    storage
        .resize((resources.capacity() * size_of::<ResourceLease>()) as u64)
        .map_err(|_| GpuError::Pressure {
            domain: "pixel demand metadata",
            requested: (resources.capacity() * size_of::<ResourceLease>()) as u64,
            available: limit.saturating_sub(pool.usage().bytes),
        })?;
    resources.extend(sources.filter(include).cloned());
    if resources.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        crate::PixelDemand::new(resources, storage).map_err(|_| GpuError::InvalidSize)?,
    ))
}

/// Acquire a pixel reader or report a concurrently evicted source.
pub(crate) fn required_pixels(
    source: &ResourceLease,
    pool: &reel_core::budget::BudgetPool,
    limit: u64,
) -> Result<reel_ui::resources::ResourcePixels, GpuError> {
    if let Some(reader) = source.read_pixels() {
        return Ok(reader);
    }
    if source.pixel_byte_len().is_none() {
        return Err(GpuError::InvalidSize);
    }
    // A concurrent restore can satisfy this demand before the player sees it.
    // The player already completes resident demands without starting IO.
    let demand =
        pixel_demand(std::iter::once(source), pool, limit, |_| true)?.expect("one image source");
    Err(GpuError::PixelsNeeded(demand))
}

/// Every demanded image coexists; only the largest indivisible upload overlaps.
pub(crate) fn peak(images: impl Iterator<Item = (u64, u64)>, limit: u64) -> Result<u64, GpuError> {
    let mut resident = 0u64;
    let mut staging = 0u64;
    for (bytes, upload) in images {
        resident = resident.checked_add(bytes).ok_or(GpuError::InvalidSize)?;
        staging = staging.max(upload);
    }
    if resident > limit {
        return Err(GpuError::Budget {
            domain: "image residency",
            requested: resident,
            available: limit,
        });
    }
    let peak = resident.checked_add(staging).ok_or(GpuError::InvalidSize)?;
    if peak > limit {
        return Err(GpuError::Budget {
            domain: "image residency and upload",
            requested: peak,
            available: limit,
        });
    }
    Ok(resident)
}

#[cfg(test)]
#[path = "image_plan_tests.rs"]
mod tests;
