use super::*;

impl Context<'_> {
    pub(super) fn combine_group(
        &mut self,
        target: &Target,
        source: &Target,
        mask: &Target,
        paint: GroupPaint<'_>,
    ) -> Result<(), GpuError> {
        self.flush_draws()?;
        if self.scissor.empty() {
            return Ok(());
        }
        let GroupPaint {
            source_origin,
            composite,
            invert,
            operation,
            image_mask,
        } = paint;
        let blend = blend_code(composite.blend)
            .ok_or(GpuError::Device("unsupported group blend".into()))?;
        if blend == 0
            && invert == 0.
            && matches!(operation, CompositeOperation::SourceOver)
            && let Some((definition, transform)) = image_mask
            && self.composite_sample_mask(target, source, definition, transform, composite.alpha)?
        {
            return Ok(());
        }
        let expression = if let Some((definition, transform)) = image_mask {
            Some(self.mask_expression(definition, transform)?)
        } else {
            None
        };
        let backdrop = if blend != 0 {
            let backdrop = self.target()?;
            self.combine(
                &backdrop,
                target,
                target,
                1.,
                0.,
                CompositeOperation::SourceOver,
            )?;
            Some(backdrop)
        } else {
            None
        };
        self.composition(
            target,
            Composition {
                source,
                mask,
                backdrop: backdrop.as_ref().unwrap_or(source),
                alpha: composite.alpha,
                invert,
                operation,
                blend,
                expression: expression.as_ref(),
                source_origin,
            },
        )?;
        if let Some(backdrop) = backdrop {
            self.recycle_target(backdrop)?;
        }
        if let Some(expression) = expression {
            self.recycle_target(expression)?;
        }
        Ok(())
    }
    pub(super) fn combine_transition(
        &mut self,
        target: &Target,
        old: &Target,
        new: &Target,
        transition: &SceneTransition,
        outer: Affine,
    ) -> Result<(), GpuError> {
        let expression = if let Some(definition) = transition
            .mask
            .as_ref()
            .filter(|_| transition.progress > 0. && transition.progress < 1.)
        {
            Some(self.mask_expression(definition, outer * transition.mask_to_window)?)
        } else {
            None
        };
        let operation = if expression.is_some() {
            CompositeOperation::MaskedTransition(transition.progress)
        } else {
            CompositeOperation::Transition(transition.progress)
        };
        self.composition(
            target,
            Composition {
                source: old,
                mask: new,
                backdrop: old,
                alpha: 1.,
                invert: 0.,
                operation,
                blend: 0,
                expression: expression.as_ref(),
                source_origin: [0; 2],
            },
        )?;
        if let Some(expression) = expression {
            self.recycle_target(expression)?;
        }
        Ok(())
    }
}
