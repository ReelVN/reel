//! Execute author-selected effect density and reconstruct in the parent grid.
use super::*;
use crate::common::{effect_plan, effect_resolution as resolution};
use reel_ui::frame::FrameEffect;

impl Context<'_> {
    #[expect(
        clippy::too_many_arguments,
        reason = "same inputs as the full-density effect pass"
    )]
    pub(in crate::gxm) fn effect_draw(
        &mut self,
        drawing: &FrameDrawing,
        effect: &FrameEffect,
        transform: Affine,
        source: effect_plan::Source<'_, Target>,
        source_view: Option<Affine>,
        terminal: Option<effect_plan::Terminal<'_, Target>>,
        output_crop: Option<crate::common::source_crop::Crop>,
        at: RecordLocation,
    ) -> Result<Target, GpuError> {
        let reduced = resolution::size(self.size, effect.resolution_scale);
        if reduced == self.size {
            return self.effect_draw_full(
                drawing,
                effect,
                transform,
                source,
                source_view,
                terminal,
                output_crop,
                at,
            );
        }
        self.flush_draws()?;
        let original = self.size;
        let clip = self.scissor;
        let scale = resolution::transform(original, reduced);
        self.size = reduced;
        self.scissor = resolution::scissor(clip, original, reduced);
        let low = self.effect_draw_full(
            drawing,
            effect,
            scale * transform,
            source,
            source_view.map(|view| view * scale.inverse()),
            None,
            None,
            at,
        );
        self.size = original;
        self.scissor = clip;
        let low = low?;
        let output = if let Some(terminal) = &terminal {
            terminal.target.clone()
        } else {
            self.target()?
        };
        self.resample_effect(&output, &low, terminal)?;
        self.recycle_target(low)?;
        Ok(output)
    }
}

impl Context<'_> {
    fn resample_effect(
        &mut self,
        output: &Target,
        low: &Target,
        terminal: Option<effect_plan::Terminal<'_, Target>>,
    ) -> Result<(), GpuError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::SourceComposite);
        if self.scissor.empty() {
            return Ok(());
        }
        self.ensure_sampled(&low.view)?;
        let coverage = terminal.as_ref().and_then(|t| t.coverage);
        let mask = coverage.map_or(low, |(mask, _)| mask);
        if coverage.is_some() {
            self.ensure_sampled(&mask.view)?;
        }
        self.initialize_target(output, None)?;
        let shader = &self.programs.effect_resample;
        let (uv, bounds) = resolution::uv(low.size, low.view.size());
        let mut values = Values::new();
        values.extend(shader.int("u_source", 0));
        values.extend(shader.int("u_mask", 1));
        values.extend(shader.vec4("u_source_uv", uv));
        values.extend(shader.vec4("u_backdrop_uv", bounds));
        values.extend(shader.vec4("u_mask_uv", mask.frame_uv(self.size)));
        values.extend(shader.vec4(
            "u_params",
            [
                terminal.as_ref().map_or(1., |t| t.opacity),
                f32::from(coverage.is_some()),
                f32::from(coverage.is_some_and(|(_, nested)| nested)),
                0.,
            ],
        ));
        self.programs.quad(
            self.gpu,
            programs::Quad {
                target: native::Target::Framebuffer(&output.framebuffer),
                size: self.size,
                shader,
                textures: &[&low.view, &mask.view],
                values: &values,
                blend: if terminal.is_some() {
                    native::Blend::Premultiplied
                } else {
                    native::Blend::Replace
                },
                scissor: self.native_scissor(),
            },
        )
    }
}
