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
fn terminal_effect_submission_preserves_visible_sources_and_culls_invisible_outputs() {
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
    for transform in [Affine::IDENTITY, Affine::translate((2000., 0.))] {
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
            instance: EffectInstance::new([4; 32], program.clone(), vec![]).unwrap(),
            shader: registry.acquire(handle).unwrap(),
            images: vec![],
            bounds,
            coverage_invert: false,
        });
        let mut bridge = r01_fixture::widget_bridge(widget, transform);
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
            let targets = if transform == Affine::IDENTITY { 2 } else { 1 };
            let bytes = targets * 160 * 120 * stride;
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
            eprintln!(
                "terminal transform={transform:?} format={format:?} target_bytes={bytes} completed={submission}"
            );
        }
    }
}

fn coverage_frame(variant: bool) -> reel_ui::frame::RenderFrame {
    coverage_frame_variant(variant, false, false)
}
fn coverage_frame_variant(
    variant: bool,
    full: bool,
    materialized: bool,
) -> reel_ui::frame::RenderFrame {
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
    // The full variant fixture rejects an accidental ordinary-program selection.
    let mut code = if full {
        b"full-target variant required".to_vec()
    } else {
        fragment.source.as_bytes().to_vec()
    };
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
    let full_offset = full.then(|| {
        let offset = code.len() as u32;
        code.extend_from_slice(
            crate::effect::compile_fragment_variant(
                &program,
                crate::effect::Language::Wgsl,
                crate::effect::OutputTail::Ordinary,
                crate::effect::FragmentCoverage::FullTarget,
                16384,
            )
            .unwrap()
            .source
            .as_bytes(),
        );
        offset
    });
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
    let descriptor = if let Some(offset) = full_offset {
        descriptor.with_full_target_variant(offset, &code).unwrap()
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
    let height = if materialized { 128. } else { 120. };
    let bounds = Rect::new(0., 0., 160., height);
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
    widget.clip = !materialized;
    widget.invert = if materialized { 0.1 } else { 0. };
    widget.inner.clip = bounds;
    widget.opacity = 0.37;
    widget.effect = Some(PaintEffect {
        instance: EffectInstance::new([4; 32], program, vec![]).unwrap(),
        shader: registry.acquire(handle).unwrap(),
        images: vec![],
        bounds: if materialized {
            bounds
        } else {
            Rect::new(0., 0., 64., 48.)
        },
        coverage_invert: false,
    });
    let mut bridge = r01_fixture::widget_bridge(
        widget,
        if materialized {
            Affine::translate((80., 64.)) * Affine::rotate(0.5) * Affine::translate((-80., -64.))
        } else {
            Affine::rotate(0.17)
        },
    );
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[handle], &pool).unwrap(),
        FrameInfo {
            logical_size: [160., height],
            physical_size: [160, height as u32],
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

#[test]
#[ignore = "actual adapter effect cache submission; no readback"]
fn static_image_effect_chain_reuses_materialized_passes() {
    use reel_ui::{
        core::Widget,
        imaging::Composite,
        kurbo::{Point, Size},
        peniko::{ImageAlphaType, ImageBrush, ImageFormat},
        resources::ColorEncoding,
    };
    let pool = r01_fixture::pool();
    let program = EffectProgram::with_textures(
        vec![EffectParameter {
            name: "tint".into(),
            ty: EffectType::Vec4,
            default: None,
        }],
        vec![EffectTextureParameter { name: "ink".into() }],
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
            EffectNode {
                ty: EffectType::Vec4,
                op: EffectOp::TextureSample {
                    binding: 0,
                    uv: EffectNodeId(0),
                    channel: EffectTextureChannel::Color,
                },
            },
            EffectNode {
                ty: EffectType::Vec4,
                op: EffectOp::Parameter(0),
            },
            EffectNode {
                ty: EffectType::Vec4,
                op: EffectOp::Binary {
                    op: EffectBinary::Multiply,
                    left: EffectNodeId(1),
                    right: EffectNodeId(2),
                },
            },
            EffectNode {
                ty: EffectType::Vec4,
                op: EffectOp::Binary {
                    op: EffectBinary::Multiply,
                    left: EffectNodeId(4),
                    right: EffectNodeId(3),
                },
            },
        ],
        EffectNodeId(5),
    )
    .unwrap();
    let fragment =
        crate::effect::compile_fragment(&program, crate::effect::Language::Wgsl, 16384).unwrap();
    let descriptor = EffectShaderDescriptor::with_textures(
        [4; 32],
        EffectShaderFormat::Wgsl,
        vec![EffectType::Vec4],
        1,
        true,
        EffectType::Vec4,
        fragment.source.as_bytes(),
    )
    .unwrap();
    let mut registry = ResourceRegistry::new(8, 16384).unwrap();
    let shader = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey("effect-chain".into()),
                version: ContentVersion(descriptor.shader_key),
            },
            descriptor,
            OwnedBytes::new(
                fragment.source.as_bytes().to_vec(),
                pool.reserve(fragment.source.len() as u64).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let image = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("static-image".into()),
                version: ContentVersion([3; 32]),
            },
            OwnedBytes::new(vec![255; 4], pool.reserve(4).unwrap()).unwrap(),
            [1, 1],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let image_lease = registry.acquire(image).unwrap();
    let pixels = crate::test_support::image_data(&image_lease);
    let bounds = Rect::new(0., 0., 160., 120.);
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Image(ImageBrush::new(pixels.clone())),
        brush_transform: None,
        shape: Geometry::Rect(bounds),
        composite: Composite::default(),
    });
    let paint = PaintEffect {
        instance: EffectInstance::with_textures(
            [4; 32],
            program,
            vec![EffectValue::Vec4([1f32.to_bits(); 4])],
            vec!["static-image".into()],
        )
        .unwrap(),
        shader: registry.acquire(shader).unwrap(),
        images: vec![image_lease.clone()],
        bounds,
        coverage_invert: false,
    };
    let mut widget = r01_fixture::RecordWidget::new(scene);
    widget.clip = false;
    widget.effect = Some(paint.clone());
    for _ in 0..2 {
        let mut parent = r01_fixture::RecordWidget::new(Scene::new());
        parent.clip = false;
        parent.effect = Some(paint.clone());
        parent
            .inner
            .add(widget.prepare(), Point::ZERO, Size::new(160., 120.));
        widget = parent;
    }
    let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[shader, image], &pool).unwrap(),
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
    let mut changed_paint = paint.clone();
    changed_paint.instance = EffectInstance::with_textures(
        [4; 32],
        paint.instance.program().clone(),
        vec![EffectValue::Vec4([0.5f32.to_bits(); 4])],
        vec!["static-image".into()],
    )
    .unwrap();
    let mut changed_scene = Scene::new();
    changed_scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Image(ImageBrush::new(pixels.clone())),
        brush_transform: None,
        shape: Geometry::Rect(bounds),
        composite: Composite::default(),
    });
    let mut changed_widget = r01_fixture::RecordWidget::new(changed_scene);
    changed_widget.clip = false;
    changed_widget.effect = Some(changed_paint.clone());
    for _ in 0..2 {
        let mut parent = r01_fixture::RecordWidget::new(Scene::new());
        parent.clip = false;
        parent.effect = Some(changed_paint.clone());
        parent
            .inner
            .add(changed_widget.prepare(), Point::ZERO, Size::new(160., 120.));
        changed_widget = parent;
    }
    let mut changed_bridge = r01_fixture::widget_bridge(changed_widget, Affine::IDENTITY);
    let mut changed_build = FrameBuild::new(
        changed_bridge.record(FrameId(2)).unwrap(),
        registry.freeze(&[shader, image], &pool).unwrap(),
        FrameInfo {
            logical_size: [160., 120.],
            physical_size: [160, 120],
        },
        r01_fixture::limits(),
        &pool,
    )
    .unwrap();
    while !changed_build
        .step(r01_fixture::step_budget())
        .unwrap()
        .complete
    {}
    let changed_frame = changed_build.finish().unwrap();
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits::default(),
    ))
    .unwrap();
    let mut renderer = Renderer::new(gpu);
    for _ in 0..2 {
        let prepared = loop {
            match renderer.prepare(&frame) {
                Ok(prepared) => break prepared,
                Err(crate::gpu::GpuError::Preparing) => {
                    renderer
                        .gpu()
                        .device
                        .poll(wgpu::PollType::wait_indefinitely())
                        .unwrap();
                    renderer.poll().unwrap();
                }
                Err(error) => panic!("effect preparation: {error:?}"),
            }
        };
        renderer.submit(prepared).unwrap();
        renderer
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
    }
    let (entries, hits) = renderer.effect_results.test_stats();
    eprintln!(
        "effect chain entries={entries} image_hits={} intermediate_hits={} target_bytes={}",
        hits[0],
        hits[1],
        renderer.usage().targets
    );
    assert!(entries >= 2, "two materialized passes must be retained");
    assert!(hits[0] >= 1, "the immutable image pass must hit");
    assert!(hits[1] >= 1, "the intermediate effect output must hit");

    use std::{sync::atomic::Ordering, time::Instant};
    let mut measurements = Vec::new();
    for baseline in [true, false] {
        renderer.effect_uniforms.disable_for_test(baseline);
        let snapshot = |renderer: &Renderer| {
            let counts = &renderer.gpu().upload_creations;
            (
                counts.effect_uniform_buffers.load(Ordering::Relaxed),
                counts.effect_bind_groups.load(Ordering::Relaxed),
                counts.effect_uniform_bytes.load(Ordering::Relaxed),
            )
        };
        let before = snapshot(&renderer);
        let mut prepare_ns = 0;
        for _ in 0..16 {
            // Compare uniform paths on identical effect work, independently of R2.
            renderer.effect_results.clear();
            let start = Instant::now();
            let prepared = renderer.prepare(&frame).unwrap();
            prepare_ns += start.elapsed().as_nanos();
            renderer.submit(prepared).unwrap();
            renderer
                .gpu()
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            renderer.poll().unwrap();
        }
        let after = snapshot(&renderer);
        let measured = (
            after.0 - before.0,
            after.1 - before.1,
            after.2 - before.2,
            prepare_ns,
        );
        eprintln!(
            "effect_uniform baseline={baseline} frames=16 buffers={} groups={} uploaded_bytes={} prepare_ns={}",
            measured.0, measured.1, measured.2, measured.3
        );
        measurements.push(measured);
    }
    assert!(measurements[1].0 < measurements[0].0);
    assert!(measurements[1].1 < measurements[0].1);
    assert!(measurements[1].2 < measurements[0].2);

    let before_dynamic = renderer
        .gpu()
        .upload_creations
        .effect_uniform_bytes
        .load(Ordering::Relaxed);
    renderer.effect_results.clear();
    let changed = renderer.prepare(&changed_frame).unwrap();
    renderer.submit(changed).unwrap();
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    let changed_bytes = renderer
        .gpu()
        .upload_creations
        .effect_uniform_bytes
        .load(Ordering::Relaxed)
        - before_dynamic;
    eprintln!("effect_uniform changed_parameter_uploaded_bytes={changed_bytes}");
    assert_eq!(
        changed_bytes,
        3 * 16,
        "only the three parameter blocks changed"
    );
    renderer.effect_results.clear();
    let same = renderer.prepare(&changed_frame).unwrap();
    renderer.submit(same).unwrap();
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    assert_eq!(
        renderer
            .gpu()
            .upload_creations
            .effect_uniform_bytes
            .load(Ordering::Relaxed)
            - before_dynamic,
        changed_bytes
    );

    renderer.effect_results.clear();
    let before_upload = renderer
        .gpu()
        .upload_creations
        .effect_uniform_bytes
        .load(Ordering::Relaxed);
    let cancelled = renderer.prepare(&frame).unwrap();
    assert_eq!(
        renderer
            .gpu()
            .upload_creations
            .effect_uniform_bytes
            .load(Ordering::Relaxed),
        before_upload
    );
    drop(cancelled);
    assert_eq!(
        renderer
            .gpu()
            .upload_creations
            .effect_uniform_bytes
            .load(Ordering::Relaxed),
        before_upload
    );
    renderer.effect_results.clear();
    let first = renderer.prepare(&frame).unwrap();
    renderer.effect_results.clear();
    let second = renderer.prepare(&frame).unwrap();
    renderer.submit(second).unwrap();
    renderer.submit(first).unwrap();
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();

    // A cache invalidation while a candidate is held must prevent old slots
    // from returning after that candidate completes.
    renderer.effect_results.clear();
    renderer.effect_uniforms.clear();
    let old_generation = renderer.prepare(&frame).unwrap();
    renderer.effect_uniforms.clear();
    renderer.submit(old_generation).unwrap();
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    assert_eq!(renderer.effect_uniforms.test_len(), 0);

    renderer.effect_results.clear();
    let warm = renderer.prepare(&frame).unwrap();
    renderer.submit(warm).unwrap();
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    let slots = renderer.effect_uniforms.test_len();
    assert!(slots > 0);
    let resident = renderer.usage().transient;
    for _ in 0..usize::from(crate::common::target_cache::MAX_IDLE_SUBMISSIONS) + slots {
        renderer.effect_uniforms.submitted();
    }
    assert_eq!(renderer.effect_uniforms.test_len(), 0);
    assert!(
        renderer.usage().transient < resident,
        "cold slots release their budget"
    );
}

#[test]
#[ignore = "requires graphics adapter; synthetic numeric domain comparison"]
fn full_target_effect_keeps_local_domain_and_initializes_unwritten_pixels() {
    let make_renderer = || {
        Renderer::new(
            pollster::block_on(Gpu::request(
                &wgpu::Instance::default(),
                None,
                DeviceGeneration(1),
                Default::default(),
            ))
            .unwrap(),
        )
    };
    let mut original = make_renderer();
    let baseline = original
        .prepare(&coverage_frame_variant(false, false, true))
        .unwrap();
    let expected = original.readback(baseline).unwrap();
    let mut optimized = make_renderer();
    let frame = coverage_frame_variant(false, true, true);
    let candidate = optimized.prepare(&frame).unwrap();
    let actual = optimized.readback(candidate).unwrap();
    assert_eq!(expected.len(), actual.len());
    for (before, after) in expected.iter().zip(&actual) {
        for (a, b) in before.iter().zip(after) {
            assert!((a - b).abs() <= 0.004);
        }
    }
}
