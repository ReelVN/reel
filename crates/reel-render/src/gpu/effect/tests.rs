use crate::{
    gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer},
    test_support::r01_fixture,
};
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_ui::{
    core::PaintEffect,
    frame::{FrameBuild, FrameInfo},
    imaging::record::{Draw, Geometry, Scene},
    kurbo::{Affine, Rect},
    peniko::{Brush, Color},
    resources::{EffectShaderDescriptor, EffectShaderFormat, ResourceRegistry},
    stage::effect::*,
};

#[test]
#[ignore = "actual adapter submission and accounting; no capture or pixel readback"]
fn terminal_effect_native_submission_keeps_two_targets_and_both_quality_formats() {
    let pool = r01_fixture::pool();
    let program = EffectProgram::new(
        vec![],
        vec![
            EffectNode {
                ty: EffectType::Vec2,
                op: EffectOp::Input(EffectInput::Uv),
            },
            EffectNode {
                ty: EffectType::Vec4,
                op: EffectOp::SourceSample {
                    uv: EffectNodeId(0),
                },
            },
        ],
        EffectNodeId(1),
    )
    .unwrap();
    let fragment =
        crate::effect::compile_fragment(&program, crate::effect::Language::Wgsl, 16384).unwrap();
    let descriptor = EffectShaderDescriptor::with_textures(
        [4; 32],
        EffectShaderFormat::Wgsl,
        vec![],
        0,
        true,
        EffectType::Vec4,
        fragment.source.as_bytes(),
    )
    .unwrap();
    let mut registry = ResourceRegistry::new(1, 16384).unwrap();
    let handle = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey("terminal-contract".into()),
                version: ContentVersion(descriptor.shader_key),
            },
            descriptor,
            OwnedBytes::new(
                fragment.source.clone().into_bytes(),
                pool.reserve(fragment.source.len() as u64).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let bounds = Rect::new(0., 0., 160., 120.);
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::WHITE),
        brush_transform: None,
        shape: Geometry::Rect(bounds),
        composite: Default::default(),
    });
    let mut widget = r01_fixture::RecordWidget::new(scene);
    widget.clip = false;
    widget.opacity = 0.37;
    widget.effect = Some(PaintEffect {
        instance: EffectInstance::new([4; 32], program, vec![]).unwrap(),
        shader: registry.acquire(handle).unwrap(),
        images: vec![],
        bounds,
        coverage_invert: false,
    });
    let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[handle], &pool).unwrap(),
        FrameInfo {
            logical_size: [160., 120.],
            physical_size: [160, 120],
        },
        r01_fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(r01_fixture::step_budget()).unwrap().complete {}
    let frame = build.finish().unwrap();
    for (format, stride) in [
        (wgpu::TextureFormat::Rgba16Float, 8),
        (wgpu::TextureFormat::Rgba8Unorm, 4),
    ] {
        let bytes = 2 * 160 * 120 * stride;
        let gpu = pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            DeviceGeneration(1),
            GpuLimits {
                target_bytes: bytes,
                ..Default::default()
            },
        ))
        .unwrap();
        eprintln!(
            "terminal adapter={:?} f16={}",
            gpu.adapter.get_info(),
            gpu.device.features().contains(wgpu::Features::SHADER_F16)
        );
        let mut renderer = Renderer::new(gpu);
        renderer.prepare_pipelines().unwrap();
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
        eprintln!("terminal format={format:?} target_bytes={bytes} completed={submission}");
    }
}

