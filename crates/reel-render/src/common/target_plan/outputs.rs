//! Physical output windows share their frozen logical grid and lifetime walk.
use super::*;
use crate::common::{effect_output, source_crop::Crop};
use reel_ui::stage::effect::EffectType;

impl Input<'_> {
    pub(super) fn output_crop(
        &self,
        current: &Stream<'_>,
        scope: &Scope<'_>,
        scratch: &mut Scratch,
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Option<Crop>, GpuError> {
        let Some(push) = scope.push else {
            return Ok(None);
        };
        let mut crop = self.schedule.output_crop(push);
        if self.schedule.collect_output_crops()
            && scope.forward_source
            && !scope.macro_source
            && !scope.shared_source
            && !scope.source_view
            && self.samples == 1
            && !self.capture
            && self.second.is_none()
            && current.offset == Some(0)
            && std::ptr::eq(current.drawing, self.frame.drawing())
            && current.retained_context == 0
            && current.drawing.transitions().is_empty()
            && current.drawing.masks().is_empty()
            && current.drawing.capture_exclusions().is_empty()
            && current
                .scopes
                .last()
                .and_then(|parent| parent.group)
                .is_some_and(inputs::ordinary)
            && let Some(group) = scope.group.filter(|group| inputs::ordinary(group))
            && let Some(effect) = &group.effect
            && effect.resolution_scale == 1.
            && super::super::effect_plan::shader(
                current.drawing,
                effect,
                current.commands[push].origin.at,
            )
            .is_ok_and(|shader| shader.descriptor.output == EffectType::Vec4)
        {
            crop = Crop::physical_window(
                current.size,
                self.schedule.storage_size(current.size),
                current.scissor,
            )
            .filter(|crop| self.schedule.storage_size(crop.size) == crop.size);
            if let Some(crop) = crop {
                super::super::scopes::push_stack(
                    &mut scratch.output_crops,
                    effect_output::Entry { push, crop },
                    &mut *reserve,
                )?;
            }
        }
        if let Some(crop) = crop {
            scratch.cropped = true;
            if !scratch.exact.contains(&crop.size) {
                super::super::scopes::push_stack(&mut scratch.exact, crop.size, reserve)?;
            }
        }
        Ok(crop)
    }
}
