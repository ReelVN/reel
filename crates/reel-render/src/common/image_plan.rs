//! CPU-only image and mask demand for the commands that a preparation replays.
use super::{GpuError, scopes::push_stack};
use reel_ui::{
    frame::{FrameDraw, FrameImageMask, FrameOp, RecordLocation, RenderFrame},
    peniko::{Brush, ImageFormat},
    resources::{ResourceData, ResourceHandle, ResourceLease},
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
            } else if let ResourceData::Yuv420(image) = source.data() {
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
