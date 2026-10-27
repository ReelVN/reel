//! Complete prepared stage effects, resource retention and real submission; no pixels are read.
#[allow(dead_code, reason = "shared UI fixture")]
#[path = "../../reel-ui/tests/support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared frame fixture")]
#[path = "../../reel-ui/tests/support/r01_fixture.rs"]
mod r01_fixture;
use r01_fixture::*;
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_render::effect::{Language, compile_fragment};
use reel_ui::{
    core::PaintEffect,
    core::Widget,
    frame::{FrameBuild, FrameInfo, FrameOp, InkBounds, RenderFrame, RetainBuild},
    imaging::{
        Composite,
        record::{Draw, Geometry, Scene},
    },
    kurbo::{Affine, Point, Rect, Size},
    peniko::{Brush, Color},
    resources::{EffectShaderDescriptor, EffectShaderFormat, ResourceRegistry},
    stage::effect::*,
};
use std::sync::Arc;
const SIZE: [u32; 2] = [160, 120];

fn program(color: bool) -> Arc<EffectProgram> {
    let mut nodes = vec![
        EffectNode {
            ty: EffectType::Float,
            op: EffectOp::Parameter(0),
        },
        EffectNode {
            ty: EffectType::Vec2,
            op: EffectOp::Input(EffectInput::Uv),
        },
        EffectNode {
            ty: EffectType::Vec4,
            op: EffectOp::SourceSample {
                uv: EffectNodeId(1),
            },
        },
    ];
    let output = if color {
        nodes.push(EffectNode {
            ty: EffectType::Vec4,
            op: EffectOp::Constant(EffectValue::Vec4(
                [0.125f32, 0.25, 0.5, 0.75].map(f32::to_bits),
            )),
        });
        EffectNodeId(3)
    } else {
        EffectNodeId(0)
    };
    EffectProgram::new(
        vec![EffectParameter {
            name: "coverage".into(),
            ty: EffectType::Float,
            default: None,
        }],
        nodes,
        output,
    )
    .unwrap()
}
fn frames(language: Language, color: bool) -> Vec<RenderFrame> {
    frames_with_texture(language, color, false)
}
fn frames_with_texture(language: Language, color: bool, external: bool) -> Vec<RenderFrame> {
    frames_with_inputs(language, color, external, false)
}
fn frames_with_inputs(
    language: Language,
    color: bool,
    external: bool,
    coverage: bool,
) -> Vec<RenderFrame> {
    frames_with_mode(language, color, external, coverage, false)
}
fn frames_with_mode(
    language: Language,
    color: bool,
    external: bool,
    coverage: bool,
    plain: bool,
) -> Vec<RenderFrame> {
    frames_with_chain(language, color, external, coverage, plain, 1)
}
fn frames_with_chain(
    language: Language,
    color: bool,
    external: bool,
    coverage: bool,
    plain: bool,
    depth: usize,
) -> Vec<RenderFrame> {
    let mut program = program(color);
    if external {
        let mut nodes = program.nodes().to_vec();
        nodes.push(EffectNode {
            ty: EffectType::Float,
            op: EffectOp::TextureSample {
                binding: 0,
                uv: EffectNodeId(1),
                channel: EffectTextureChannel::Gray,
            },
        });
        nodes.push(EffectNode {
            ty: EffectType::Float,
            op: EffectOp::TextureSample {
                binding: 0,
                uv: EffectNodeId(1),
                channel: EffectTextureChannel::Alpha,
            },
        });
        nodes.push(EffectNode {
            ty: EffectType::Float,
            op: EffectOp::Binary {
                op: EffectBinary::Multiply,
                left: EffectNodeId(0),
                right: EffectNodeId(3),
            },
        });
        nodes.push(EffectNode {
            ty: EffectType::Float,
            op: EffectOp::Binary {
                op: EffectBinary::Multiply,
                left: EffectNodeId(5),
                right: EffectNodeId(4),
            },
        });
        program = EffectProgram::with_textures(
            program.parameters().to_vec(),
            vec![EffectTextureParameter { name: "ink".into() }],
            nodes,
            EffectNodeId(6),
        )
        .unwrap();
    }
    let fragment = compile_fragment(&program, language, 32768).unwrap();
    let format = match language {
        Language::GlslEs100 => EffectShaderFormat::GlslEs100,
        Language::Wgsl => EffectShaderFormat::Wgsl,
    };
    let descriptor = EffectShaderDescriptor::with_textures(
        [3; 32],
        format,
        vec![EffectType::Float],
        program.textures().len() as u32,
        fragment.source_sampler,
        fragment.output_type,
        fragment.source.as_bytes(),
    )
    .unwrap();
    let bytes = fragment.source.into_bytes();
    let pool = reel_core::budget::BudgetPool::new(
        reel_core::budget::BudgetCategory::CpuOutput,
        reel_core::budget::QueueLimits {
            max_requests: 1024,
            max_bytes: 16 << 20,
        },
    );
    let bytes = OwnedBytes::new(bytes.clone(), pool.reserve(bytes.len() as u64).unwrap()).unwrap();
    let mut registry = ResourceRegistry::new(8, 1 << 20).unwrap();
    let shader = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey(format.asset_path([3; 32])),
                version: ContentVersion([4; 32]),
            },
            descriptor,
            bytes,
        )
        .unwrap();
    let texture = external.then(|| {
        registry
            .register_image(
                ResourceVersion {
                    key: AssetKey("effect-external-image".into()),
                    version: ContentVersion([8; 32]),
                },
                OwnedBytes::new(
                    vec![
                        0, 0, 0, 255, 32, 64, 96, 128, 128, 64, 32, 192, 255, 255, 255, 255,
                    ],
                    pool.reserve(16).unwrap(),
                )
                .unwrap(),
                [2, 2],
                reel_ui::peniko::ImageFormat::Rgba8,
                reel_ui::peniko::ImageAlphaType::Alpha,
                reel_ui::resources::ColorEncoding::Srgb,
            )
            .unwrap()
    });
    let mut frames = Vec::new();
    for index in 0..3 {
        let mut scene = Scene::new();
        if !color {
            scene.draw(Draw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Solid(Color::from_rgba8(64, 128, 192, 192)),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., 0., 240., 80.)),
                composite: Composite::default(),
            });
        }
        let mut widget = RecordWidget::new(scene);
        widget.clip = !color && !plain;
        widget.inner.clip = Rect::new(0., 0., 80., 70.);
        widget.opacity = 0.75;
        let paint = PaintEffect {
            instance: EffectInstance::with_textures(
                [3; 32],
                program.clone(),
                vec![EffectValue::Float((index as f32 * 0.5).to_bits())],
                if external {
                    vec!["effect-external-image".into()]
                } else {
                    vec![]
                },
            )
            .unwrap(),
            shader: registry.acquire(shader).unwrap(),
            images: texture
                .map(|handle| vec![registry.acquire(handle).unwrap()])
                .unwrap_or_default(),
            bounds: Rect::new(0., 0., if color { 100. } else { 240. }, 80.),
            coverage_invert: coverage,
        };
        if coverage {
            widget.mask_effect = Some(paint);
        } else {
            widget.effect = Some(paint);
        }
        if depth > 1 {
            widget.opacity = 1.;
            for _ in 1..depth {
                let mut parent = RecordWidget::new(Scene::new());
                parent.clip = false;
                parent.effect = widget.effect.clone();
                parent
                    .inner
                    .add(widget.prepare(), Point::ZERO, Size::new(240., 80.));
                widget = parent;
            }
        }
        let mut bridge = if color || plain {
            widget_bridge(widget, Affine::translate((18., 12.)) * Affine::rotate(0.1))
        } else {
            // The source is wider than the viewport and starts outside it.
            // Both ancestor and self clips apply to the processed output only.
            let mut ancestor = RecordWidget::new(Scene::new());
            ancestor.inner.clip = Rect::new(0., 0., 55., 50.);
            ancestor.inner.add(
                widget
                    .prepare()
                    .with_transform(Affine::translate((-40., -18.))),
                Point::ZERO,
                Size::new(240., 80.),
            );
            widget_bridge(ancestor, Affine::IDENTITY)
        };
        let mut build = FrameBuild::new(
            bridge.record(FrameId(index + 1)).unwrap(),
            registry
                .freeze(
                    &std::iter::once(shader).chain(texture).collect::<Vec<_>>(),
                    &pool,
                )
                .unwrap(),
            FrameInfo {
                logical_size: [160., 120.],
                physical_size: SIZE,
            },
            limits(),
            &pool,
        )
        .unwrap();
        while !build.step(step_budget()).unwrap().complete {}
        let frame = build.finish().unwrap();
        let group = frame
            .commands()
            .iter()
            .find_map(|command| match &command.op {
                FrameOp::PushGroup(group)
                    if group.effect.is_some() || group.mask_effect.is_some() =>
                {
                    Some(group)
                }
                _ => None,
            })
            .unwrap();
        assert!(
            matches!(group.ink_bounds,InkBounds::Rect(rect) if rect.width()>0. && rect.height()>0.)
        );
        assert_eq!(
            group
                .effect
                .as_ref()
                .or(group.mask_effect.as_ref())
                .unwrap()
                .values,
            vec![EffectValue::Float((index as f32 * 0.5).to_bits())]
        );
        assert_eq!(
            group
                .mask_effect
                .as_ref()
                .is_some_and(|effect| effect.coverage_invert),
            coverage
        );
        frames.push(frame);
    }
    // Retained pictures keep shader leases and resolved values after producer retirement.
    let mut retain = RetainBuild::new(
        &frames[1],
        None,
        reel_ui::frame::BuildLimits { max_bytes: 1 << 20 },
        &pool,
    )
    .unwrap();
    while !retain.step(step_budget()).unwrap().complete {}
    let retained = retain.finish().unwrap();
    assert!(
        retained
            .drawing()
            .resources()
            .leases()
            .iter()
            .any(|lease| lease.handle() == shader)
    );
    let saved = retained.snapshot(|_| None).unwrap();
    let encoded = postcard::to_allocvec(&saved).unwrap();
    let saved: reel_ui::frame::SavedRetainedScene = postcard::from_bytes(&encoded).unwrap();
    let mut restore = saved.restore_begin(limits(), &pool).unwrap();
    let budget = reel_ui::frame::StepBudget {
        max_commands: 1024,
        max_units: 1 << 20,
        max_bytes: 1 << 20,
    };
    while !restore.step(budget).unwrap().build.complete {}
    let restored = restore.finish().unwrap();
    assert!(restored.drawing().commands().iter().any(|command|matches!(&command.op,FrameOp::PushGroup(group) if group.effect.as_ref().or(group.mask_effect.as_ref()).is_some_and(|effect|effect.values==vec![EffectValue::Float(0.5f32.to_bits())] && effect.coverage_invert == coverage))));
    if let Some(texture) = texture {
        assert!(
            retained
                .drawing()
                .resources()
                .leases()
                .iter()
                .any(|lease| lease.handle() == texture)
        );
        registry.retire(texture);
    }
    registry.retire(shader);
    frames
}

