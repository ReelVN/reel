#![cfg(feature = "wgpu")]
#[allow(dead_code, reason = "independent reference shared with contract tests")]
#[path = "support/color.rs"]
mod color;
#[allow(dead_code, reason = "shared actual Masonry fixture")]
#[path = "../../reel-ui/tests/support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared R01 compiler fixture")]
#[path = "../../reel-ui/tests/support/r01_fixture.rs"]
mod r01_fixture;
use r01_fixture::*;
use reel_core::identity::FrameId;
use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
use reel_ui::{
    imaging::{
        Composite,
        record::{Draw, Geometry, Scene},
    },
    kurbo::{Affine, Rect},
    peniko::{Brush, Color},
};
fn scene(color: Color) -> Scene {
    let mut scene = Scene::new();
    fill(&mut scene, color);
    scene
}
fn fill(scene: &mut Scene, color: Color) {
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(color),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., 400., 300.)),
        composite: Composite::default(),
    });
}
#[test]
#[ignore = "requires actual Vulkan/Metal/DX12 adapter; run explicitly"]
fn linear_pixels_completion_and_frozen_crossfade() {
    let instance = wgpu::Instance::default();
    let gpu = pollster::block_on(Gpu::request(
        &instance,
        None,
        DeviceGeneration(1),
        GpuLimits::default(),
    ))
    .expect("actual device required");
    eprintln!("adapter={:?}", gpu.adapter.get_info());
    let mut renderer = Renderer::new(gpu);
    let pool = pool();
    let mut a = widget_bridge(
        RecordWidget::new(scene(Color::from_rgba8(255, 0, 0, 128))),
        Affine::IDENTITY,
    );
    let mut b = widget_bridge(
        RecordWidget::new(scene(Color::from_rgba8(0, 0, 255, 255))),
        Affine::IDENTITY,
    );
    let a = build(a.record(FrameId(1)).unwrap(), empty_snapshot(&pool), &pool);
    let b = build(b.record(FrameId(2)).unwrap(), empty_snapshot(&pool), &pool);
    let prepared = renderer.crossfade(&a, &b, 0.5).unwrap();
    eprintln!(
        "quality={:?} queued={:?}",
        prepared.quality(),
        renderer.usage()
    );
    let targets = renderer.usage().targets;
    assert!(targets > 0);
    let pixels = renderer.readback(prepared).unwrap();
    let p = pixels[100 * 400 + 100];
    assert!((p[0] - 128. / 255. * 0.5).abs() < 0.005, "{p:?}");
    assert!((p[2] - 0.5).abs() < 0.005, "{p:?}");
    assert!((p[3] - (128. / 255. + 1.) * 0.5).abs() < 0.005, "{p:?}");
    assert!(!renderer.has_pending());
    let repeated = renderer.crossfade(&a, &b, 0.5).unwrap();
    assert_eq!(renderer.usage().targets, targets);
    close(pixel(&renderer.readback(repeated).unwrap(), 100, 100), p);
}

fn device() -> Renderer {
    let instance = wgpu::Instance::default();
    let limits = GpuLimits {
        target_bytes: 512 << 20,
        ..Default::default()
    };
    Renderer::new(
        pollster::block_on(Gpu::request(&instance, None, DeviceGeneration(1), limits)).unwrap(),
    )
}
fn pixel(pixels: &[[f32; 4]], x: usize, y: usize) -> [f32; 4] {
    pixels[y * 400 + x]
}
fn close(actual: [f32; 4], expected: [f32; 4]) {
    for i in 0..4 {
        assert!(
            (actual[i] - expected[i]).abs() < 0.006,
            "actual={actual:?}, expected={expected:?}"
        );
    }
}

