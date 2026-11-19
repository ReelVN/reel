//! Real resource device-call counts and preparation ownership regressions.
use super::*;
use crate::test_support::r01_fixture;
use r01_fixture::*;
use reel_core::identity::FrameId;
use reel_ui::frame::{FrameBuild, FrameInfo};
use reel_ui::{
    imaging::{
        Composite,
        record::{Clip, Draw, Geometry, Group, Scene},
    },
    kurbo::{Affine, Rect, Shape},
    peniko::{Brush, Color},
};
use std::time::Instant;

mod atlas;
mod captures;
mod effects;
mod nine_slice;
mod pipelines;
mod textures;

mod synthesis;

mod glyph_gradient;

fn renderer(limits: crate::gpu::GpuLimits) -> Renderer {
    Renderer::new(
        pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            crate::gpu::DeviceGeneration(1),
            limits,
        ))
        .expect("actual adapter required"),
    )
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
fn workload(solids: usize, clips: usize, color: Color) -> Scene {
    let mut scene = Scene::new();
    for _ in 0..solids {
        fill(&mut scene, color);
    }
    for _ in 0..clips {
        scene.push_clip(Clip::Fill {
            transform: Affine::IDENTITY,
            // Exercise coverage textures independently of rectangular scissor clips.
            shape: Geometry::Path(Rect::new(0., 0., 400., 300.).to_path(0.1)),
            fill_rule: Default::default(),
        });
        scene.pop_clip();
    }
    scene
}
fn frame(scene: Scene, pool: &BudgetPool) -> RenderFrame {
    frame_with_snapshot(scene, empty_snapshot(pool), pool)
}
fn frame_with_snapshot(
    scene: Scene,
    snapshot: reel_ui::resources::ResourceSnapshot,
    pool: &BudgetPool,
) -> RenderFrame {
    frame_from_widget(RecordWidget::new(scene), snapshot, pool)
}
fn frame_from_widget(
    mut widget: RecordWidget,
    snapshot: reel_ui::resources::ResourceSnapshot,
    pool: &BudgetPool,
) -> RenderFrame {
    widget.clip = false;
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        snapshot,
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [64, 48],
        },
        limits(),
        pool,
    )
    .unwrap();
    while !build.step(step_budget()).unwrap().complete {}
    build.finish().unwrap()
}
fn counts(renderer: &Renderer, textures: usize) {
    let white = usize::from(renderer.white.lock().unwrap().is_some());
    assert_eq!(
        renderer.gpu.upload_creations.snapshot(),
        [textures, textures - white, textures - white]
    );
}

fn moving_triangle_fixture() -> (
    reel_ui::bridge::PresentationBridge,
    reel_ui::core::WidgetId,
    BudgetPool,
) {
    use reel_ui::{
        core::Widget,
        kurbo::{BezPath, Point, Size},
    };
    let mut parent = RecordWidget::new(Scene::new());
    parent.clip = false;
    let mut middle = None;
    for index in 0..3 {
        let mut path = BezPath::new();
        path.move_to((0., 0.));
        path.line_to((20., 0.));
        path.line_to((0., 20.));
        path.close_path();
        let mut scene = Scene::new();
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(Color::WHITE),
            brush_transform: None,
            shape: Geometry::Path(path),
            composite: Default::default(),
        });
        let mut child = RecordWidget::new(scene);
        child.clip = false;
        let id = parent.inner.add(
            child.prepare(),
            Point::new(f64::from(index * 30), 0.),
            Size::new(20., 20.),
        );
        if index == 1 {
            middle = Some(id);
        }
    }
    (
        widget_bridge(parent, Affine::IDENTITY),
        middle.unwrap(),
        pool(),
    )
}

