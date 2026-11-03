//! Draw retained transition inputs in their original composition domain.
use super::*;

impl Context<'_> {
    pub(super) fn combine_transition(
        &mut self,
        target: &Target,
        old: &wgpu::TextureView,
        new: &wgpu::TextureView,
        transition: &SceneTransition,
        outer: Affine,
    ) -> Result<(), GpuError> {
        self.flush_draws()?;
        if self.scissor.empty() {
            return Ok(());
        }
        let Some(definition) = transition
            .mask
            .as_ref()
            .filter(|_| transition.progress > 0. && transition.progress < 1.)
        else {
            return self.combine(
                target,
                old,
                new,
                1.,
                0.,
                CompositeOperation::Transition(transition.progress),
            );
        };
        let transform = outer * transition.mask_to_window;
        if definition.bounds.width() <= 0.
            || definition.bounds.height() <= 0.
            || transform.determinant() == 0.
        {
            return self.combine(target, old, new, 1., 0., CompositeOperation::Transition(0.));
        }
        let uv = Affine::scale_non_uniform(
            1. / definition.bounds.width(),
            1. / definition.bounds.height(),
        ) * Affine::translate((-definition.bounds.x0, -definition.bounds.y0))
            * transform.inverse();
        let pipeline = self.pipelines.masks.get(
            self.gpu,
            self.quality,
            &definition.program,
            definition.parameters,
            &self.budgets.resources,
        )?;
        self.transient(
            pipeline.parameter_cost(definition.images.len())
                + ((8 + definition.images.len()) * size_of::<wgpu::BindGroupEntry>()
                    + definition.images.len() * size_of::<&wgpu::TextureView>())
                    as u64,
        )?;
        self.ensure_sampled(old);
        self.ensure_sampled(new);
        let images = definition
            .images
            .iter()
            .map(|handle| self.images[handle].as_ref())
            .collect();
        pipeline.draw(
            &self.gpu.device,
            &mut self.encoder,
            target,
            super::super::mask::Inputs {
                source: old,
                mask: new,
                backdrop: old,
                images,
            },
            super::super::pipeline::CompositeParams {
                clear: self.target_writes.take(target.slot),
                scissor: self.scissor,
                opacity: 1.,
                invert: 0.,
                operation: CompositeOperation::MaskedTransition(transition.progress),
                blend: 0,
                source_origin: [0; 2],
                backdrop_origin: [0; 2],
            },
            definition,
            uv,
            &self.pipelines.samplers,
        );
        Ok(())
    }
}
