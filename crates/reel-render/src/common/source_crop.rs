//! Smaller backing for one proved, independently rasterized ordinary source.
use super::{effect_plan, scissor::Scissor, scopes::GroupPlan};
#[cfg(feature = "profiling")]
use crate::profiling::CropDecision;
#[cfg(test)]
mod tests;
use reel_ui::{
    frame::{FrameCommand, FrameDraw, FrameDrawing, FrameOp},
    imaging::record::Geometry,
    kurbo::Affine,
    peniko::Brush,
    resources::ResourceView,
    stage::effect::EffectType,
};

/// Original logical source and a dyadic integer window on its unchanged raster.
/// Normalized coordinates scale by powers of two; source-domain clamps remain
/// independent of the smaller backing. Origin-zero physical windows may also
/// back materialized effects while their content keeps its original grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Crop {
    pub logical: [u32; 2],
    pub origin: [u32; 2],
    pub size: [u32; 2],
    /// Original padded backing; Some retains the complete original raster map.
    pub physical: Option<[u32; 2]>,
}

#[derive(Clone, Copy)]
pub(crate) struct Planned {
    pub crop: Crop,
    pub leaf: usize,
}

impl Crop {
    pub(crate) fn original_coordinates(self) -> bool {
        self.physical.is_some() && self.origin == [0; 2]
    }

    pub(crate) fn canonical_storage(self) -> [u32; 2] {
        self.physical.unwrap_or(self.logical)
    }

    /// Consumer results retain their original content identity. The raw backing
    /// itself is never published; source records undo only the integer placement.
    pub(crate) fn original_grid(
        self,
        mut grid: super::effect_result::Grid,
    ) -> super::effect_result::Grid {
        let physical = Affine::new(grid.source_transform.map(f64::from_bits));
        grid.source_transform =
            (Affine::translate((f64::from(self.origin[0]), f64::from(self.origin[1]))) * physical)
                .as_coeffs()
                .map(f64::to_bits);
        grid.source_size = self.logical;
        grid.source_storage = self.physical.unwrap_or(self.logical);
        grid.source_scissor[0] += self.origin[0];
        grid.source_scissor[1] += self.origin[1];
        grid
    }

    pub(crate) fn window(logical: [u32; 2], demand: Scissor) -> Option<Self> {
        if logical.contains(&0) || demand.empty() {
            return None;
        }
        let low = [demand.x, demand.y];
        let high = [
            demand.x.checked_add(demand.width)?,
            demand.y.checked_add(demand.height)?,
        ];
        let mut size = logical;
        let mut origin = [0; 2];
        for axis in 0..2 {
            if high[axis] > logical[axis] {
                return None;
            }
            // At a non-power-of-two original edge, (1-half)*scale-cell can
            // round differently from (scale-cell)-new_half. Keep that axis
            // unchanged if the demand reaches the upper edge. At origin zero,
            // the lower half-texel bound scales exactly by a power of two.
            if !logical[axis].is_power_of_two() && high[axis] == logical[axis] {
                continue;
            }
            // Do not trade a smaller payload for sub-tile target classes.
            while size[axis].is_multiple_of(2) && size[axis] / 2 >= 32 {
                let next = size[axis] / 2;
                let cell = low[axis] / next;
                if (high[axis] - 1) / next != cell {
                    break;
                }
                size[axis] = next;
                origin[axis] = cell * next;
            }
        }
        (size != logical).then_some(Self {
            logical,
            origin,
            size,
            physical: None,
        })
    }