fn long_text_fixture() -> (
    BudgetPool,
    reel_ui::resources::ResourceRegistry,
    reel_ui::resources::ResourceHandle,
    RenderFrame,
    reel_ui::bridge::PresentationBridge,
) {
    use reel_ui::{
        app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
        core::*,
        dpi::PhysicalSize,
        peniko::Blob,
        resources::{ResourceData, ResourceRegistry},
    };
    let pool = pool();
    let bytes = std::fs::read(std::env::var("REEL_R01_TEST_FONT").unwrap()).unwrap();
    let reservation = pool.reserve(bytes.len() as u64).unwrap();
    let font = Blob::from(bytes);
    let mut registry = ResourceRegistry::new(4, 65264).unwrap();
    let handle = registry
        .register(
            reel_core::identity::ResourceVersion {
                key: reel_core::identity::AssetKey("atlas-font".into()),
                version: reel_core::identity::ContentVersion([1; 32]),
            },
            ResourceData::Font(font.clone()),
            reservation,
        )
        .unwrap();
    let text = "Chat history: office ffi Reel 0123456789. ".repeat(160);
    let root = RenderRoot::new(
        NewWidget::new(
            reel_ui::widgets::Label::new(text)
                // Keep thousands of painted glyphs in the viewport now that Label
                // culls offscreen lines. Preserve the atlas/batching workload.
                .with_style(StyleProperty::FontSize(8.))
                .with_style(StyleProperty::FontStyle(
                    reel_ui::reel_text::FontStyle::Italic,
                ))
                .with_outline(Some(TextOutline {
                    width: 1.,
                    color: Color::from_rgb8(255, 0, 0),
                })),
        )
        .with_props((
            reel_ui::properties::LineBreaking::WordWrap,
            reel_ui::properties::ContentColor::new(Color::WHITE),
        )),
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(400, 300),
            scale_factor: 1.,
            default_font: Some(font),
        },
    );
    let mut bridge = r01_fixture::bridge(root);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[handle], &pool).unwrap(),
        &pool,
    );
    (pool, registry, handle, frame, bridge)
}

fn usage(renderer: &Renderer) -> [u64; 3] {
    let u = renderer.usage();
    [u.targets, u.resources, u.transient]
}

fn empty(renderer: &Renderer) {
    renderer.targets.clear();
    renderer.geometry.clear();
    renderer.images.clear_idle();
    let white = renderer.white.lock().unwrap().is_some();
    assert_eq!(usage(renderer), [0, u64::from(white) * 8, 0]);
    assert_eq!(renderer.admission.usage().tickets, 0);
    assert_eq!(renderer.budgets.resources.usage().tickets, u32::from(white));
}