#[test]
fn prepared_effect_frame_retains_bounds_values_and_shader() {
    for language in [Language::GlslEs100, Language::Wgsl] {
        for color in [false, true] {
            let _ = frames(language, color);
        }
        let _ = frames_with_texture(language, false, true);
        let _ = frames_with_inputs(language, false, true, true);
    }
}

fn retained_effect_transition_frames(language: Language) -> Vec<RenderFrame> {
    let old = frames_with_chain(language, false, false, false, true, 8).remove(0);
    let pool = pool();
    let mut retain = RetainBuild::new(&old, None, limits(), &pool).unwrap();
    let mut budget = step_budget();
    loop {
        let progress = retain.step(budget).unwrap();
        if progress.complete {
            break;
        }
        budget.max_units = budget.max_units.max(progress.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
    }
    let previous = retain.finish().unwrap();
    let mut bridge = widget_bridge(RecordWidget::new(Scene::new()), Affine::IDENTITY);
    [0., 0.25, 0.75]
        .into_iter()
        .enumerate()
        .map(|(index, progress)| {
            let mut build = FrameBuild::new(
                bridge.record(FrameId(index as u64 + 10)).unwrap(),
                empty_snapshot(&pool),
                FrameInfo {
                    logical_size: SIZE.map(f64::from),
                    physical_size: SIZE,
                },
                limits(),
                &pool,
            )
            .unwrap();
            build
                .set_transitions(vec![reel_ui::frame::FrameTransition {
                    root: None,
                    previous: previous.clone(),
                    progress,
                    mask: None,
                }])
                .unwrap();
            let mut budget = step_budget();
            loop {
                let progress = build.step(budget).unwrap();
                if progress.complete {
                    break;
                }
                budget.max_units = budget.max_units.max(progress.minimum_units);
                budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
            }
            build.finish().unwrap()
        })
        .collect()
}

// Two sequential local domains need four exact scratch textures, but only two
// shared physical textures. The output retains its exact presentation extent.
fn mixed_extent_effect_frame(language: Language) -> RenderFrame {
    let program = program(false);
    let fragment = compile_fragment(&program, language, 64 << 10).unwrap();
    let format = match language {
        Language::Wgsl => EffectShaderFormat::Wgsl,
        Language::GlslEs100 => EffectShaderFormat::GlslEs100,
    };
    let descriptor = EffectShaderDescriptor::with_textures(
        [7; 32],
        format,
        vec![EffectType::Float],
        0,
        fragment.source_sampler,
        fragment.output_type,
        fragment.source.as_bytes(),
    )
    .unwrap();
    let pool = reel_core::budget::BudgetPool::new(
        reel_core::budget::BudgetCategory::CpuOutput,
        reel_core::budget::QueueLimits {
            max_requests: 1024,
            max_bytes: 16 << 20,
        },
    );
    let payload = fragment.source.into_bytes();
    let bytes =
        OwnedBytes::new(payload.clone(), pool.reserve(payload.len() as u64).unwrap()).unwrap();
    let mut registry = ResourceRegistry::new(8, 1 << 20).unwrap();
    let shader = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey(format.asset_path([7; 32])),
                version: ContentVersion([7; 32]),
            },
            descriptor,
            bytes,
        )
        .unwrap();
    let mut root = RecordWidget::new(Scene::new());
    root.clip = false;
    for (index, extent) in [Size::new(240., 80.), Size::new(200., 100.)]
        .into_iter()
        .enumerate()
    {
        let bounds = Rect::from_origin_size(Point::ZERO, extent);
        let paint = PaintEffect {
            instance: EffectInstance::new(
                [7; 32],
                program.clone(),
                vec![EffectValue::Float(0.75f32.to_bits())],
            )
            .unwrap(),
            shader: registry.acquire(shader).unwrap(),
            images: vec![],
            bounds,
            coverage_invert: false,
        };
        let mut scene = Scene::new();
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(Color::from_rgba8(64, 128, 192, 192)),
            brush_transform: None,
            shape: Geometry::Rect(bounds),
            composite: Composite::default(),
        });
        let mut widget = RecordWidget::new(scene);
        widget.clip = false;
        if index == 0 {
            widget.effect = Some(paint.clone());
        } else {
            widget.mask_effect = Some(paint.clone());
        }
        for _ in 1..if index == 0 { 8 } else { 1 } {
            let mut parent = RecordWidget::new(Scene::new());
            parent.clip = false;
            parent.effect = Some(paint.clone());
            parent.inner.add(widget.prepare(), Point::ZERO, extent);
            widget = parent;
        }
        root.inner.add(
            widget.prepare(),
            Point::new(index as f64 * 10., index as f64 * 5.),
            extent,
        );
    }
    let mut bridge = widget_bridge(root, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[shader], &pool).unwrap(),
        FrameInfo {
            logical_size: [160., 120.],
            physical_size: SIZE,
        },
        limits(),
        &pool,
    )
    .unwrap();
    while !build.step(step_budget()).unwrap().complete {}
    build.finish().unwrap()
}