    /// Slice only the unused right/bottom backing. Original vertices, viewport,
    /// WPOS and interpolation stay identical; only attachment bounds get smaller.
    pub(crate) fn physical_window(
        logical: [u32; 2],
        storage: [u32; 2],
        demand: Scissor,
    ) -> Option<Self> {
        if logical.contains(&0) || demand.empty() {
            return None;
        }
        let high = [
            demand.x.checked_add(demand.width)?,
            demand.y.checked_add(demand.height)?,
        ];
        if (0..2).any(|i| high[i] > logical[i] || logical[i] > storage[i]) {
            return None;
        }
        let mut size = storage;
        for axis in 0..2 {
            while size[axis].is_multiple_of(2)
                && size[axis] / 2 >= 32
                && high[axis] <= size[axis] / 2
            {
                size[axis] /= 2;
            }
        }
        (size != storage).then_some(Self {
            logical,
            origin: [0; 2],
            size,
            physical: Some(storage),
        })
    }

    pub(crate) fn raster_size(self) -> [u32; 2] {
        if self.physical.is_some() {
            self.logical
        } else {
            self.size
        }
    }

    pub(crate) fn transform(self, original: Affine) -> Affine {
        if self.physical.is_some() {
            return original;
        }
        Affine::translate((-f64::from(self.origin[0]), -f64::from(self.origin[1]))) * original
    }

    pub(crate) fn scissor(self, demand: Scissor) -> Scissor {
        Scissor {
            x: demand.x - self.origin[0],
            y: demand.y - self.origin[1],
            ..demand
        }
    }

    pub(crate) fn map(self, context: &mut [[f32; 4]; crate::effect::CONTEXT_SLOTS]) {
        if let Some(storage) = self.physical {
            // Preserve the original rounded logical/physical coefficients before
            // the exact power-of-two rescale. Recomputing logical/new is different.
            effect_plan::map_storage(context, self.logical, storage);
            for axis in 0..2 {
                let scale = (storage[axis] / self.size[axis]) as f32;
                for row in [2 + axis, 7 + axis] {
                    for value in &mut context[row][..3] {
                        *value *= scale;
                    }
                }
                context[5][2 + axis] = self.size[axis] as f32;
                context[6][axis] *= scale;
                context[6][axis + 2] *= scale;
            }
            return;
        }
        for axis in 0..2 {
            let scale = (self.logical[axis] / self.size[axis]) as f32;
            let cell = (self.origin[axis] / self.size[axis]) as f32;
            // Authored samples use the original identity source map. Keep the
            // original logical domain, including its edge clamp, after rebasing.
            let opacity = context[2 + axis][3];
            context[2 + axis] = if axis == 0 {
                [scale, 0., -cell, opacity]
            } else {
                [0., scale, -cell, opacity]
            };
            for value in &mut context[7 + axis][..3] {
                *value *= scale;
            }
            context[7 + axis][2] -= cell;
            context[5][2 + axis] = self.size[axis] as f32;
            context[6][axis] = -cell;
            context[6][axis + 2] = scale;
        }
    }
}

