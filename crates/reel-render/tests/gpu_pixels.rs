#![cfg(feature = "wgpu")]
#[allow(dead_code, reason = "shared actual Masonry fixture")]
#[path = "../../reel-stage/tests/support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared R01 compiler fixture")]
#[path = "../../reel-stage/tests/support/r01_fixture.rs"]
mod r01_fixture;
use masonry_core::{
    imaging::{
        Composite,
        record::{Draw, Geometry, Scene},
    },
    kurbo::{Affine, Rect},
    peniko::{Brush, Color},
};
use r01_fixture::*;
use reel_core::identity::FrameId;
use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
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
    assert!(renderer.usage().targets > 0);
    let pixels = renderer.readback(prepared).unwrap();
    let p = pixels[100 * 400 + 100];
    assert!((p[0] - 128. / 255. * 0.5).abs() < 0.005, "{p:?}");
    assert!((p[2] - 0.5).abs() < 0.005, "{p:?}");
    assert!((p[3] - (128. / 255. + 1.) * 0.5).abs() < 0.005, "{p:?}");
    assert_eq!(renderer.usage().targets, 0);
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
#[test]
#[ignore = "requires an actual graphics adapter"]
fn whole_group_opacity_and_nested_masks_clips() {
    use masonry_core::imaging::{
        MaskMode,
        record::{AppliedMask, Clip, Group, Mask},
    };
    let mut renderer = device();
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
    let mask = s.define_mask(Mask::new(MaskMode::Luminance, coverage));
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
    renderer.gpu().device.poll(wgpu::PollType::Wait).unwrap();
    // Driver completion alone cannot release application ownership until poll observes it.
    assert!(renderer.usage().targets > 0);
    assert_eq!(renderer.poll().unwrap(), vec![id]);
    assert_eq!(renderer.usage().targets, 0);
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
#[ignore = "requires adapter and REEL_R01_TEST_FONT"]
fn image_linear_upload_retirement_and_actual_parley_glyphs() {
    use masonry_core::{
        app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
        core::{DefaultProperties, Widget},
        dpi::PhysicalSize,
        peniko::{Blob, FontData, ImageAlphaType, ImageBrush, ImageFormat},
    };
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_stage::{ResourceData, ResourceRegistry};
    use std::sync::Arc;
    let mut renderer = device();
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
            reel_stage::ColorEncoding::Srgb,
        )
        .unwrap();
    let lease = registry.acquire(h).unwrap();
    let weak = lease.downgrade();
    let ResourceData::Image { image, .. } = lease.data() else {
        unreachable!()
    };
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
    let expected = (reel_render::color::srgb_to_linear(128. / 255.) * 128. / 255.) as f32;
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
            ResourceData::Font(FontData::new(font.clone(), 0)),
            pool.reserve(bytes as u64).unwrap(),
        )
        .unwrap();
    let root = RenderRoot::new(
        masonry::widgets::Label::new("office ffi Reel 25").prepare(),
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(400, 300),
            scale_factor: 1.,
            test_font: Some(font),
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
            reel_stage::FrameOp::Draw(reel_stage::FrameDraw::GlyphRun(run)) => {
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
        "{covered} covered pixels for {count} original Parley glyphs"
    );
    eprintln!("Parley glyphs={count}; Swash covered pixels={covered}");
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
    let _ = renderer.gpu().device.poll(wgpu::PollType::Wait);
    assert!(matches!(
        renderer.gpu().check(),
        Err(reel_render::gpu::GpuError::DeviceLost)
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
            blend: masonry_core::peniko::BlendMode::new(
                masonry_core::peniko::Mix::Multiply,
                masonry_core::peniko::Compose::SrcOver,
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
                    .find(|c| matches!(c.op, reel_stage::FrameOp::Draw(_)))
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
    use masonry_core::{imaging::record::Clip, kurbo::BezPath};
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
}