#[cfg(feature = "wgpu")]
#[test]
#[ignore = "requires an actual WGPU adapter; no readback"]
fn wgpu_sequential_effect_extents_share_physical_scratch_under_pressure() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    let limit = 400_000;
    assert!(4 * (2 * 160 * 120 + 2 * 240 * 80 + 2 * 200 * 100) > limit);
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits {
            target_bytes: limit,
            ..Default::default()
        },
    ))
    .unwrap();
    let mut renderer = Renderer::new(gpu);
    let frame = mixed_extent_effect_frame(Language::Wgsl);
    let prepared = renderer.prepare(&frame).unwrap();
    assert_eq!(prepared.size(), SIZE);
    assert_eq!(prepared.quality().format, wgpu::TextureFormat::Rgba8Unorm);
    assert_eq!(prepared.quality().samples, 1);
    assert_eq!(renderer.usage().targets, 345_600);
    renderer.submit(prepared).unwrap();
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert_eq!(renderer.poll().unwrap().len(), 1);
}

#[cfg(feature = "gles2")]
#[test]
#[ignore = "requires a native GLES context; no readback"]
fn gles_sequential_effect_extents_share_physical_scratch_under_pressure() {
    use reel_render::{
        AlphaMode, PresentOutcome, RenderBackend, SurfaceInfo,
        gles2::{GlesBackend, GlesLimits},
    };
    let mut renderer = GlesBackend::headless(
        SIZE,
        GlesLimits {
            target_bytes: 400_000,
            ..Default::default()
        },
    )
    .unwrap();
    renderer
        .configure(SurfaceInfo {
            physical_size: SIZE,
            scale_factor: 1.,
            surface_epoch: reel_core::identity::SurfaceEpoch(1),
            color_encoding: reel_ui::resources::ColorEncoding::Srgb,
            alpha_mode: AlphaMode::Opaque,
        })
        .unwrap();
    let frame = mixed_extent_effect_frame(Language::GlslEs100);
    let mut presents = 0;
    assert!(matches!(
        renderer.render_present(&frame, || presents += 1).unwrap(),
        PresentOutcome::PresentAccepted(_)
    ));
    assert_eq!(presents, 1);
    let mut events = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while renderer.has_pending() {
        renderer.poll(&mut events, 64).unwrap();
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(
        events
            .iter()
            .any(|event| matches!(event, reel_render::RenderEvent::RenderFinished { .. }))
    );
}

#[cfg(feature = "wgpu")]
#[test]
#[ignore = "requires an actual WGPU adapter; no readback"]
fn wgpu_retained_effects_prepare_once_under_a_split_target_budget() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuError, GpuLimits, Renderer};
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits {
            target_bytes: u64::from(SIZE[0]) * u64::from(SIZE[1]) * 4 * 5,
            ..Default::default()
        },
    ))
    .unwrap();
    let mut renderer = Renderer::new(gpu);
    let frames = retained_effect_transition_frames(Language::Wgsl);
    for (index, frame) in frames.iter().enumerate() {
        let mut phases = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match renderer.prepare(frame) {
                Ok(prepared) => {
                    renderer.submit(prepared).unwrap();
                    break;
                }
                Err(GpuError::Preparing) => {
                    phases += 1;
                }
                Err(error) if error.is_retryable() => {}
                Err(error) => panic!("retained phase: {error}"),
            }
            renderer
                .gpu()
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            renderer.poll().unwrap();
            assert!(std::time::Instant::now() < deadline);
        }
        assert!(phases >= usize::from(index == 0));
        if index != 0 {
            assert_eq!(
                phases, 0,
                "hot retained result skips dependency preparation"
            );
        }
        renderer
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
    }
    let mut renderer = Renderer::new(
        pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            DeviceGeneration(2),
            GpuLimits {
                target_bytes: u64::from(SIZE[0]) * u64::from(SIZE[1]) * 4 * 5,
                ..Default::default()
            },
        ))
        .unwrap(),
    );
    let frame = &frames[0];
    loop {
        match renderer.prepare(frame) {
            Err(GpuError::Preparing) => break,
            Err(error) if error.is_retryable() => {}
            result => panic!(
                "cold cancellation fixture must first prepare its retained dependency: {}",
                result.err().map_or_else(
                    || "unexpected complete frame".into(),
                    |error| error.to_string()
                )
            ),
        }
        renderer
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
    }
    renderer.cancel_preparation(frame);
    assert!(
        renderer.usage().targets > 0,
        "submitted owners survive cancellation"
    );
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    assert_eq!(
        renderer.usage().targets,
        0,
        "completed canceled dependencies release their targets"
    );
}