fn read_frame(renderer: &mut Renderer, frame: &reel_ui::frame::RenderFrame) -> Vec<[f32; 4]> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match renderer.prepare(frame) {
            Ok(prepared) => return renderer.readback(prepared).unwrap(),
            Err(reel_render::gpu::GpuError::Preparing) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "frame preparation stalled"
                );
                renderer
                    .gpu()
                    .device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                renderer.poll().unwrap();
            }
            Err(error) => panic!("frame preparation failed: {error}"),
        }
    }
}
#[test]
#[ignore = "requires an actual graphics adapter"]
fn whole_group_opacity_and_nested_masks_clips() {
    use reel_ui::imaging::{
        MaskMode,
        record::{AppliedMask, Clip, Group, Mask},
    };
    let instance = wgpu::Instance::default();
    let mut renderer = Renderer::new(
        pollster::block_on(Gpu::request(
            &instance,
            None,
            DeviceGeneration(1),
            GpuLimits::default(),
        ))
        .unwrap(),
    );
    eprintln!(
        "nested composition: {:?}",
        renderer.gpu().adapter.get_info()
    );
    let pool = pool();
    let mut s = Scene::new();
    s.push_group(Group {
        composite: Composite {
            alpha: 0.5,
            ..Default::default()
        },
        ..Default::default()
    });
    for color in [Color::from_rgb8(255, 0, 0), Color::from_rgb8(0, 0, 255)] {
        fill(&mut s, color);
    }
    s.pop_group();
    let mut bridge = widget_bridge(RecordWidget::new(s), Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let pixels = renderer
        .readback(renderer.prepare(&frame).unwrap())
        .unwrap();
    close(pixel(&pixels, 100, 100), [0., 0., 0.5, 0.5]);

    let mut coverage = Scene::new();
    let inner = coverage.define_mask(Mask::new(
        MaskMode::Alpha,
        scene(Color::from_rgba8(255, 255, 255, 128)),
    ));
    coverage.push_group(Group {
        mask: Some(AppliedMask::new(inner)),
        ..Default::default()
    });
    fill(&mut coverage, Color::from_rgb8(255, 0, 0));
    coverage.pop_group();
    let mut s = Scene::new();
    let mask = s.define_mask(Mask::new(MaskMode::Luminance, coverage.clone()));
    s.push_clip(Clip::Fill {
        transform: Affine::IDENTITY,
        shape: Geometry::Rect(Rect::new(20., 20., 180., 180.)),
        fill_rule: Default::default(),
    });
    s.push_group(Group {
        mask: Some(AppliedMask::new(mask)),
        composite: Composite {
            alpha: 0.5,
            ..Default::default()
        },
        ..Default::default()
    });
    fill(&mut s, Color::WHITE);
    s.pop_group();
    s.pop_clip();
    let mut bridge = widget_bridge(RecordWidget::new(s), Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(2)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let pixels = renderer
        .readback(renderer.prepare(&frame).unwrap())
        .unwrap();
    close(pixel(&pixels, 100, 100), [0.2126 * 128. / 255. * 0.5; 4]);
    close(pixel(&pixels, 200, 200), [0.; 4]);

    // Repeating sibling compositions must reuse their completed targets without
    // recycling the enclosing group, either mask input, or a frozen fade input.
    let mut peak = None;
    for count in [1, 24] {
        let mut s = Scene::new();
        let mask = s.define_mask(Mask::new(MaskMode::Luminance, coverage.clone()));
        for index in 0..count {
            let x = (index % 6 * 64) as f64;
            let y = (index / 6 * 70) as f64;
            s.push_group(Group {
                composite: Composite {
                    alpha: 0.5,
                    ..Default::default()
                },
                ..Default::default()
            });
            s.push_clip(Clip::Fill {
                transform: Affine::IDENTITY,
                shape: Geometry::Rect(Rect::new(x + 8., y + 8., x + 56., y + 62.)),
                fill_rule: Default::default(),
            });
            s.push_group(Group {
                mask: Some(AppliedMask::new(mask)),
                ..Default::default()
            });
            fill(
                &mut s,
                if index % 2 == 0 {
                    Color::from_rgb8(255, 0, 0)
                } else {
                    Color::from_rgb8(0, 255, 0)
                },
            );
            s.pop_group();
            s.pop_clip();
            s.pop_group();
        }
        let mut bridge = widget_bridge(RecordWidget::new(s), Affine::IDENTITY);
        let frame = build(
            bridge.record(FrameId(3)).unwrap(),
            empty_snapshot(&pool),
            &pool,
        );
        let prepared = renderer.prepare(&frame).unwrap();
        let used = renderer.usage().targets;
        assert!(used <= GpuLimits::default().target_bytes);
        assert_eq!(used, *peak.get_or_insert(used));
        eprintln!(
            "{count} sibling compositions: {:?}, targets={used}",
            prepared.quality()
        );
        let pixels = renderer.readback(prepared).unwrap();
        let alpha = 0.2126 * 128. / 255. * 0.5;
        for index in 0..count {
            let x = index % 6 * 64;
            let y = index / 6 * 70;
            let expected = if index % 2 == 0 {
                [alpha, 0., 0., alpha]
            } else {
                [0., alpha, 0., alpha]
            };
            close(pixel(&pixels, x + 32, y + 32), expected);
            close(pixel(&pixels, x + 2, y + 32), [0.; 4]);
        }
        close(pixel(&pixels, 399, 299), [0.; 4]);
        assert!(!renderer.has_pending());
        assert!(renderer.usage().targets <= used);

        if count == 24 {
            let mut other = widget_bridge(
                RecordWidget::new(scene(Color::from_rgb8(0, 0, 255))),
                Affine::IDENTITY,
            );
            let other = build(
                other.record(FrameId(4)).unwrap(),
                empty_snapshot(&pool),
                &pool,
            );
            let prepared = renderer.crossfade(&frame, &other, 0.5).unwrap();
            let fade_targets = renderer.usage().targets;
            assert!(fade_targets <= GpuLimits::default().target_bytes);
            let pixels = renderer.readback(prepared).unwrap();
            close(
                pixel(&pixels, 32, 32),
                [alpha * 0.5, 0., 0.5, (1. + alpha) * 0.5],
            );
            close(
                pixel(&pixels, 96, 32),
                [0., alpha * 0.5, 0.5, (1. + alpha) * 0.5],
            );
            close(pixel(&pixels, 2, 32), [0., 0., 0.5, 0.5]);
            assert!(!renderer.has_pending());
            assert!(renderer.usage().targets <= fade_targets);
        }
    }

    // Each parent has its own draw and a translucent child. Their simultaneous
    // full-size targets exceed 128 MiB even at RGBA8/1x.
    let mut s = Scene::new();
    for _ in 0..300 {
        s.push_group(Group {
            composite: Composite {
                alpha: 0.5,
                ..Default::default()
            },
            ..Default::default()
        });
        fill(&mut s, Color::WHITE);
    }
    fill(&mut s, Color::WHITE);
    for _ in 0..300 {
        s.pop_group();
    }
    let mut bridge = widget_bridge(RecordWidget::new(s), Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(5)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let before_refusal = renderer.usage();
    let mut retained = None;
    for _ in 0..2 {
        assert!(matches!(
            renderer.prepare(&frame),
            Err(reel_render::gpu::GpuError::Budget {
                domain: "targets",
                ..
            })
        ));
        assert!(!renderer.has_pending());
        let usage = renderer.usage();
        assert!(usage.targets <= before_refusal.targets);
        assert!(usage.resources <= before_refusal.resources);
        // The group plan can remain cached. Retrying the same frame must reuse it.
        assert_eq!(usage.transient, *retained.get_or_insert(usage.transient));
    }
    let mut bridge = widget_bridge(RecordWidget::new(scene(Color::WHITE)), Affine::IDENTITY);
    let simple = build(
        bridge.record(FrameId(6)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    close(
        pixel(&read_frame(&mut renderer, &simple), 100, 100),
        [1.; 4],
    );
}
#[test]
#[ignore = "requires an actual graphics adapter"]
fn bounded_admission_and_completion_preserve_frame_lease() {
    let mut renderer = device();
    let pool = pool();
    let mut bridge = widget_bridge(RecordWidget::new(scene(Color::WHITE)), Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let a = renderer.prepare(&frame).unwrap();
    let b = renderer.prepare(&frame).unwrap();
    assert!(matches!(
        renderer.prepare(&frame),
        Err(reel_render::gpu::GpuError::Busy)
    ));
    drop(b);
    let id = renderer.submit(a).unwrap();
    drop(frame);
    assert!(pool.usage().bytes > 0);
    assert!(renderer.usage().targets > 0);
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    // Driver completion alone cannot release application ownership until poll observes it.
    assert!(renderer.usage().targets > 0);
    assert_eq!(renderer.poll().unwrap(), vec![id]);
    assert!(!renderer.has_pending());
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
#[ignore = "requires adapter and REEL_R01_TEST_FONT"]
fn image_linear_upload_retirement_and_actual_parley_glyphs() {
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::resources::{ResourceData, ResourceRegistry};
    use reel_ui::{
        app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
        core::{DefaultProperties, StyleProperty, Widget},
        dpi::PhysicalSize,
        peniko::{Blob, ImageAlphaType, ImageBrush, ImageFormat},
    };
    use std::sync::Arc;
    let mut renderer = device();
    eprintln!("adapter={:?}", renderer.gpu().adapter.get_info());
    let pool = pool();
    let mut registry = ResourceRegistry::new(2, 64 << 10).unwrap();
    let version = |n| ResourceVersion {
        key: AssetKey(format!("test-{n}")),
        version: ContentVersion([n; 32]),
    };
    let bytes = OwnedBytes::new(vec![128, 128, 128, 128], pool.reserve(4).unwrap()).unwrap();
    let h = registry
        .register_image(
            version(1),
            bytes,
            [1, 1],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            reel_ui::resources::ColorEncoding::Srgb,
        )
        .unwrap();
    let lease = registry.acquire(h).unwrap();
    let weak = lease.downgrade();
    let image = test_image_data(&lease);
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Image(ImageBrush::new(image.clone())),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., 400., 300.)),
        composite: Default::default(),
    });
    let mut bridge = widget_bridge(RecordWidget::new(scene), Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[h], &pool).unwrap(),
        &pool,
    );
    let prepared = renderer.prepare(&frame).unwrap();
    registry.retire(h);
    drop(lease);
    drop(bridge);
    drop(frame);
    assert!(!weak.is_released());
    let pixels = renderer.readback(prepared).unwrap();
    let expected = (color::srgb_to_linear(128. / 255.) * 128. / 255.) as f32;
    close(
        pixel(&pixels, 100, 100),
        [expected, expected, expected, 128. / 255.],
    );
    assert!(weak.is_released());

    let data =
        std::fs::read(std::env::var("REEL_R01_TEST_FONT").expect("provide a real font")).unwrap();
    let bytes = data.len();
    let font = Blob::from(data);
    let handle = registry
        .register(
            version(2),
            ResourceData::Font(font.clone()),
            pool.reserve(bytes as u64).unwrap(),
        )
        .unwrap();
    for (text, font_size) in [("office ffi Reel 25", 24.), ("R", 400.)] {
        let root = RenderRoot::new(
            reel_ui::widgets::Label::new(text)
                .with_style(StyleProperty::FontSize(font_size))
                .prepare(),
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(400, 300),
                scale_factor: 1.,
                default_font: Some(font.clone()),
            },
        );
        let mut bridge = r01_fixture::bridge(root);
        let frame = build(
            bridge.record(FrameId(2)).unwrap(),
            registry.freeze(&[handle], &pool).unwrap(),
            &pool,
        );
        let count: usize = frame
            .commands()
            .iter()
            .filter_map(|c| match &c.op {
                reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::GlyphRun(run)) => {
                    assert_eq!(run.font_size, font_size);
                    Some(run.glyphs.len())
                }
                _ => None,
            })
            .sum();
        assert!(count > 0);
        let pixels = renderer
            .readback(renderer.prepare(&frame).unwrap())
            .unwrap();
        let covered = pixels.iter().filter(|p| p[3] > 0.1).count();
        assert!(
            covered > 100,
            "{covered} covered pixels for {count} original Parley glyphs at size {font_size}"
        );
        eprintln!("Parley glyphs={count}, size={font_size}; Swash covered pixels={covered}");
    }
}

