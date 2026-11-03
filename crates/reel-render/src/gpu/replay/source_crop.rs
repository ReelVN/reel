//! Select the shared raw-source crop before allocating its first writer.
use super::*;
use crate::common::{
    effect_plan::SourceLayout,
    source_crop::{self, Crop},
};

impl Context<'_> {
    pub(super) fn enter_source_raster(
        &mut self,
        current: &mut Stream<'_>,
        source: &SourceLayout,
        crop: Option<Crop>,
        demand: &mut Option<Scissor>,
    ) {
        current.transform = crop.map_or(source.frame_to_source, |crop| {
            crop.transform(source.frame_to_source)
        });
        current.size = crop.map_or(source.size, |crop| crop.raster_size());
        self.size = current.size;
        if let Some(crop) = crop {
            *demand = demand.map(|region| crop.scissor(region));
        }
        self.mesh_clip = None;
    }

    pub(super) fn source_crop(
        &self,
        current: &mut Stream<'_>,
        source: &SourceLayout,
        demand: Scissor,
        eligible: bool,
    ) -> (bool, Option<Crop>) {
        if current.source_crop.is_none()
            && eligible
            && self.offscreen.source_crops()
            && (self.offscreen.crop_backing(source.size).is_some()
                || self.offscreen.storage_size(source.size) == source.size)
        {
            current.source_crop = source_crop::plan(
                current.drawing,
                current.commands,
                current.next - 1,
                &current.groups,
                source,
                self.offscreen.crop_backing(source.size),
                demand,
                self.capture || self.quality.samples != 1,
            )
            .filter(|plan| self.offscreen.storage_size(plan.crop.size) == plan.crop.size);
        }
        let defer = current
            .source_crop
            .is_some_and(|plan| plan.leaf != current.next - 1);
        let crop = if current
            .source_crop
            .is_some_and(|plan| plan.leaf == current.next - 1)
        {
            current.source_crop.take().map(|plan| plan.crop)
        } else {
            None
        };
        (defer, crop)
    }

    pub(in crate::gpu) fn cropped_target(&mut self, crop: Crop) -> Result<Target, GpuError> {
        let size = self.size;
        self.size = crop.size;
        let result = self.target();
        self.size = size;
        let mut target = result?;
        target.size = size;
        target.source_crop = Some(crop);
        Ok(target)
    }
}