#[cfg(feature = "gles2")]
#[test]
#[ignore = "requires a native GLES context; no readback"]
fn gles_retained_effects_prepare_once_under_a_split_target_budget() {
    use reel_render::{
        AlphaMode, PresentOutcome, RenderBackend, RenderEvent, SurfaceInfo,
        gles2::{GlesBackend, GlesLimits},
    };
    let mut renderer = GlesBackend::headless(
        SIZE,
        GlesLimits {
            target_bytes: u64::from(SIZE[0]) * u64::from(SIZE[1]) * 4 * 5,
            ..Default::default()
        },
    )
    .unwrap();
    renderer
        .configure(SurfaceInfo {
            physical_size: SIZE,
            scale_factor: 1.,
            surface_epoch: reel_core::identity::SurfaceEpoch(1),
            color_encoding: reel_ui::resources::ColorEncoding::Srgb,
            alpha_mode: AlphaMode::Opaque,
        })
        .unwrap();
    let frames = retained_effect_transition_frames(Language::GlslEs100);
    let mut events = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        let mut phases = 0;
        let mut presents = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match renderer.render_present(frame, || presents += 1) {
                Ok(PresentOutcome::PresentAccepted(id)) => {
                    assert_eq!(id, frame.frame_id());
                    break;
                }
                Ok(PresentOutcome::Preparing) => {
                    phases += 1;
                    assert_eq!(presents, 0);
                }
                Err(error) if error.is_retryable() => {}
                result => panic!("retained phase: {result:?}"),
            }
            events.clear();
            renderer.poll(&mut events, 64).unwrap();
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, RenderEvent::RenderFinished { .. })),
                "dependency completion never accepts/presents a frame"
            );
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(presents, 1);
        assert!(phases >= usize::from(index == 0));
        if index != 0 {
            assert_eq!(
                phases, 0,
                "hot retained result skips dependency preparation"
            );
        }
        while renderer.has_pending() {
            renderer.poll(&mut events, 64).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

#[cfg(feature = "wgpu")]
#[test]
#[ignore = "requires an actual WGPU adapter"]
fn wgpu_prepared_effects_submit_scalar_and_empty_source_color() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    for (color, external, coverage, plain, depth) in [
        (false, false, false, false, 1),
        (true, false, false, false, 1),
        (false, true, false, false, 1),
        (false, true, true, false, 1),
        (false, true, true, true, 1),
        (false, false, false, true, 8),
    ] {
        // A plain scalar mask needs the parent plus two alternating local sources.
        let target_bytes = (if depth > 1 {
            4
        } else if plain {
            3
        } else if coverage {
            5
        } else {
            4
        }) * SIZE[0] as u64
            * SIZE[1] as u64
            * 4;
        let gpu = pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            DeviceGeneration(1),
            GpuLimits {
                target_bytes,
                ..Default::default()
            },
        ))
        .unwrap();
        let mut renderer = Renderer::new(gpu);
        let frames = frames_with_chain(Language::Wgsl, color, external, coverage, plain, depth);
        let mut resident = None;
        for frame in &frames {
            let prepared = renderer.prepare(frame).unwrap();
            assert_eq!(prepared.quality().format, wgpu::TextureFormat::Rgba8Unorm);
            renderer.submit(prepared).unwrap();
            renderer
                .gpu()
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            renderer.poll().unwrap();
            let bytes = renderer.usage().resources;
            if let Some(previous) = resident {
                assert_eq!(
                    bytes, previous,
                    "parameter updates reuse compiled shader storage"
                );
            }
            resident = Some(bytes);
        }
    }
}

