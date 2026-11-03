//! Execute author-selected effect density and reconstruct in the parent grid.
use super::*;
use crate::common::{effect_plan, effect_resolution as resolution};
use reel_ui::frame::FrameEffect;

impl Context<'_> {
    #[expect(
        clippy::too_many_arguments,
        reason = "same inputs as the full-density effect pass"
    )]
    pub(in crate::gpu) fn effect_draw(
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
        use crate::gpu::pipeline::{COMPOSITE_BYTES, COMPOSITE_WORDS, CompositeInputs};
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::SourceComposite);
        if self.scissor.empty() {
            return Ok(());
        }
        self.transient(COMPOSITE_BYTES)?;
        self.ensure_sampled(&low.view);
        let coverage = terminal.as_ref().and_then(|t| t.coverage);
        let mask = coverage.map_or(low, |(mask, _)| mask);
        if coverage.is_some() {
            self.ensure_sampled(&mask.view);
        }
        let (uv, bounds) = resolution::uv(low.size, low.storage_size());
        let mut words = [0; COMPOSITE_WORDS];
        words[0] = terminal.as_ref().map_or(1., |t| t.opacity).to_bits();
        words[1] = u32::from(coverage.is_some());
        words[5] = u32::from(
            coverage.is_some_and(|(_, nested)| nested)
                && self.quality.format != wgpu::TextureFormat::Rgba16Float,
        );
        words[6] = u32::from(matches!(
            mask.view.texture().format(),
            wgpu::TextureFormat::R8Unorm | wgpu::TextureFormat::R16Float
        ));
        words[8..12].copy_from_slice(&uv.map(f32::to_bits));
        words[12..16].copy_from_slice(
            &crate::common::source_view::uv_mapping(self.size, mask.storage_size(), [0; 2])
                .map(f32::to_bits),
        );
        words[16..20].copy_from_slice(&bounds.map(f32::to_bits));
        let group = self.pipelines.composite_group_words(
            &self.gpu.device,
            CompositeInputs {
                source: &low.view,
                mask: &mask.view,
                backdrop: &low.view,
            },
            &words,
        );
        let clear = self.target_writes.take(output.slot);
        let mut pass = self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("effect reconstruction"),
            color_attachments: &[Some(output.attachment(clear))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            multiview_mask: None,
            occlusion_query_set: None,
        });
        pass.set_viewport(0., 0., self.size[0] as f32, self.size[1] as f32, 0., 1.);
        pass.set_pipeline(&self.pipelines.effect_resample[usize::from(terminal.is_some())]);
        self.scissor.apply(&mut pass);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}
