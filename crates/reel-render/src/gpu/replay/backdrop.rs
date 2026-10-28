//! Finite source blends copy only the parent pixels their output can consume.
use super::*;
use crate::common::source_view::SourceView;

impl Context<'_> {
    pub(super) fn combine_cropped(
        &mut self,
        target: &Target,
        source: &Target,
        view: SourceView,
        composite: reel_ui::frame::GroupComposite,
        invert: f32,
    ) -> Result<(), GpuError> {
        self.flush_draws()?;
        if self.scissor.empty() {
            return Ok(());
        }
        // Reuse the common source grid, including its admitted capacity class.
        // The integer copy offset preserves parent texels and MSAA sample positions.
        let size = self.size;
        self.size = view.storage_size;
        let backdrop = self.target()?;
        self.size = size;
        self.transient(super::super::pipeline::COMPOSITE_BYTES * 2)?;
        self.ensure_sampled(&target.view);
        self.pipelines.composite(
            &self.gpu.device,
            &mut self.encoder,
            &backdrop,
            super::super::pipeline::CompositeInputs {
                source: &target.view,
                mask: &target.view,
                backdrop: &target.view,
            },
            super::super::pipeline::CompositeParams {
                clear: self.target_writes.take(backdrop.slot),
                scissor: view.source_rect,
                opacity: 1.,
                invert: 0.,
                operation: CompositeOperation::SourceOver,
                blend: 0,
                source_origin: view.origin().map(|coordinate| -coordinate),
                backdrop_origin: [0; 2],
            },
        );
        self.ensure_sampled(&source.view);
        self.pipelines.composite(
            &self.gpu.device,
            &mut self.encoder,
            target,
            super::super::pipeline::CompositeInputs {
                source: &source.view,
                mask: &source.view,
                backdrop: &backdrop.view,
            },
            super::super::pipeline::CompositeParams {
                clear: self.target_writes.take(target.slot),
                scissor: self.scissor,
                opacity: composite.alpha,
                invert,
                operation: CompositeOperation::SourceOver,
                blend: blend_code(composite.blend).expect("validated group blend"),
                source_origin: view.origin(),
                backdrop_origin: view.origin(),
            },
        );
        self.recycle_target(backdrop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::r01_fixture as f;
    use reel_core::identity::FrameId;
    use reel_ui::{
        frame::{FrameBuild, FrameInfo},
        imaging::{
            Composite,
            record::{Draw, Geometry, Group, Scene},
        },
        kurbo::Rect,
        peniko::{Brush, Color, Mix},
    };

    #[test]
    #[ignore = "actual adapter submission and accounting; no capture or pixel readback"]
    fn cropped_backdrops_submit_translucent_blends_in_bounded_storage() {
        let pool = f::pool();
        for (format, stride) in [
            (wgpu::TextureFormat::Rgba16Float, 8),
            (wgpu::TextureFormat::Rgba8Unorm, 4),
        ] {
            let bytes = (960 * 544 + 2 * 160 * 64) * stride;
            let gpu = pollster::block_on(Gpu::request(
                &wgpu::Instance::default(),
                None,
                crate::gpu::DeviceGeneration(1),
                crate::gpu::GpuLimits {
                    target_bytes: bytes,
                    ..Default::default()
                },
            ))
            .unwrap();
            let mut renderer = crate::gpu::Renderer::new(gpu);
            renderer.prepare_pipelines().unwrap();
            for mix in [
                Mix::Multiply,
                Mix::Screen,
                Mix::Overlay,
                Mix::ColorDodge,
                Mix::SoftLight,
            ] {
                let mut scene = Scene::new();
                let fill = |scene: &mut Scene, rect, color| {
                    scene.draw(Draw::Fill {
                        transform: Affine::IDENTITY,
                        fill_rule: Default::default(),
                        brush: Brush::Solid(color),
                        brush_transform: None,
                        shape: Geometry::Rect(rect),
                        composite: Default::default(),
                    })
                };
                fill(
                    &mut scene,
                    Rect::new(0., 0., 960., 544.),
                    Color::from_rgba8(64, 128, 192, 170),
                );
                scene.push_group(Group {
                    composite: Composite {
                        alpha: 0.5,
                        blend: mix.into(),
                    },
                    ..Default::default()
                });
                fill(
                    &mut scene,
                    Rect::new(20., 30., 120., 70.),
                    Color::from_rgba8(240, 80, 120, 96),
                );
                fill(
                    &mut scene,
                    Rect::new(60., 30., 160., 70.),
                    Color::from_rgba8(120, 240, 80, 192),
                );
                scene.pop_group();
                let mut widget = f::RecordWidget::new(scene);
                widget.clip = false;
                let mut bridge = f::widget_bridge(widget, Affine::IDENTITY);
                let mut build = FrameBuild::new(
                    bridge.record(FrameId(1)).unwrap(),
                    f::empty_snapshot(&pool),
                    FrameInfo {
                        logical_size: [960., 544.],
                        physical_size: [960, 544],
                    },
                    f::limits(),
                    &pool,
                )
                .unwrap();
                while !build.step(f::step_budget()).unwrap().complete {}
                let frame = build.finish().unwrap();
                let prepared = renderer.prepare(&frame).unwrap();
                assert_eq!(prepared.quality().format, format);
                assert_eq!(prepared.quality().samples, 1);
                assert_eq!(renderer.usage().targets, bytes);
                let submission = renderer.submit(prepared).unwrap();
                renderer
                    .gpu()
                    .device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                assert!(renderer.poll().unwrap().contains(&submission));
                eprintln!(
                    "cropped_backdrop blend={mix:?} format={format:?} targets={bytes} old_targets={} completed={submission}",
                    (2 * 960 * 544 + 160 * 64) * stride
                );
            }
        }
    }
}
