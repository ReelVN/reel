//! Reuse a forwarded effect before its otherwise redundant source draw.
use super::*;
use crate::common::effect_plan;
use crate::common::effect_result::{Grid, Probe};

impl Context<'_> {
    /// The common group proof established that this effect is its isolated
    /// parent's sole source, with identical source grids and ordinary blending.
    pub(super) fn cached_source(
        &mut self,
        current: &mut Stream<'_>,
        group: &reel_ui::frame::FrameGroup,
        parent: (Affine, [u32; 2], Scissor),
        region: Scissor,
    ) -> Result<bool, GpuError> {
        if !self.effect_admission.enabled()
            || self.capture
            || !current.drawing.transitions().is_empty()
            || !current.drawing.capture_exclusions().is_empty()
        {
            return Ok(false);
        }
        let Some(offset) = current.offset else {
            return Ok(false);
        };
        let Some(end) = current.commands[current.next..]
            .iter()
            .position(|command| matches!(command.op, FrameOp::PopGroup))
            .map(|end| current.next + end + 1)
        else {
            return Ok(false);
        };
        let effect = group.effect.as_ref().expect("forwarded effect");
        let effect_transform = parent.0 * group.effect_to_target;
        let output_scissor = current.scopes.last().scissor.intersect(parent.2);
        if output_scissor.empty()
            || effect_plan::context(effect, effect_transform, parent.1, self.size).is_none()
        {
            return Ok(false);
        }
        let (source_transform, effect_transform) =
            Grid::transforms(current.transform, effect_transform);
        let grid = Grid {
            source_transform,
            effect_transform,
            source_size: self.size,
            source_storage: self.offscreen.storage_size(self.size),
            source_scissor: [region.x, region.y, region.width, region.height],
            output_size: parent.1,
            output_storage: self.offscreen.storage_size(parent.1),
            scissor: [
                output_scissor.x,
                output_scissor.y,
                output_scissor.width,
                output_scissor.height,
            ],
            quality: [0, 1],
        };
        // Probe validates the complete source range. A nested group, clip,
        // additional draw or mask rejects this early lookup.
        let Some(probe) = Probe::new(
            current.drawing,
            offset + current.next - 1..offset + end,
            effect,
            grid,
        ) else {
            return Ok(false);
        };
        let Some(hit) = self.cached_effect(&probe)? else {
            return Ok(false);
        };
        let unused = std::mem::replace(&mut current.scopes.last_mut().target, hit);
        self.recycle_target(unused)?;
        current.next = end;
        current.transform = parent.0;
        current.size = parent.1;
        self.size = parent.1;
        self.scissor = output_scissor;
        self.mesh_clip = current.scopes.last().mesh_clip;
        Ok(true)
    }
}
