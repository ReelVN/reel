//! Unique image demand, including masks and both inputs of a transition.
use super::{GpuError, resources};
use reel_ui::{
    frame::{FrameDraw, FrameOp, RecordLocation, RenderFrame},
    peniko::{Brush, ImageFormat},
    resources::{ResourceData, ResourceHandle, ResourceLease},
};
use std::collections::{HashMap, HashSet};

pub(super) struct Required<'a> {
    pub(super) source: &'a ResourceLease,
    pub(super) at: RecordLocation,
    pub(super) resident: u64,
    pub(super) staging: u64,
}

pub(super) fn collect<'a>(
    frames: impl Iterator<Item = &'a RenderFrame>,
    max_dimension: u32,
    mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<Vec<Required<'a>>, GpuError> {
    let mut required = Vec::new();
    let mut seen = HashSet::new();
    for frame in frames {
        let leases = frame.resources().leases();
        // Charge the bounded lookup/work lists before allocating. They contain
        // at most one entry per frame lease/mask, not one per sprite instance.
        reserve(
            (leases.len()
                * (size_of::<Required<'_>>()
                    + 4 * size_of::<ResourceHandle>()
                    + 2 * size_of::<&ResourceLease>())
                + (frame.masks().len() + 1) * (size_of::<&[reel_ui::frame::FrameCommand]>() + 1))
                as u64,
        )?;
        let sources: HashMap<_, _> = leases.iter().map(|lease| (lease.handle(), lease)).collect();
        let mut visited = vec![false; frame.masks().len()];
        let mut streams = vec![frame.commands()];
        while let Some(commands) = streams.pop() {
            for command in commands {
                let at = command.origin.at;
                let brush = match &command.op {
                    FrameOp::Draw(
                        FrameDraw::Fill { brush, .. } | FrameDraw::Stroke { brush, .. },
                    ) => brush,
                    FrameOp::PushGroup(group) => {
                        if let Some(mask) = group.mask {
                            let index = mask.mask as usize;
                            let definition =
                                frame.masks().get(index).ok_or(GpuError::Unsupported {
                                    at,
                                    record: "missing mask",
                                })?;
                            if !visited[index] {
                                visited[index] = true;
                                streams.push(&definition.commands);
                            }
                        }
                        continue;
                    }
                    _ => continue,
                };
                let Brush::Image(brush) = brush else { continue };
                let handle = brush.image.resource;
                if !seen.insert(handle) {
                    continue;
                }
                let source = *sources.get(&handle).ok_or(GpuError::Unsupported {
                    at,
                    record: "missing image lease",
                })?;
                let (size, resident, staging) = match source.data() {
                    ResourceData::Image { image, .. } => {
                        if image.format != ImageFormat::Rgba8 {
                            return Err(GpuError::Unsupported {
                                at,
                                record: "image format",
                            });
                        }
                        let size = [image.width, image.height];
                        let resident = u64::from(image.width) * u64::from(image.height) * 8;
                        (size, resident, resources::upload_cost(size)? - resident)
                    }
                    ResourceData::Yuv420(image) => {
                        let (resident, staging) = super::video::costs(image);
                        (image.size(), resident, staging)
                    }
                    _ => {
                        return Err(GpuError::Unsupported {
                            at,
                            record: "non-image resource",
                        });
                    }
                };
                if size.contains(&0) || size.iter().any(|&n| n > max_dimension) {
                    return Err(GpuError::InvalidSize);
                }
                required.push(Required {
                    source,
                    at,
                    resident,
                    staging,
                });
            }
        }
    }
    Ok(required)
}

/// All textures must coexist for rendering, but only one missing texture's
/// staging must fit. Larger batches are opportunistic, never a prerequisite.
pub(super) fn peak(images: impl Iterator<Item = (u64, u64)>, limit: u64) -> Result<u64, GpuError> {
    let mut resident = 0u64;
    let mut staging = 0u64;
    for (bytes, upload) in images {
        resident = resident.checked_add(bytes).ok_or(GpuError::InvalidSize)?;
        staging = staging.max(upload);
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
mod tests {
    use super::*;

    #[test]
    fn full_hd_cold_restore_fits_without_simultaneous_staging() {
        let resident = 1920 * 1080 * 8;
        let staging = resources::upload_cost([1920, 1080]).unwrap() - resident;
        let limit = 128 << 20;
        assert!(5 * (resident + staging) > limit);
        assert_eq!(
            peak(std::iter::repeat_n((resident, staging), 5), limit).unwrap(),
            5 * resident
        );
        assert!(matches!(
            peak(std::iter::repeat_n((resident, staging), 8), limit),
            Err(GpuError::Budget { .. })
        ));
        // A warm frame needs residency only, independent of the original upload peak.
        assert_eq!(
            peak(std::iter::repeat_n((resident, 0), 8), limit).unwrap(),
            8 * resident
        );
    }
}