#[test]
fn white_texture_upload_extent_cost() {
    assert_eq!(crate::gpu::resources::upload_cost([1, 1]).unwrap(), 256 + 8);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn white_texture_lazy_counts_and_crossfade_pixels() {
    let pool = pool();
    let mut renderer = renderer(Default::default());
    let mut unused = Scene::new();
    unused.push_group(Group::default());
    unused.pop_group();
    drop(renderer.prepare(&frame(unused, &pool)).unwrap());
    counts(&renderer, 0);
    empty(&renderer);
    for (solids, clips) in [(100, 0), (0, 100), (50, 50)] {
        let prepared = renderer
            .prepare(&frame(workload(solids, clips, Color::WHITE), &pool))
            .unwrap();
        counts(&renderer, 1);
        assert_eq!(renderer.usage().resources, 8);
        assert_eq!(prepared.budget_scope.resources.usage().bytes, 8);
        drop(prepared);
        empty(&renderer);
    }
    let red = frame(workload(50, 50, Color::from_rgb8(255, 0, 0)), &pool);
    let blue = frame(workload(50, 50, Color::from_rgb8(0, 0, 255)), &pool);
    let prepared = renderer.crossfade(&red, &blue, 0.25).unwrap();
    counts(&renderer, 1);
    assert_eq!(renderer.usage().resources, 8);
    let pixels = renderer.readback(prepared).unwrap();
    assert!(pixels.iter().all(|p| {
        p.iter()
            .zip([0.75, 0., 0.25, 1.])
            .all(|(a, b)| (*a - b).abs() < 0.006)
    }));
    empty(&renderer);

    use reel_ui::imaging::{
        MaskMode,
        record::{AppliedMask, Mask},
    };
    let mut masked = Scene::new();
    let mask = masked.define_mask(Mask::new(MaskMode::Alpha, workload(50, 10, Color::WHITE)));
    masked.push_group(Group {
        mask: Some(AppliedMask::new(mask)),
        ..Default::default()
    });
    fill(&mut masked, Color::WHITE);
    masked.pop_group();
    let prepared = renderer.prepare(&frame(masked, &pool)).unwrap();
    counts(&renderer, 1);
    assert_eq!(renderer.usage().resources, 8);
    let pixels = renderer.readback(prepared).unwrap();
    assert!(
        pixels
            .iter()
            .all(|p| p.iter().all(|c| (*c - 1.).abs() < 0.006))
    );
    empty(&renderer);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn white_texture_reuses_initialized_source_after_cancellation() {
    let pool = pool();
    let mut renderer = renderer(crate::gpu::GpuLimits {
        resource_bytes: 8,
        ..Default::default()
    });
    let solid = frame(workload(1, 0, Color::WHITE), &pool);
    let first = renderer.prepare(&solid).unwrap();
    assert_eq!(first.budget_scope.resources.usage().bytes, 8);
    drop(first);
    counts(&renderer, 1);
    assert_eq!(renderer.usage().resources, 8);
    assert_eq!(renderer.admission.usage().tickets, 0);

    // The gradient shader reads the initialized texel after the first
    // preparation is abandoned before its frame commands reach the queue.
    let mut gradient = Scene::new();
    gradient.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Gradient(
            reel_ui::peniko::Gradient::new_linear((0., 0.), (400., 0.))
                .with_stops([(0., Color::WHITE), (1., Color::WHITE)]),
        ),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., 400., 300.)),
        composite: Composite::default(),
    });
    let prepared = renderer.prepare(&frame(gradient, &pool)).unwrap();
    assert_eq!(prepared.budget_scope.resources.usage().bytes, 8);
    assert!(
        renderer
            .readback(prepared)
            .unwrap()
            .iter()
            .flatten()
            .all(|component| (*component - 1.).abs() < 0.006)
    );
    counts(&renderer, 1);
    assert_eq!(renderer.usage().resources, 8);
    assert_eq!(renderer.budgets.resources.usage().tickets, 1);
    assert_eq!(renderer.admission.usage().tickets, 0);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn white_texture_releases_residency_under_pressure_after_gpu_completion() {
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::{
        peniko::{ImageAlphaType, ImageBrush, ImageFormat},
        resources::{ColorEncoding, ResourceRegistry},
    };
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 65264).unwrap();
    let handle = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("translucent.png".into()),
                version: ContentVersion([1; 32]),
            },
            OwnedBytes::new(vec![255, 0, 0, 128], pool.reserve(4).unwrap()).unwrap(),
            [1, 1],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let image = crate::test_support::image_data(&registry.acquire(handle).unwrap());
    let mut picture = Scene::new();
    picture.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Image(ImageBrush::new(image)),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., 400., 300.)),
        composite: Composite::default(),
    });
    let picture = frame_with_snapshot(picture, registry.freeze(&[handle], &pool).unwrap(), &pool);
    let solid = frame(workload(1, 0, Color::WHITE), &pool);
    let mut renderer = renderer(crate::gpu::GpuLimits {
        resource_bytes: 256 + 8,
        ..Default::default()
    });
    let held = renderer.prepare(&solid).unwrap();
    assert!(matches!(
        renderer.prepare(&picture),
        Err(GpuError::Pressure { .. })
    ));
    let submitted = renderer.submit(held).unwrap();
    assert!(matches!(
        renderer.prepare(&picture),
        Err(GpuError::Pressure { .. })
    ));
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert_eq!(renderer.poll().unwrap(), [submitted]);

    let prepared = renderer.prepare(&picture).unwrap();
    assert!(renderer.white.lock().unwrap().is_none());
    assert_eq!(renderer.usage().resources, 264);
    assert_eq!(
        prepared.images[&handle].residency.as_ref().unwrap().bytes(),
        8
    );
    let pixels = renderer.readback(prepared).unwrap();
    assert!(pixels.iter().all(|pixel| {
        pixel
            .iter()
            .zip([128. / 255., 0., 0., 128. / 255.])
            .all(|(actual, expected)| (*actual - expected).abs() < 0.002)
    }));
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn white_texture_image_only_and_mixed_resources_are_independent() {
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::{
        peniko::{ImageAlphaType, ImageBrush, ImageFormat},
        resources::{ColorEncoding, ResourceRegistry},
    };
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 65264).unwrap();
    let handle = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("white-test-image".into()),
                version: ContentVersion([1; 32]),
            },
            OwnedBytes::new(vec![255; 4], pool.reserve(4).unwrap()).unwrap(),
            [1, 1],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let lease = registry.acquire(handle).unwrap();
    let image = crate::test_support::image_data(&lease);
    let mut image_scene = Scene::new();
    image_scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Image(ImageBrush::new(image.clone())),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., 400., 300.)),
        composite: Composite::default(),
    });
    let image_only = frame_with_snapshot(
        image_scene.clone(),
        registry.freeze(&[handle], &pool).unwrap(),
        &pool,
    );
    let mut mixed = image_scene;
    for _ in 0..100 {
        fill(&mut mixed, Color::WHITE);
    }
    let mixed = frame_with_snapshot(mixed, registry.freeze(&[handle], &pool).unwrap(), &pool);
    let mut renderer = renderer(Default::default());
    let p = renderer.prepare(&image_only).unwrap();
    counts(&renderer, 1); // One image upload; an unused white slot creates nothing.
    assert_eq!(renderer.usage().resources, 264);
    drop(p);
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    assert_eq!(renderer.usage().resources, 8);
    let p = renderer.prepare(&mixed).unwrap();
    counts(&renderer, 2);
    assert_eq!(renderer.usage().resources, 16);
    let pixels = renderer.readback(p).unwrap();
    assert!(
        pixels
            .iter()
            .all(|p| p.iter().all(|c| (*c - 1.).abs() < 0.006))
    );
    renderer.targets.clear();
    assert_eq!(usage(&renderer), [0, 16, 0]);
    let cached = renderer.prepare(&image_only).unwrap();
    counts(&renderer, 2);
    assert_eq!(usage(&renderer)[1], 16);
    drop(cached);
    renderer.retire(std::slice::from_ref(lease.version()));
    empty(&renderer);

    let mut limited = self::renderer(crate::gpu::GpuLimits {
        resource_bytes: 264,
        ..Default::default()
    });
    let prepared = limited.prepare(&image_only).unwrap();
    limited.readback(prepared).unwrap();
    let held = limited.prepare(&image_only).unwrap();
    let other_residency = limited.budgets.resources.reserve(256).unwrap();
    let solid = frame(workload(1, 0, Color::WHITE), &pool);
    assert!(matches!(
        limited.prepare(&solid),
        Err(GpuError::Pressure { .. })
    ));
    drop(held);
    let prepared = limited
        .prepare(&solid)
        .expect("unused image residency is evictable for non-image uploads too");
    limited.readback(prepared).unwrap();
    drop(other_residency);
    empty(&limited);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn rectangle_clips_share_targets_and_preserve_group_opacity() {
    let pool = pool();
    let mut scene = Scene::new();
    fill(&mut scene, Color::from_rgb8(255, 0, 0));
    for rect in [
        Rect::new(0., 0., 400., 300.),
        Rect::new(100., 50., 300., 250.),
    ] {
        scene.push_clip(Clip::Fill {
            transform: Affine::IDENTITY,
            shape: Geometry::Rect(rect),
            fill_rule: Default::default(),
        });
    }
    scene.push_group(Group {
        composite: Composite {
            alpha: 0.5,
            ..Default::default()
        },
        ..Default::default()
    });
    fill(&mut scene, Color::from_rgb8(0, 0, 255));
    scene.push_clip(Clip::Fill {
        transform: Affine::IDENTITY,
        shape: Geometry::Rect(Rect::new(150., 100., 250., 200.)),
        fill_rule: Default::default(),
    });
    fill(&mut scene, Color::from_rgb8(0, 255, 0));
    scene.pop_clip();
    scene.pop_group();
    scene.pop_clip();
    scene.pop_clip();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::WHITE),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., 50., 50.)),
        composite: Composite::default(),
    });
    let mut renderer = renderer(Default::default());
    let prepared = renderer.prepare(&frame(scene, &pool)).unwrap();
    let quality = renderer.gpu.quality([64, 48]).unwrap();
    let viewport = crate::gpu::targets::target_bytes([64, 48], quality).unwrap();
    let cropped_group = crate::gpu::targets::target_bytes([32, 32], quality).unwrap();
    assert_eq!(
        renderer.usage().targets,
        viewport + cropped_group,
        "only output and isolated opacity group need targets"
    );
    let pixels = renderer.readback(prepared).unwrap();
    for (i, pixel) in pixels.iter().enumerate() {
        let (x, y) = (i % 64, i / 64);
        let expected = if x < 8 && y < 8 {
            [1., 1., 1., 1.]
        } else if (24..40).contains(&x) && (16..32).contains(&y) {
            [0.5, 0.5, 0., 1.]
        } else if (16..48).contains(&x) && (8..40).contains(&y) {
            [0.5, 0., 0.5, 1.]
        } else {
            [1., 0., 0., 1.]
        };
        assert!(
            pixel
                .iter()
                .zip(expected)
                .all(|(a, b)| (*a - b).abs() < 0.006),
            "pixel {x},{y}: {pixel:?}"
        );
    }
    *renderer.white.lock().unwrap() = None;
    empty(&renderer);
}

