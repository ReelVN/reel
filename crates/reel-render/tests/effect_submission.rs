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
        widget.clip = !color;
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
        let mut bridge = if color {
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

#[cfg(feature = "wgpu")]
#[test]
#[ignore = "requires an actual WGPU adapter"]
fn wgpu_prepared_effects_submit_scalar_and_empty_source_color() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    for (color, external, coverage) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, true, true),
    ] {
        // Scalar masks also retain the local coverage and covered source.
        let target_bytes = (if coverage { 5 } else { 4 }) * SIZE[0] as u64 * SIZE[1] as u64 * 4;
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
        let frames = frames_with_inputs(Language::Wgsl, color, external, coverage);
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
    for (color, external, coverage) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, true, true),
    ] {
        let mut renderer = GlesBackend::headless(
            SIZE,
            GlesLimits {
                target_bytes: (if coverage { 5 } else { 3 }) * SIZE[0] as u64 * SIZE[1] as u64 * 4,
                ..Default::default()
            },
        )
        .unwrap();
        let frames = frames_with_inputs(Language::GlslEs100, color, external, coverage);
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