#[test]
#[ignore = "requires an actual graphics adapter"]
fn explicit_quality_downgrade_and_device_loss() {
    let instance = wgpu::Instance::default();
    for (budget, expected_format) in [
        (1 << 20, Some(wgpu::TextureFormat::Rgba16Float)),
        (600_000, Some(wgpu::TextureFormat::Rgba8Unorm)),
        (100, None),
    ] {
        let gpu = pollster::block_on(Gpu::request(
            &instance,
            None,
            DeviceGeneration(1),
            GpuLimits {
                target_bytes: budget,
                ..Default::default()
            },
        ))
        .unwrap();
        if let Some(format) = expected_format {
            let quality = gpu.quality([400, 300]).unwrap();
            assert_eq!(quality.samples, 1);
            assert_eq!(quality.format, format);
            assert!(quality.degraded);
        } else {
            assert!(gpu.quality([400, 300]).is_err());
        }
    }
    let renderer = device();
    renderer.gpu().device.destroy();
    let _ = renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely());
    assert!(matches!(
        renderer.gpu().check(),
        Err(reel_render::gpu::GpuError::DeviceLost(_))
    ));
    let replacement = device();
    assert!(replacement.gpu().check().is_ok());
}

#[test]
#[ignore = "requires an actual graphics adapter"]
fn localized_refusal_and_foreign_prepared_device() {
    let mut renderer = device();
    let pool = pool();
    let mut s = Scene::new();
    s.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::WHITE),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., 20., 20.)),
        composite: Composite {
            blend: reel_ui::peniko::BlendMode::new(
                reel_ui::peniko::Mix::Multiply,
                reel_ui::peniko::Compose::SrcOver,
            ),
            alpha: 1.,
        },
    });
    let mut bridge = widget_bridge(RecordWidget::new(s), Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    match renderer.prepare(&frame) {
        Err(reel_render::gpu::GpuError::Unsupported { at, record }) => {
            assert_eq!(record, "blend mode");
            assert_eq!(
                at,
                frame
                    .commands()
                    .iter()
                    .find(|c| matches!(c.op, reel_ui::frame::FrameOp::Draw(_)))
                    .unwrap()
                    .origin
                    .at
            );
        }
        _ => panic!("unsupported blend must refuse the entire frame"),
    }
    assert_eq!(renderer.usage().targets, 0);
    assert!(bridge.accepted_key().is_none());
    let mut bridge = widget_bridge(RecordWidget::new(scene(Color::WHITE)), Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(2)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let other = device();
    let prepared = other.prepare(&frame).unwrap();
    assert!(renderer.submit(prepared).is_err());
    assert_eq!(other.usage().targets, 0);
}

#[test]
#[ignore = "requires an actual graphics adapter"]
fn transformed_path_clip_has_geometric_coverage() {
    use reel_ui::{
        imaging::record::Clip,
        kurbo::{BezPath, Stroke},
    };
    let mut renderer = device();
    let pool = pool();
    let mut path = BezPath::new();
    path.move_to((0., 0.));
    path.line_to((100., 0.));
    path.line_to((0., 100.));
    path.close_path();
    let mut s = Scene::new();
    s.push_clip(Clip::Fill {
        transform: Affine::translate((50., 50.)) * Affine::scale_non_uniform(2., 1.),
        shape: Geometry::Path(path),
        fill_rule: Default::default(),
    });
    fill(&mut s, Color::WHITE);
    s.pop_clip();
    let mut bridge = widget_bridge(RecordWidget::new(s), Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let pixels = renderer
        .readback(renderer.prepare(&frame).unwrap())
        .unwrap();
    close(pixel(&pixels, 75, 75), [1.; 4]);
    close(pixel(&pixels, 220, 140), [0.; 4]);
    close(pixel(&pixels, 30, 30), [0.; 4]);

    // Kurbo permits each edge type immediately after ClosePath. The implicit
    // restart must have the same pixels as an explicit MoveTo(first), including
    // the final unclosed curve whose End event is supplied at iterator exhaustion.
    let mut rendered = Vec::new();
    for explicit in [false, true] {
        let mut path = BezPath::new();
        path.move_to((100., 100.));
        path.line_to((150., 100.));
        path.line_to((150., 80.));
        path.close_path();
        if explicit {
            path.move_to((100., 100.));
        }
        path.line_to((100., 160.));
        path.close_path();
        if explicit {
            path.move_to((100., 100.));
        }
        path.quad_to((160., 160.), (180., 100.));
        path.close_path();
        if explicit {
            path.move_to((100., 100.));
        }
        path.curve_to((180., 180.), (180., 240.), (100., 240.));
        let mut scene = Scene::new();
        scene.draw(Draw::Stroke {
            transform: Affine::IDENTITY,
            shape: Geometry::Path(path),
            stroke: Stroke::new(10.),
            brush: Brush::Solid(Color::WHITE),
            brush_transform: None,
            composite: Composite::default(),
        });
        let mut bridge = widget_bridge(RecordWidget::new(scene), Affine::IDENTITY);
        let frame = build(
            bridge.record(FrameId(2 + u64::from(explicit))).unwrap(),
            empty_snapshot(&pool),
            &pool,
        );
        rendered.push(
            renderer
                .readback(renderer.prepare(&frame).unwrap())
                .unwrap(),
        );
    }
    close(pixel(&rendered[0], 100, 130), [1.; 4]);
    assert_eq!(rendered[0], rendered[1]);
}

#[test]
#[ignore = "requires an actual graphics adapter"]
fn rgba_encodings_and_alpha_modes_match_independent_linear_pixels() {
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::{
        peniko::{ImageAlphaType, ImageBrush, ImageFormat},
        resources::{ColorEncoding, ResourceRegistry},
    };
    let mut renderer = device();
    let pool = pool();
    for encoding in [ColorEncoding::Srgb, ColorEncoding::Linear] {
        for alpha_type in [ImageAlphaType::Alpha, ImageAlphaType::AlphaPremultiplied] {
            let mut bytes = Vec::new();
            let mut expected = Vec::new();
            let mut scene = Scene::new();
            for n in 0..=255u8 {
                let mut channels = [n, 255 - n, 128, n];
                if alpha_type == ImageAlphaType::AlphaPremultiplied {
                    for c in &mut channels[..3] {
                        *c = (u16::from(*c) * u16::from(n) / 255) as u8;
                    }
                }
                if n == 0 {
                    channels = [255, 128, 64, 0];
                }
                bytes.extend(channels);
                let a = f64::from(n) / 255.;
                let mut linear = [0.; 4];
                for i in 0..3 {
                    let c = f64::from(channels[i]) / 255.;
                    let straight = if alpha_type == ImageAlphaType::AlphaPremultiplied && a > 0. {
                        c / a
                    } else {
                        c
                    };
                    linear[i] = if encoding == ColorEncoding::Srgb {
                        color::srgb_to_linear(straight) * a
                    } else {
                        straight * a
                    };
                }
                linear[3] = a;
                expected.push(linear.map(|c| c as f32));
                // Solids exercise the same library through the drawing path.
                scene.draw(Draw::Fill {
                    transform: Affine::IDENTITY,
                    fill_rule: Default::default(),
                    brush: Brush::Solid(Color::from_rgba8(n, 255 - n, 128, n)),
                    brush_transform: None,
                    shape: Geometry::Rect(Rect::new(f64::from(n), 2., f64::from(n) + 1., 3.)),
                    composite: Composite::default(),
                });
            }
            let mut registry = ResourceRegistry::new(1, 8192).unwrap();
            let size = bytes.len();
            let handle = registry
                .register_image(
                    ResourceVersion {
                        key: AssetKey("color-matrix".into()),
                        version: ContentVersion([1; 32]),
                    },
                    OwnedBytes::new(bytes, pool.reserve(size as u64).unwrap()).unwrap(),
                    [256, 1],
                    ImageFormat::Rgba8,
                    alpha_type,
                    encoding,
                )
                .unwrap();
            let lease = registry.acquire(handle).unwrap();
            let image = test_image_data(&lease);
            scene.draw(Draw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Image(ImageBrush::new(image.clone())),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., 0., 256., 1.)),
                composite: Composite::default(),
            });
            let mut bridge = widget_bridge(RecordWidget::new(scene), Affine::IDENTITY);
            let frame = build(
                bridge.record(FrameId(1)).unwrap(),
                registry.freeze(&[handle], &pool).unwrap(),
                &pool,
            );
            let prepared = renderer.prepare(&frame).unwrap();
            let pixels = renderer.readback(prepared).unwrap();
            for (x, expected) in expected.into_iter().enumerate() {
                close(pixel(&pixels, x, 0), expected);
                let n = x as f64 / 255.;
                close(
                    pixel(&pixels, x, 2),
                    color::linear_premultiplied([n, 1. - n, 128. / 255., n]).map(|c| c as f32),
                );
            }
            assert!(!renderer.has_pending());
            assert_eq!(renderer.usage().cached_images, 256 * 8);
            assert!(registry.retire(handle));
            drop(frame);
            drop(bridge);
            drop(lease);
            renderer.poll().unwrap();
            assert_eq!(renderer.usage().cached_images, 0);
            assert_eq!(renderer.usage().retired_image_readers, 0);
        }
    }
}