fn image_restore_fixture(
    size: [u32; 2],
    capture_included: bool,
) -> (
    RenderFrame,
    reel_ui::resources::ResourceRegistry,
    Vec<reel_ui::resources::ResourceHandle>,
) {
    image_restore_fixture_with_encoding(
        size,
        capture_included,
        reel_ui::resources::ColorEncoding::Linear,
    )
}

fn image_restore_fixture_with_encoding(
    size: [u32; 2],
    capture_included: bool,
    encoding: reel_ui::resources::ColorEncoding,
) -> (
    RenderFrame,
    reel_ui::resources::ResourceRegistry,
    Vec<reel_ui::resources::ResourceHandle>,
) {
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::{
        peniko::{ImageAlphaType, ImageBrush, ImageFormat},
        resources::ResourceRegistry,
    };
    let pool = pool();
    let mut registry = ResourceRegistry::new(3, 65264).unwrap();
    let mut handles = Vec::new();
    let mut scene = Scene::new();
    for (index, pixel) in [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]]
        .into_iter()
        .enumerate()
    {
        let bytes = pixel.repeat((size[0] * size[1]) as usize);
        let ticket = pool.reserve(bytes.len() as u64).unwrap();
        let handle = registry
            .register_image(
                ResourceVersion {
                    key: AssetKey(format!("cache-{index}")),
                    version: ContentVersion([index as u8; 32]),
                },
                OwnedBytes::new(bytes, ticket).unwrap(),
                size,
                ImageFormat::Rgba8,
                ImageAlphaType::Alpha,
                encoding,
            )
            .unwrap();
        let lease = registry.acquire(handle).unwrap();
        let image = crate::test_support::image_data(&lease);
        // Keep all three resources visible so occlusion culling cannot remove
        // the uploads and retirement work exercised by this fixture.
        let left = index as f64 * 125.;
        let right = if index == 2 { 400. } else { left + 125. };
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Image(ImageBrush::new(image.clone())),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(left, 0., right, 300.)),
            composite: Composite::default(),
        });
        handles.push(handle);
    }
    let mut widget = RecordWidget::new(scene);
    widget.capture_included = capture_included;
    let frame = frame_from_widget(widget, registry.freeze(&handles, &pool).unwrap(), &pool);
    (frame, registry, handles)
}