fn coverage_frame(variant: bool) -> reel_ui::frame::RenderFrame {
    let pool = r01_fixture::pool();
    let program = EffectProgram::new(
        vec![],
        vec![
            EffectNode {
                ty: EffectType::Vec2,
                op: EffectOp::Input(EffectInput::Uv),
            },
            EffectNode {
                ty: EffectType::Vec4,
                op: EffectOp::SourceSample {
                    uv: EffectNodeId(0),
                },
            },
        ],
        EffectNodeId(1),
    )
    .unwrap();
    let fragment =
        crate::effect::compile_fragment(&program, crate::effect::Language::Wgsl, 16384).unwrap();
    let mut code = fragment.source.as_bytes().to_vec();
    let offset = code.len() as u32;
    if variant {
        code.extend_from_slice(
            crate::effect::compile_fragment_with_tail(
                &program,
                crate::effect::Language::Wgsl,
                crate::effect::OutputTail::AlphaCoverage,
                16384,
            )
            .unwrap()
            .source
            .as_bytes(),
        );
    }
    let descriptor = EffectShaderDescriptor::with_textures(
        [4; 32],
        EffectShaderFormat::Wgsl,
        vec![],
        0,
        true,
        EffectType::Vec4,
        &code,
    )
    .unwrap();
    let descriptor = if variant {
        descriptor.with_coverage_variant(offset, &code).unwrap()
    } else {
        descriptor
    };
    let mut registry = ResourceRegistry::new(1, 32768).unwrap();
    let handle = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey("terminal-contract".into()),
                version: ContentVersion(descriptor.shader_key),
            },
            descriptor,
            OwnedBytes::new(code.clone(), pool.reserve(code.len() as u64).unwrap()).unwrap(),
        )
        .unwrap();
    let bounds = Rect::new(0., 0., 160., 120.);
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::WHITE),
        brush_transform: None,
        shape: Geometry::Rect(bounds),
        composite: Default::default(),
    });
    let mut widget = r01_fixture::RecordWidget::new(scene);
    widget.clip = true;
    widget.inner.clip = bounds;
    widget.opacity = 0.37;
    widget.effect = Some(PaintEffect {
        instance: EffectInstance::new([4; 32], program, vec![]).unwrap(),
        shader: registry.acquire(handle).unwrap(),
        images: vec![],
        bounds: Rect::new(0., 0., 64., 48.),
        coverage_invert: false,
    });
    let mut bridge = r01_fixture::widget_bridge(widget, Affine::rotate(0.17));
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[handle], &pool).unwrap(),
        FrameInfo {
            logical_size: [160., 120.],
            physical_size: [160, 120],
        },
        r01_fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(r01_fixture::step_budget()).unwrap().complete {}
    build.finish().unwrap()
}

#[test]
fn coverage_plan_retains_source_and_preserves_mask_grid() {
    for variant in [false, true] {
        let frame = coverage_frame(variant);
        let effect_group = frame
            .commands()
            .iter()
            .find_map(|c| {
                if let reel_ui::frame::FrameOp::PushGroup(g) = &c.op {
                    g.effect.as_ref().map(|_| g)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(effect_group.clip.is_none());
        assert_eq!(
            crate::common::effect_plan::clipped_effect(
                frame.drawing(),
                frame.commands(),
                1,
                1,
                false
            )
            .is_some(),
            variant
        );
        for (samples, boundary) in [(4, false), (1, true)] {
            assert!(
                crate::common::effect_plan::clipped_effect(
                    frame.drawing(),
                    frame.commands(),
                    1,
                    samples,
                    boundary
                )
                .is_none()
            );
        }
        let mut same_group = effect_group.clone();
        if let reel_ui::frame::FrameOp::PushClip(clip) = &frame.commands()[0].op {
            same_group.clip = Some(clip.clone());
        }
        assert_eq!(
            crate::common::effect_plan::terminal_tail(frame.drawing(), &same_group, 1, false),
            variant.then_some(crate::effect::OutputTail::AlphaCoverage)
        );
        let mut siblings = frame.commands().to_vec();
        let draw = siblings
            .iter()
            .find(|c| matches!(c.op, reel_ui::frame::FrameOp::Draw(_)))
            .unwrap()
            .clone();
        siblings.insert(siblings.len() - 1, draw);
        assert!(
            crate::common::effect_plan::clipped_effect(frame.drawing(), &siblings, 1, 1, false)
                .is_none()
        );
        let pixels = crate::common::target_plan::target_pixels(
            &frame,
            None,
            [160, 120],
            false,
            1,
            4096,
            &Default::default(),
            &Default::default(),
            crate::common::target_plan::MaskExecution::Inline,
            &Default::default(),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(pixels, if variant { 41472 } else { 60672 });
        eprintln!("coverage variant={variant}: planned pixels={pixels}");
    }
    assert!(!crate::common::effect_plan::coverage_retention(
        [1, 161],
        [160, 2]
    ));
    assert!(crate::common::effect_plan::coverage_retention(
        [160, 120],
        [160, 120]
    ));
}
#[test]
#[ignore = "actual coverage-tail submission; no readback"]
fn independent_coverage_native_submission_keeps_three_targets() {
    for variant in [false, true] {
        let frame = coverage_frame(variant);
        for (format, stride) in [
            (wgpu::TextureFormat::Rgba16Float, 8),
            (wgpu::TextureFormat::Rgba8Unorm, 4),
        ] {
            let bytes = (if variant { 41472 } else { 60672 }) * stride;
            let gpu = pollster::block_on(Gpu::request(
                &wgpu::Instance::default(),
                None,
                DeviceGeneration(1),
                GpuLimits {
                    target_bytes: bytes,
                    ..Default::default()
                },
            ))
            .unwrap();
            let mut renderer = Renderer::new(gpu);
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
                "coverage-tail variant={variant} {format:?}: target_bytes={bytes}, completed={submission}"
            );
        }
    }
}
