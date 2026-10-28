//! Finite source blends copy only the parent pixels their output can consume.
use super::*;
use crate::common::source_view::SourceView;

impl Context<'_> {
    /// Copy an initialized parent grid into a distinct scratch target. Partial
    /// writes retain transparent physical padding, including filter neighbours.
    pub(super) fn copy_backdrop(
        &mut self,
        source: &Target,
        destination: &Target,
        source_rect: Scissor,
        destination_rect: Scissor,
    ) -> bool {
        #[cfg(test)]
        if self
            .gpu
            .upload_creations
            .disable_backdrop_copy
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return false;
        }
        if source.texture == destination.texture
            || source.texture.format() != destination.texture.format()
            || source.multisample.is_some()
            || destination.multisample.is_some()
            || !source
                .texture
                .usage()
                .contains(wgpu::TextureUsages::COPY_SRC)
            || !destination
                .texture
                .usage()
                .contains(wgpu::TextureUsages::COPY_DST)
        {
            return false;
        }
        let destination_storage = destination.storage_size();
        let Some(region) = crate::common::backdrop_copy::Region::new(
            source.storage_size(),
            destination_storage,
            source_rect,
            destination_rect,
            self.quality.samples,
        ) else {
            return false;
        };
        self.ensure_sampled(&source.view);
        if region.overwrites_destination(destination_storage) {
            self.target_writes.take(destination.slot);
        } else {
            self.ensure_sampled(&destination.view);
        }
        let origin = |p: [u32; 2]| wgpu::Origin3d {
            x: p[0],
            y: p[1],
            z: 0,
        };
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::TextureCopy);
        self.encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &source.texture,
                mip_level: 0,
                origin: origin(region.source_origin),
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &destination.texture,
                mip_level: 0,
                origin: origin(region.destination_origin),
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: region.extent[0],
                height: region.extent[1],
                depth_or_array_layers: 1,
            },
        );
        #[cfg(test)]
        self.gpu
            .upload_creations
            .backdrop_copies
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        true
    }
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
        // Allocation capacity follows the common plan; the copy keeps the source
        // grid, preserving parent texels and MSAA sample positions.
        let size = self.size;
        self.size = self.offscreen.backdrop_size(view.storage_size, size);
        let mut backdrop = self.target()?;
        backdrop.size = view.storage_size;
        self.size = size;
        self.transient(super::super::pipeline::COMPOSITE_BYTES)?;
        if !self.copy_backdrop(target, &backdrop, view.parent_rect, view.source_rect) {
            self.transient(super::super::pipeline::COMPOSITE_BYTES)?;
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
        }
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
    #[ignore = "requires graphics adapter; synthetic numeric values only"]
    fn transfer_matches_composite_for_cropped_and_full_backdrops() {
        use std::sync::atomic::Ordering;
        let pool = f::pool();
        let size = [257, 129];
        for cropped in [true, false] {
            for stride in [4, 8, 40] {
                let mut scene = Scene::new();
                let mut fill = |rect, color| {
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
                    Rect::new(0., 0., 257., 129.),
                    Color::from_rgba8(45, 91, 177, 173),
                );
                fill(
                    Rect::new(39., 25., 191., 101.),
                    Color::from_rgba8(201, 49, 105, 89),
                );
                scene.push_group(Group {
                    composite: Composite {
                        alpha: 0.61,
                        blend: Mix::Multiply.into(),
                    },
                    ..Default::default()
                });
                let rect = if cropped {
                    Rect::new(37.5, 19.25, 193.5, 103.25)
                } else {
                    Rect::new(0., 0., 257., 129.)
                };
                scene.draw(Draw::Fill {
                    transform: Affine::IDENTITY,
                    fill_rule: Default::default(),
                    brush: Brush::Solid(Color::from_rgba8(221, 154, 39, 151)),
                    brush_transform: None,
                    shape: Geometry::Rect(rect),
                    composite: Default::default(),
                });
                scene.pop_group();
                let mut widget = f::RecordWidget::new(scene);
                widget.clip = false;
                let mut bridge = f::widget_bridge(widget, Affine::IDENTITY);
                let mut build = FrameBuild::new(
                    bridge.record(FrameId(1)).unwrap(),
                    f::empty_snapshot(&pool),
                    FrameInfo {
                        logical_size: [257., 129.],
                        physical_size: size,
                    },
                    f::limits(),
                    &pool,
                )
                .unwrap();
                while !build.step(f::step_budget()).unwrap().complete {}
                let frame = build.finish().unwrap();
                let mut outputs = Vec::new();
                for legacy in [true, false] {
                    let gpu = pollster::block_on(Gpu::request(
                        &wgpu::Instance::default(),
                        None,
                        crate::gpu::DeviceGeneration(1),
                        crate::gpu::GpuLimits {
                            target_bytes: u64::from(size[0]) * u64::from(size[1]) * 3 * stride,
                            ..Default::default()
                        },
                    ))
                    .unwrap();
                    gpu.upload_creations
                        .disable_backdrop_copy
                        .store(legacy, Ordering::Relaxed);
                    let mut renderer = crate::gpu::Renderer::new(gpu);
                    #[cfg(feature = "profiling")]
                    let before = reel_core::profile::Sample::snapshot();
                    let (prepared, passes, clears) =
                        super::super::super::targets::pass_trace::collect_counts(|| {
                            renderer.prepare(&frame).unwrap()
                        });
                    assert_eq!(prepared.quality().samples, if stride == 40 { 4 } else { 1 });
                    assert_eq!(
                        prepared.quality().format,
                        if stride == 4 {
                            wgpu::TextureFormat::Rgba8Unorm
                        } else {
                            wgpu::TextureFormat::Rgba16Float
                        }
                    );
                    let count = renderer
                        .gpu
                        .upload_creations
                        .backdrop_copies
                        .load(Ordering::Relaxed);
                    assert_eq!(count, usize::from(!legacy && stride != 40));
                    #[cfg(feature = "profiling")]
                    {
                        let profile = reel_core::profile::Sample::snapshot().delta(before);
                        assert_eq!(
                            profile.calls[reel_core::profile::Phase::TextureCopy as usize],
                            count as u64
                        );
                    }
                    let usage = renderer.usage();
                    let pixels = renderer.readback(prepared).unwrap();
                    outputs.push((pixels, usage.targets, usage.transient, passes, clears));
                }
                let [
                    (old, old_targets, old_transient, old_passes, old_clears),
                    (new, new_targets, new_transient, new_passes, new_clears),
                ] = outputs.try_into().unwrap();
                let max = old
                    .iter()
                    .zip(&new)
                    .flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| (x - y).abs()))
                    .fold(0f32, f32::max);
                assert!(max <= 0.005, "cropped={cropped} stride={stride} max={max}");
                assert_eq!(new_targets, old_targets);
                if stride == 40 {
                    assert_eq!(
                        (new_passes, new_clears, new_transient),
                        (old_passes, old_clears, old_transient)
                    );
                } else {
                    assert!(
                        new_transient < old_transient,
                        "{new_transient} vs {old_transient}"
                    );
                    if cropped {
                        assert_eq!((new_passes, new_clears), (old_passes, old_clears));
                    } else {
                        assert_eq!((new_passes + 1, new_clears + 1), (old_passes, old_clears));
                    }
                }
                eprintln!(
                    "passes={old_passes}->{new_passes} attachment_clear_requests={old_clears}->{new_clears}"
                );
                eprintln!(
                    "backdrop cropped={cropped} stride={stride} max={max} targets={old_targets} transient={old_transient}->{new_transient}"
                );
            }
        }
    }
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
