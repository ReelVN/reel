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
    pub(super) minimum_staging: u64,
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
                let (size, resident, staging, minimum_staging) = match source.data() {
                    ResourceData::Image { image, .. } => {
                        if image.format != ImageFormat::Rgba8 {
                            return Err(GpuError::Unsupported {
                                at,
                                record: "image format",
                            });
                        }
                        let size = [image.width, image.height];
                        let resident = u64::from(image.width) * u64::from(image.height) * 8;
                        (
                            size,
                            resident,
                            resources::upload_cost(size)? - resident,
                            resources::row_bytes(size[0]),
                        )
                    }
                    ResourceData::Yuv420(image) => {
                        let (resident, staging) = super::video::costs(image);
                        (image.size(), resident, staging, staging)
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
                    minimum_staging,
                });
            }
        }
    }
    Ok(required)
}

/// All textures must coexist for rendering, but uploads only require their
/// smallest copy unit. Larger batches use available space opportunistically.
pub(super) fn peak(images: impl Iterator<Item = (u64, u64)>, limit: u64) -> Result<u64, GpuError> {
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
mod tests {
    use super::*;

    #[test]
    fn cold_images_fit_with_row_staging_and_residency_excess_is_permanent() {
        let resident = 1920 * 1080 * 8;
        let staging = resources::upload_cost([1920, 1080]).unwrap() - resident;
        let limit = 128 << 20;
        assert!(5 * (resident + staging) > limit);
        assert_eq!(
            peak(std::iter::repeat_n((resident, staging), 5), limit).unwrap(),
            5 * resident
        );
        let row = resources::row_bytes(1920);
        assert_eq!(
            peak(std::iter::repeat_n((resident, row), 8), limit).unwrap(),
            8 * resident
        );
        assert!(matches!(
            peak(std::iter::repeat_n((resident, row), 9), limit),
            Err(GpuError::Budget {
                domain: "image residency",
                ..
            })
        ));
        // A warm frame needs residency only, independent of the original upload peak.
        assert_eq!(
            peak(std::iter::repeat_n((resident, 0), 8), limit).unwrap(),
            8 * resident
        );
    }

    #[test]
    fn large_image_upload_progresses_with_only_one_row_of_spare_capacity() {
        let size = [2048, 1959];
        let resident = u64::from(size[0]) * u64::from(size[1]) * 8;
        let row = resources::row_bytes(size[0]);
        let limit = resident + row;
        assert!(resources::upload_cost(size).unwrap() > limit);
        assert_eq!(
            peak(std::iter::once((resident, row)), limit).unwrap(),
            resident
        );
        let mut next = 0;
        let mut converted = 0;
        while next < size[1] {
            let count = resources::rows_per_upload(size, next, limit - resident);
            assert_eq!(count, 1);
            next += count;
            converted += u64::from(count) * u64::from(size[0]);
        }
        assert_eq!(converted, u64::from(size[0]) * u64::from(size[1]));
        assert_eq!(resources::rows_per_upload(size, 0, row - 1), 0);
        assert_eq!(resources::rows_per_upload(size, size[1] - 1, 4 * row), 1);
        assert!(matches!(
            peak(std::iter::once((resident, row)), limit - 1),
            Err(GpuError::Budget {
                domain: "image residency and upload",
                ..
            })
        ));
    }
}