fn retained_picture(
    frame: &reel_ui::frame::RenderFrame,
    root: Option<reel_ui::core::WidgetId>,
    pool: &reel_core::budget::BudgetPool,
) -> reel_ui::frame::RetainedScene {
    let mut build = reel_ui::frame::RetainBuild::new(frame, root, limits(), pool).unwrap();
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
}
fn transition_frame(
    source: reel_ui::bridge::RecordedSource,
    transitions: Vec<reel_ui::frame::FrameTransition>,
    pool: &reel_core::budget::BudgetPool,
) -> reel_ui::frame::RenderFrame {
    use reel_ui::frame::*;
    let mut builder = FrameBuild::new(
        source,
        empty_snapshot(pool),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        pool,
    )
    .unwrap();
    builder.set_transitions(transitions).unwrap();
    while !builder.step(step_budget()).unwrap().complete {}
    builder.finish().unwrap()
}
#[test]
#[ignore = "requires an actual graphics adapter"]
fn retained_transition_uses_current_parent_backdrop_and_replaces_linear_pixels() {
    use reel_ui::{
        core::Widget,
        frame::FrameTransition,
        kurbo::{Point, Size},
        peniko::{BlendMode, Compose, Mix},
    };
    let mut renderer = device();
    for blend in [
        BlendMode::default(),
        BlendMode::new(Mix::Normal, Compose::PlusLighter),
        BlendMode::new(Mix::Multiply, Compose::SrcOver),
        BlendMode::new(Mix::Screen, Compose::SrcOver),
    ] {
        let pool = pool();
        let tree = |color| {
            let mut parent = RecordWidget::new(scene(Color::from_rgba8(80, 140, 200, 180)));
            parent.opacity = 0.7;
            let mut child = RecordWidget::new(scene(color));
            child.blend = blend.into();
            child.opacity = 0.6;
            let id = parent.inner.add(
                child.prepare().with_transform(Affine::rotate(0.15)),
                Point::new(70., 55.),
                Size::new(140., 95.),
            );
            (
                widget_bridge(parent, Affine::translate((4., 8.)) * Affine::scale(0.9)),
                id,
            )
        };
        let (mut old_bridge, old_id) = tree(Color::from_rgba8(255, 40, 30, 150));
        let old = build(
            old_bridge.record(FrameId(1)).unwrap(),
            empty_snapshot(&pool),
            &pool,
        );
        let old_pixels = renderer.readback(renderer.prepare(&old).unwrap()).unwrap();
        let picture = retained_picture(&old, Some(old_id), &pool);
        drop(old);
        drop(old_bridge);
        let (mut new_bridge, new_id) = tree(Color::from_rgba8(20, 255, 150, 110));
        let new = build(
            new_bridge.record(FrameId(1)).unwrap(),
            empty_snapshot(&pool),
            &pool,
        );
        let new_pixels = renderer.readback(renderer.prepare(&new).unwrap()).unwrap();
        drop(new);
        for (i, progress) in [0., 0.25, 0.5, 1.].into_iter().enumerate() {
            let frame = transition_frame(
                new_bridge.record(FrameId(2 + i as u64)).unwrap(),
                vec![FrameTransition {
                    root: Some(new_id),
                    previous: picture.clone(),
                    progress,
                    mask: None,
                }],
                &pool,
            );
            let pixels = read_frame(&mut renderer, &frame);
            for (index, pixel) in pixels.into_iter().enumerate() {
                let expected = std::array::from_fn(|channel| {
                    old_pixels[index][channel] * (1. - progress)
                        + new_pixels[index][channel] * progress
                });
                close(pixel, expected);
            }
        }
    }
}
#[test]
#[ignore = "requires an actual graphics adapter"]
fn retained_transition_mask_endpoints_and_nested_midpoint_survive_new_frames() {
    use reel_ui::{
        core::PaintImageMask,
        frame::FrameTransition,
        stage::{MaskOp, MaskProgram},
    };
    let pool = pool();
    let mut renderer = device();
    let mut old_bridge = widget_bridge(
        RecordWidget::new(scene(Color::from_rgba8(255, 0, 0, 128))),
        Affine::IDENTITY,
    );
    let old = build(
        old_bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let picture = retained_picture(&old, None, &pool);
    drop(old);
    drop(old_bridge);
    let mut current = widget_bridge(
        RecordWidget::new(scene(Color::from_rgba8(0, 0, 255, 200))),
        Affine::IDENTITY,
    );
    let mask = PaintImageMask {
        program: MaskProgram::new(vec![MaskOp::Constant(0.25_f32.to_bits())], vec![]).unwrap(),
        images: std::sync::Arc::from([]),
        bounds: Rect::new(0., 0., 400., 300.),
        parameters: [0.5, 0.06, 0., 0., 0.],
    };
    for (i, progress) in [0., 0.5, 1.].into_iter().enumerate() {
        let frame = transition_frame(
            current.record(FrameId(i as u64 + 1)).unwrap(),
            vec![FrameTransition {
                root: None,
                previous: picture.clone(),
                progress,
                mask: Some(mask.clone()),
            }],
            &pool,
        );
        let pixels = read_frame(&mut renderer, &frame);
        let coverage = if progress == 0. || progress == 1. {
            progress
        } else {
            0.25
        };
        close(
            pixel(&pixels, 100, 100),
            [
                128. / 255. * (1. - coverage),
                0.,
                200. / 255. * coverage,
                (128. * (1. - coverage) + 200. * coverage) / 255.,
            ],
        );
        if progress == 0.5 {
            let middle = retained_picture(&frame, None, &pool);
            drop(frame);
            let mut green = widget_bridge(
                RecordWidget::new(scene(Color::from_rgb8(0, 255, 0))),
                Affine::IDENTITY,
            );
            let nested = transition_frame(
                green.record(FrameId(1)).unwrap(),
                vec![FrameTransition {
                    root: None,
                    previous: middle,
                    progress: 0.5,
                    mask: None,
                }],
                &pool,
            );
            let pixels = read_frame(&mut renderer, &nested);
            close(
                pixel(&pixels, 100, 100),
                [
                    128. / 255. * 0.75 * 0.5,
                    0.5,
                    200. / 255. * 0.25 * 0.5,
                    ((128. * 0.75 + 200. * 0.25) / 255. + 1.) * 0.5,
                ],
            );
        }
    }
}

#[test]
#[ignore = "requires an actual graphics adapter"]
fn retained_transition_hidden_target_keeps_empty_subtree_boundary() {
    use reel_ui::{
        core::Widget,
        frame::FrameTransition,
        kurbo::{Point, Size},
    };
    let pool = pool();
    let mut renderer = device();
    let mut parent = RecordWidget::new(scene(Color::from_rgb8(0, 0, 255)));
    let child = parent.inner.add(
        RecordWidget::new(scene(Color::from_rgb8(255, 0, 0))).prepare(),
        Point::new(50., 40.),
        Size::new(100., 90.),
    );
    let parent = parent.prepare();
    let parent_id = parent.id();
    let mut bridge = r01_fixture::bridge(fixture::root(parent).0);
    let old = build(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let picture = retained_picture(&old, Some(child), &pool);
    drop(old);
    bridge.edit_widget(parent_id, |mut node| {
        let mut node = node.downcast::<RecordWidget>();
        node.ctx
            .set_stashed(&mut node.widget.inner.children[0].0, true);
    });
    let hidden = transition_frame(
        bridge.record(FrameId(2)).unwrap(),
        vec![FrameTransition {
            root: Some(child),
            previous: picture,
            progress: 0.5,
            mask: None,
        }],
        &pool,
    );
    let pixels = read_frame(&mut renderer, &hidden);
    close(pixel(&pixels, 60, 60), [0.5, 0., 0.5, 1.]);
    close(pixel(&pixels, 20, 20), [0., 0., 1., 1.]);
    let empty = retained_picture(&hidden, Some(child), &pool);
    assert!(empty.drawing().commands().is_empty());
    // The accepted intermediate content is retained through its nested transition.
    assert_eq!(empty.drawing().transitions().len(), 1);
}

fn test_image_data(lease: &reel_ui::resources::ResourceLease) -> reel_ui::peniko::ImageData {
    let image = lease.image_descriptor().unwrap();
    reel_ui::peniko::ImageData {
        data: lease.read_pixels().unwrap().blob(),
        format: image.format,
        alpha_type: image.alpha_type,
        width: image.width,
        height: image.height,
    }
}