/// Prove the actual raw producer before changing any allocation or transform.
/// Capture, transitions, masks, nested effects and multiple producers retain
/// their existing complete backing. Only rectangle fills on the exact 1/16
/// pixel raster are admitted; arbitrary tessellation keeps its original path.
#[expect(
    clippy::too_many_arguments,
    reason = "one source proof combines command scope, raster storage, demand and capture boundaries"
)]
pub(crate) fn plan(
    drawing: &FrameDrawing,
    commands: &[FrameCommand],
    root: usize,
    groups: &GroupPlan,
    source: &effect_plan::SourceLayout,
    physical: Option<[u32; 2]>,
    demand: Scissor,
    boundary: bool,
) -> Option<Planned> {
    macro_rules! reject {
        ($reason:ident) => {{
            #[cfg(feature = "profiling")]
            crate::profiling::record_source_crop_plan(CropDecision::$reason);
            return None;
        }};
    }
    if boundary
        || source.size.into_iter().any(|size| size > 4096)
        || !drawing.transitions().is_empty()
        || !drawing.masks().is_empty()
        || !drawing.capture_exclusions().is_empty()
    {
        reject!(Boundary);
    }
    let mut leaf = root;
    let mut demand = demand;
    if let Some(chain) = super::effect_chain::plan(commands, root, groups) {
        // Existing macro candidates retain their original complete cells.
        if super::effect_chain::layout(source.size, chain.count + 1).is_some() {
            reject!(Macro);
        }
        for &push in chain.effects[..chain.count].iter().rev() {
            let FrameOp::PushGroup(group) = &commands.get(push)?.op else {
                return None;
            };
            let output = Scissor::group_bounds(group, source.frame_to_source, source.size, false)
                .map_or(demand, |bounds| demand.intersect(bounds));
            demand = effect_plan::roi::source_scissor(
                drawing,
                group,
                source.frame_to_source * group.effect_to_target,
                source.size,
                output,
                source.size,
                source.size,
            );
            leaf = push;
        }
    }
    let FrameOp::PushGroup(group) = &commands.get(leaf)?.op else {
        reject!(Boundary);
    };
    if group.mask_effect.is_some()
        || group.clip.is_some()
        || group.mask.is_some()
        || group.image_mask.is_some()
        || !group.filters.is_empty()
    {
        reject!(Footprint);
    }
    let effect = group.effect.as_ref()?;
    let descriptor = drawing.resources().leases().iter().find_map(|lease| {
        if lease.handle() != effect.shader {
            return None;
        }
        let ResourceView::EffectShader { descriptor, .. } = lease.view() else {
            return None;
        };
        Some(descriptor)
    })?;
    if descriptor.source_footprint.is_none()
        || !descriptor.source_sampler
        || descriptor.output != EffectType::Vec4
    {
        reject!(Footprint);
    }
    let crop = physical.map_or_else(
        || Crop::window(source.size, demand),
        |storage| Crop::physical_window(source.size, storage, demand),
    );
    let Some(crop) = crop else {
        #[cfg(feature = "profiling")]
        crate::profiling::record_source_crop_window(
            source.size,
            [demand.x, demand.y, demand.width, demand.height],
        );
        reject!(Window);
    };
    let mut depth = 0usize;
    let mut fill = false;
    for (index, command) in commands.iter().enumerate().skip(leaf + 1) {
        match &command.op {
            FrameOp::PushContext(_) | FrameOp::PopContext => {}
            FrameOp::PushGroup(_) if groups.can_inline(index) => depth += 1,
            FrameOp::PopGroup if depth != 0 => depth -= 1,
            FrameOp::PopGroup => {
                if !fill {
                    reject!(Producer);
                }
                #[cfg(feature = "profiling")]
                crate::profiling::record_source_crop_plan(CropDecision::Accepted);
                return Some(Planned { crop, leaf });
            }
            FrameOp::Draw(FrameDraw::Fill {
                transform,
                shape: Geometry::Rect(rect),
                composite,
                brush,
                ..
            }) if !fill && composite.blend == Default::default() => {
                if crop.physical.is_none() {
                    match brush {
                        Brush::Solid(_) => {}
                        Brush::Image(image)
                            if image.image.appearance.is_none_or(|a| a.gradient.is_none()) => {}
                        _ => reject!(Brush),
                    }
                    let transform = source.frame_to_source * *transform;
                    let [a, b, c, d, _, _] = transform.as_coeffs();
                    if b != 0. || c != 0. || a == 0. || d == 0. {
                        reject!(Axis);
                    }
                    for corner in [rect.origin(), (rect.x1, rect.y1).into()] {
                        if [corner.x, corner.y]
                            .iter()
                            .any(|n| f64::from(*n as f32) != *n)
                        {
                            reject!(Grid);
                        }
                        let point = transform * corner;
                        if [point.x, point.y]
                            .iter()
                            .any(|n| !n.is_finite() || (n * 16.).fract() != 0.)
                        {
                            reject!(Grid);
                        }
                        if point.x < 0.
                            || point.y < 0.
                            || point.x > f64::from(source.size[0])
                            || point.y > f64::from(source.size[1])
                        {
                            reject!(Bounds);
                        }
                    }
                }
                fill = true;
            }
            _ => reject!(Producer),
        }
    }
    None
}
