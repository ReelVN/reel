//! WGPU storage costs for the shared CPU-only image demand.
use super::{GpuError, resources};
use crate::common::image_plan;
pub(super) use image_plan::peak;
use reel_ui::{
    frame::{RecordLocation, RenderFrame},
    peniko::ImageFormat,
    resources::{ColorEncoding, ResourceLease, ResourceView},
};

pub(super) struct Required<'a> {
    pub(super) source: &'a ResourceLease,
    pub(super) at: RecordLocation,
    pub(super) resident: u64,
    pub(super) staging: u64,
    pub(super) minimum_staging: u64,
    pub(super) raster_format: Option<wgpu::TextureFormat>,
    pub(super) texture: Option<crate::common::resources::TextureUploadLayout>,
}
pub(super) struct Plan<'a> {
    pub(super) images: Vec<Required<'a>>,
    pub(super) aliases: Vec<image_plan::Alias<'a>>,
    pub(super) masks: Vec<image_plan::Mask<'a>>,
}
pub(super) fn collect<'a>(
    frames: impl Iterator<Item = &'a RenderFrame>,
    max_dimension: u32,
    capture: bool,
    selection: &crate::common::retained::Plan,
    mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<Plan<'a>, GpuError> {
    let plan = image_plan::collect(frames, max_dimension, capture, selection, &mut reserve)?;
    reserve((plan.images.len() * size_of::<Required<'_>>()) as u64)?;
    let images = plan
        .images
        .into_iter()
        .map(|item| {
            let mut raster_format = None;
            let (resident, staging, minimum_staging) = if let Some(upload) = item.texture {
                let (resident, staging) = resources::texture_costs(upload);
                (resident, staging, resources::texture_row_bytes(upload))
            } else if let Some(image) = item.source.image_descriptor() {
                let format = if image.format == ImageFormat::Rgba8
                    && image.encoding == ColorEncoding::Srgb
                    && item
                        .source
                        .image_coverage()
                        .is_some_and(|coverage| coverage.is_opaque())
                {
                    wgpu::TextureFormat::Rgba8UnormSrgb
                } else {
                    wgpu::TextureFormat::Rgba16Float
                };
                raster_format = Some(format);
                let resident = u64::from(image.width)
                    * u64::from(image.height)
                    * u64::from(format.block_copy_size(None).unwrap());
                let row = resources::raster_row_bytes(image.width, format);
                (resident, row * u64::from(image.height), row)
            } else if let ResourceView::Yuv420(image) = item.source.view() {
                let (resident, staging) = super::video::costs(image);
                (resident, staging, staging)
            } else {
                unreachable!("common image demand validates resource kinds")
            };
            Ok(Required {
                source: item.source,
                at: item.at,
                resident,
                staging,
                minimum_staging,
                raster_format,
                texture: item.texture,
            })
        })
        .collect::<Result<_, GpuError>>()?;
    Ok(Plan {
        images,
        aliases: plan.aliases,
        masks: plan.masks,
    })
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
            let count = resources::rows_per_upload(size[1], next, limit - resident, row);
            assert_eq!(count, 1);
            next += count;
            converted += u64::from(count) * u64::from(size[0]);
        }
        assert_eq!(converted, u64::from(size[0]) * u64::from(size[1]));
        assert_eq!(resources::rows_per_upload(size[1], 0, row - 1, row), 0);
        assert_eq!(
            resources::rows_per_upload(size[1], size[1] - 1, 4 * row, row),
            1
        );
        assert!(matches!(
            peak(std::iter::once((resident, row)), limit - 1),
            Err(GpuError::Budget {
                domain: "image residency and upload",
                ..
            })
        ));
    }
}