#[cfg(feature = "gles2")]
#[test]
#[ignore = "requires a native GLES context"]
fn gles_prepared_effects_submit_scalar_and_empty_source_color() {
    use reel_render::{
        AlphaMode, RenderBackend, RenderEvent, SurfaceInfo,
        gles2::{GlesBackend, GlesLimits},
    };
    for (color, external, coverage, plain, depth) in [
        (false, false, false, false, 1),
        (true, false, false, false, 1),
        (false, true, false, false, 1),
        (false, true, true, false, 1),
        (false, true, true, true, 1),
        (false, false, false, true, 8),
    ] {
        let mut renderer = GlesBackend::headless(
            SIZE,
            GlesLimits {
                target_bytes: (if depth > 1 {
                    4
                } else if plain {
                    3
                } else if coverage {
                    5
                } else {
                    3
                }) * SIZE[0] as u64
                    * SIZE[1] as u64
                    * 4,
                ..Default::default()
            },
        )
        .unwrap();
        let frames =
            frames_with_chain(Language::GlslEs100, color, external, coverage, plain, depth);
        renderer
            .configure(SurfaceInfo {
                physical_size: SIZE,
                scale_factor: 1.,
                surface_epoch: frames[0].surface_epoch(),
                color_encoding: reel_ui::resources::ColorEncoding::Srgb,
                alpha_mode: AlphaMode::Opaque,
            })
            .unwrap();
        let mut resident = None;
        for frame in &frames {
            renderer.render_present(frame, || {}).unwrap();
            let start = std::time::Instant::now();
            let mut events = Vec::new();
            loop {
                renderer.poll(&mut events, 32).unwrap();
                if events
                    .iter()
                    .any(|event| matches!(event, RenderEvent::RenderFinished { .. }))
                {
                    break;
                }
                assert!(start.elapsed() < std::time::Duration::from_secs(10));
                std::thread::yield_now();
            }
            let bytes = renderer.usage().bytes;
            if let Some(previous) = resident {
                assert_eq!(
                    bytes, previous,
                    "parameter updates reuse compiled shader storage"
                );
            }
            resident = Some(bytes);
        }
        renderer.close().unwrap();
    }
}