#[test]
fn capture_plans_omit_excluded_image_uploads_and_isolated_targets() {
    let (frame, _, handles) = image_restore_fixture([4, 4], false);
    let normal = crate::gpu::image_plan::collect(
        [&frame].into_iter(),
        4096,
        false,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    let captured = crate::gpu::image_plan::collect(
        [&frame].into_iter(),
        4096,
        true,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(normal.images.len(), handles.len());
    assert!(captured.images.is_empty());
    assert!(!frame.commands().is_empty());

    let pool = pool();
    let mut widget = RecordWidget::new(workload(1, 1, Color::WHITE));
    widget.opacity = 0.5;
    widget.capture_included = false;
    let frame = frame_from_widget(widget, empty_snapshot(&pool), &pool);
    assert!(
        crate::gpu::targets::target_pixels(
            &frame,
            None,
            [400, 300],
            false,
            4,
            u32::MAX,
            &crate::common::glyph_plan::Plan::default(),
            &crate::common::offscreen::Plan::default(),
            crate::common::target_plan::MaskExecution::Inline,
            &Default::default(),
            |_| Ok(())
        )
        .unwrap()
            > 400 * 300
    );
    assert_eq!(
        crate::gpu::targets::target_pixels(
            &frame,
            None,
            [400, 300],
            true,
            4,
            u32::MAX,
            &crate::common::glyph_plan::Plan::default(),
            &crate::common::offscreen::Plan::default(),
            crate::common::target_plan::MaskExecution::Inline,
            &Default::default(),
            |_| Ok(())
        )
        .unwrap(),
        400 * 300
    );
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn white_texture_survives_cancelled_preparations_and_preserves_budget_limits() {
    let pool = pool();
    let good = frame(workload(100, 0, Color::WHITE), &pool);
    let renderer = renderer(crate::gpu::GpuLimits {
        resource_bytes: 8,
        ..Default::default()
    });
    let first = renderer.prepare(&good).unwrap();
    let next = renderer.prepare(&good).unwrap();
    counts(&renderer, 1);
    assert_eq!(renderer.usage().resources, 8);
    assert_eq!(renderer.admission.usage().tickets, 2);
    drop(first);
    drop(next);
    empty(&renderer);
    let mut bad = workload(1, 0, Color::WHITE);
    bad.push_group(Group {
        composite: Composite {
            blend: reel_ui::peniko::BlendMode::new(
                reel_ui::peniko::Mix::ColorDodge,
                reel_ui::peniko::Compose::SrcOver,
            ),
            ..Default::default()
        },
        ..Default::default()
    });
    bad.pop_group();
    let bad = frame(bad, &pool);
    assert!(matches!(
        renderer.prepare(&bad),
        Err(GpuError::Unsupported { .. })
    ));
    counts(&renderer, 1);
    empty(&renderer);
    drop(renderer.prepare(&good).unwrap());
    counts(&renderer, 1);
    empty(&renderer);
    assert!(matches!(
        renderer.crossfade(&good, &bad, 0.5),
        Err(GpuError::Unsupported { .. })
    ));
    counts(&renderer, 1);
    empty(&renderer);
    let small = self::renderer(crate::gpu::GpuLimits {
        resource_bytes: 7,
        ..Default::default()
    });
    assert!(matches!(
        small.prepare(&good),
        Err(GpuError::Budget {
            domain: "resources",
            requested: 8,
            available: 7
        })
    ));
    counts(&small, 0);
    empty(&small);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn white_texture_whole_frame_quality_and_budget_failures_release() {
    let pool = pool();
    let mut nested = workload(1, 0, Color::WHITE);
    nested.push_group(Group::default());
    nested.push_group(Group::default());
    nested.pop_group();
    nested.pop_group();
    let nested = frame(nested, &pool);
    let two_solids = frame(workload(2, 0, Color::WHITE), &pool);
    let mut target_limited = renderer(crate::gpu::GpuLimits {
        target_bytes: 2 * 64 * 48 * 40,
        ..Default::default()
    });
    let prepared = target_limited.prepare(&nested).unwrap();
    assert_eq!(
        prepared.quality().samples,
        1,
        "quality must fit all three live targets"
    );
    let pixels = target_limited.readback(prepared).unwrap();
    assert!(
        pixels
            .iter()
            .all(|pixel| pixel.iter().all(|c| (*c - 1.).abs() < 0.002))
    );
    counts(&target_limited, 1);
    empty(&target_limited);
    let target_limited = renderer(crate::gpu::GpuLimits {
        target_bytes: 2 * 64 * 48 * 4,
        ..Default::default()
    });
    assert!(matches!(
        target_limited.prepare(&nested),
        Err(GpuError::Budget {
            domain: "targets",
            ..
        })
    ));
    counts(&target_limited, 0); // Admission fails before creating GPU resources.
    empty(&target_limited);
    let one_solid = frame(workload(1, 0, Color::WHITE), &pool);
    let baseline = renderer(Default::default());
    drop(baseline.prepare(&one_solid).unwrap());
    let one_draw_peak = baseline.budgets.transient.usage().peak_bytes;
    let transient_limited = renderer(crate::gpu::GpuLimits {
        transient_bytes: one_draw_peak,
        ..Default::default()
    });
    assert!(matches!(
        transient_limited.prepare(&two_solids),
        Err(GpuError::Budget {
            domain: "transient",
            ..
        })
    ));
    counts(&transient_limited, 1);
    empty(&transient_limited);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn alternating_frame_quality_reuses_targets_and_preserves_pixels() {
    let pool = pool();
    let plain = frame(workload(1, 0, Color::WHITE), &pool);
    let mut nested = workload(1, 0, Color::WHITE);
    nested.push_group(Group {
        composite: Composite {
            alpha: 0.5,
            ..Default::default()
        },
        ..Default::default()
    });
    fill(&mut nested, Color::WHITE);
    nested.push_group(Group {
        composite: Composite {
            alpha: 0.5,
            ..Default::default()
        },
        ..Default::default()
    });
    fill(&mut nested, Color::WHITE);
    fill(&mut nested, Color::WHITE);
    nested.pop_group();
    fill(&mut nested, Color::WHITE);
    nested.pop_group();
    let nested = frame(nested, &pool);
    let mut renderer = renderer(crate::gpu::GpuLimits {
        target_bytes: 2 * 64 * 48 * 40,
        ..Default::default()
    });
    renderer.prepare_viewport([64, 48]).unwrap();
    let initial = renderer.usage().targets;
    assert!(
        initial > 64 * 48 * 40,
        "viewport prepares another admitted tier"
    );
    renderer.prepare_viewport([64, 48]).unwrap();
    assert_eq!(
        renderer.usage().targets,
        initial,
        "repeated configuration reuses storage"
    );
    let mut owners: [Vec<std::sync::Weak<super::super::target_cache::Allocation>>; 2] =
        Default::default();
    for iteration in 0..12 {
        let index = iteration % 2;
        let prepared = renderer
            .prepare(if index == 0 { &plain } else { &nested })
            .unwrap();
        assert_eq!(prepared.quality().samples, if index == 0 { 4 } else { 1 });
        if owners[index].is_empty() {
            owners[index] = prepared.targets.iter().map(Arc::downgrade).collect();
        } else {
            assert_eq!(owners[index].len(), prepared.targets.len());
            for (old, new) in owners[index].iter().zip(&prepared.targets) {
                assert!(
                    old.upgrade().is_some_and(|old| Arc::ptr_eq(&old, new)),
                    "returning quality recreated its target"
                );
            }
        }
        let pixels = renderer.readback(prepared).unwrap();
        assert!(pixels.iter().flatten().all(|c| (*c - 1.).abs() < 0.002));
        assert!(renderer.budgets.targets.usage().bytes <= renderer.gpu.limits.target_bytes);
    }
    renderer.gpu.check().unwrap();
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn white_texture_concurrent_contexts_and_delayed_completion_keep_tickets() {
    let pool = pool();
    let frame = frame(workload(50, 25, Color::WHITE), &pool);
    let mut renderer = renderer(crate::gpu::GpuLimits {
        max_inflight: 8,
        ..Default::default()
    });
    let barrier = std::sync::Barrier::new(8);
    let prepared = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    renderer.prepare(&frame).unwrap()
                })
            })
            .collect();
        threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect::<Vec<_>>()
    });
    counts(&renderer, 1);
    assert_eq!(renderer.usage().resources, 8);
    assert_eq!(renderer.budgets.resources.usage().tickets, 1);
    assert!(matches!(renderer.prepare(&frame), Err(GpuError::Busy)));
    let mut ids = Vec::new();
    for p in prepared {
        ids.push(renderer.submit(p).unwrap());
    }
    drop(frame);
    let held = usage(&renderer);
    assert!(pool.usage().bytes > 0);
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert_eq!(
        usage(&renderer),
        held,
        "queue callback alone cannot retire accounting"
    );
    assert_eq!(renderer.admission.usage().tickets, 8);
    assert_eq!(renderer.poll().unwrap(), ids);
    assert!(renderer.poll().unwrap().is_empty());
    empty(&renderer);
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn white_texture_device_loss_foreign_submit_and_renderer_drop_release() {
    let pool = pool();
    let frame = frame(workload(100, 0, Color::WHITE), &pool);
    let mut old = renderer(Default::default());
    let mut other = renderer(Default::default());
    let p = old.prepare(&frame).unwrap();
    assert!(matches!(other.submit(p), Err(GpuError::WrongDevice)));
    empty(&old);
    counts(&other, 0);
    let p = old.prepare(&frame).unwrap();
    old.gpu.device.destroy();
    let _ = old.gpu.device.poll(wgpu::PollType::Poll);
    assert!(matches!(old.submit(p), Err(GpuError::DeviceLost(_))));
    assert!(matches!(old.prepare(&frame), Err(GpuError::DeviceLost(_))));
    counts(&old, 1);
    empty(&old);
    let p = other.prepare(&frame).unwrap();
    counts(&other, 1);
    other.submit(p).unwrap();
    drop(frame);
    assert!(pool.usage().bytes > 0);
    let budgets = [
        other.budgets.targets.clone(),
        other.budgets.resources.clone(),
        other.budgets.transient.clone(),
        other.admission.clone(),
    ];
    other.gpu.device.destroy();
    drop(other);
    assert!(
        budgets
            .iter()
            .all(|b| b.usage().bytes == 0 && b.usage().tickets == 0)
    );
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
#[ignore = "requires actual graphics adapter; CPU measurement"]
fn white_texture_cpu_measurement() {
    let pool = pool();
    let mut renderer = renderer(Default::default());
    for (solids, clips) in [(128, 0), (64, 64)] {
        let frame = frame(workload(solids, clips, Color::WHITE), &pool);
        let warm = renderer.prepare(&frame).unwrap();
        drop(warm); // Pipeline construction and driver first use excluded from timing.
        let mut samples = Vec::new();
        for _ in 0..30 {
            let before = renderer.gpu.upload_creations.snapshot();
            let start = Instant::now();
            let prepared = renderer.prepare(&frame).unwrap();
            samples.push(start.elapsed());
            let after = renderer.gpu.upload_creations.snapshot();
            assert_eq!(after, before);
            assert_eq!(renderer.usage().resources, 8);
            drop(prepared);
            empty(&renderer);
        }
        let pixels = renderer
            .readback(renderer.prepare(&frame).unwrap())
            .unwrap();
        assert!(
            pixels
                .iter()
                .all(|p| p.iter().all(|c| (*c - 1.).abs() < 0.006))
        );
        empty(&renderer);
        samples.sort_unstable();
        eprintln!(
            "adapter={:?}; load={solids} solids+{clips} clips size=64x48 samples=30 quality={:?} resource_bytes={} p50_us={:.3} p95_us={:.3}",
            renderer.gpu.adapter.get_info(),
            renderer.gpu.quality([64, 48]).unwrap(),
            renderer.usage().resources,
            samples[14].as_secs_f64() * 1e6,
            samples[28].as_secs_f64() * 1e6
        );
    }
}

#[test]
fn capture_global_transition_keeps_old_content_when_new_content_is_excluded() {
    use reel_ui::frame::{FrameTransition, RetainBuild};
    let pool = pool();
    let (old, _registry, _handles) = image_restore_fixture([4, 4], true);
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
    let mut scene = Scene::new();
    fill(&mut scene, Color::WHITE);
    let mut widget = RecordWidget::new(scene);
    widget.capture_included = false;
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [64, 48],
        },
        limits(),
        &pool,
    )
    .unwrap();
    build
        .set_transitions(vec![FrameTransition {
            root: None,
            previous: retain.finish().unwrap(),
            progress: 0.5,
            mask: None,
        }])
        .unwrap();
    while !build.step(step_budget()).unwrap().complete {}
    let frame = build.finish().unwrap();
    let images = crate::gpu::image_plan::collect(
        std::iter::once(&frame),
        8192,
        true,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(
        images.images.len(),
        3,
        "old accepted drawing remains an input to the blend"
    );
    assert!(
        crate::gpu::targets::target_pixels(
            &frame,
            None,
            [64, 48],
            true,
            4,
            u32::MAX,
            &crate::common::glyph_plan::Plan::default(),
            &crate::common::offscreen::Plan::default(),
            crate::common::target_plan::MaskExecution::Inline,
            &Default::default(),
            |_| Ok(())
        )
        .unwrap()
            > 64 * 48
    );
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn retained_transition_reuploads_retired_images_and_releases_them_after_completion() {
    use reel_ui::frame::{FrameTransition, RetainBuild};
    let pool = pool();
    let (old, mut registry, handles) = image_restore_fixture([4, 4], true);
    let mut retaining = RetainBuild::new(&old, None, limits(), &pool).unwrap();
    let mut budget = step_budget();
    loop {
        let progress = retaining.step(budget).unwrap();
        if progress.complete {
            break;
        }
        budget.max_units = budget.max_units.max(progress.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
    }
    let previous = retaining.finish().unwrap();
    let saved = previous.snapshot(|_| None).unwrap();
    let mut restore = saved.restore_begin(limits(), &pool).unwrap();
    let mut budget = step_budget();
    loop {
        let progress = restore.step(budget).unwrap();
        if progress.build.complete {
            break;
        }
        budget.max_units = budget.max_units.max(progress.build.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.build.minimum_bytes);
    }
    let restored = restore.finish().unwrap();
    for handle in handles {
        registry.retire(handle);
    }
    drop(old);
    drop(previous);
    drop(saved);
    drop(registry);
    let mut scene = Scene::new();
    fill(&mut scene, Color::from_rgba8(255, 0, 0, 128));
    let mut widget = RecordWidget::new(scene);
    widget.clip = false;
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [64, 48],
        },
        limits(),
        &pool,
    )
    .unwrap();
    build
        .set_transitions(vec![FrameTransition {
            root: None,
            previous: restored,
            progress: 0.5,
            mask: None,
        }])
        .unwrap();
    while !build.step(step_budget()).unwrap().complete {}
    let frame = build.finish().unwrap();
    let mut renderer = renderer(Default::default());
    assert!(matches!(renderer.prepare(&frame), Err(GpuError::Preparing)));
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    let prepared = renderer.prepare(&frame).unwrap();
    assert_eq!(prepared.transition_reads.len(), 1);
    let retained_owner = prepared.transition_reads[0].allocation().clone();
    renderer.cancel_preparations();
    assert!(retained_owner.is_pinned());
    let pixels = renderer.readback(prepared).unwrap();
    assert!(!retained_owner.is_pinned());
    drop(retained_owner);
    assert!(pixels.iter().enumerate().all(|(index, pixel)| {
        let mut expected = [128. / 255. * 0.5, 0., 0., (128. / 255. + 1.) * 0.5];
        expected[((index % 64) / 20).min(2)] += 0.5;
        pixel
            .iter()
            .zip(expected)
            .all(|(a, b)| (*a - b).abs() < 0.006)
    }));
    counts(&renderer, 4); // Three retained images and the shared solid-color white texture.
    drop(frame);
    renderer.poll().unwrap();
    renderer.images.clear_idle();
    assert_eq!(renderer.usage().resources, 8);
    // Retained preparation keeps reusable targets and queue capacity until teardown.
    let budgets = [
        renderer.budgets.targets.clone(),
        renderer.budgets.resources.clone(),
        renderer.budgets.transient.clone(),
    ];
    drop(renderer);
    for budget in budgets {
        assert_eq!(budget.usage().bytes, 0);
        assert_eq!(budget.usage().tickets, 0);
    }
}

mod geometry;
mod instances;
mod task_subgraphs;
mod text;
mod uploads;

mod budgets;
