//! Real resource device-call counts and preparation ownership regressions.
use super::pipeline_tests::r01_fixture;
use super::*;
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

fn renderer(limits: super::super::GpuLimits) -> Renderer {
    Renderer::new(
        pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            super::super::DeviceGeneration(1),
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
    let mut widget = RecordWidget::new(scene);
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
fn counts(renderer: &Renderer, uploads: usize) {
    assert_eq!(renderer.gpu.upload_creations.snapshot(), [uploads; 3]);
}

#[test]
#[ignore = "requires actual graphics adapter and REEL_R01_TEST_FONT"]
fn long_text_reuses_atlas_and_batches_glyphs_across_frames_and_aborted_preparations() {
    use reel_ui::{
        app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
        core::*,
        dpi::PhysicalSize,
        peniko::Blob,
        resources::{ResourceData, ResourceRegistry},
    };
    use std::sync::atomic::Ordering;
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
                .with_style(StyleProperty::FontStyle(
                    reel_ui::reel_text::FontStyle::Italic,
                ))
                .with_outline(Some(TextOutline {
                    width: 2.,
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
    let glyph_count: usize = frame
        .commands()
        .iter()
        .filter_map(|c| match &c.op {
            reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::GlyphRun(run)) => {
                Some(run.glyphs.len())
            }
            _ => None,
        })
        .sum();
    assert!(glyph_count > 4000);
    let runs = frame
        .commands()
        .iter()
        .filter_map(|c| match &c.op {
            reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::GlyphRun(run)) => Some(run),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        runs.iter().any(|run| run.glyph_transform.is_some()),
        "regular font synthesizes italic"
    );
    assert!(
        runs.iter()
            .any(|run| matches!(run.style, reel_ui::peniko::Style::Stroke(_)))
    );
    let mut renderer = renderer(super::super::GpuLimits::default());
    eprintln!(
        "adapter={:?}, glyphs={glyph_count}",
        renderer.gpu.adapter.get_info()
    );
    let discarded = renderer.prepare(&frame).unwrap();
    let accepted = renderer.prepare(&frame).unwrap();
    drop(discarded);
    let pixels = renderer.readback(accepted).unwrap();
    assert!(pixels.iter().filter(|p| p[3] > 0.1).count() > 100);
    assert!(
        pixels
            .iter()
            .any(|p| p[0] > 0.3 && p[1] < 0.01 && p[2] < 0.01),
        "red outline remains outside fill"
    );
    assert!(
        pixels
            .iter()
            .any(|p| p[0] > 0.3 && p[1] > 0.3 && p[2] > 0.3),
        "white fill uses a separate coverage cache entry"
    );
    let run = runs
        .iter()
        .find(|r| matches!(r.style, reel_ui::peniko::Style::Stroke(_)))
        .unwrap();
    let at = frame.commands()[0].origin.at;
    let mut raster = super::super::glyph::Rasterizer::default();
    assert!(matches!(
        super::super::glyph::rasterize(&mut raster, &frame, run, 0, at, 1, 8192),
        Err(GpuError::Budget {
            domain: "glyph raster",
            ..
        })
    ));
    assert!(matches!(
        super::super::glyph::rasterize(&mut raster, &frame, run, 0, at, u64::MAX, 1),
        Err(GpuError::Unsupported {
            record: "glyph raster exceeds device texture dimensions",
            ..
        })
    ));
    let warm = renderer.gpu.upload_creations.snapshot();
    let resident = renderer.usage().resources;
    let target_resident = renderer.usage().targets;
    assert!(resident <= 2 << 20, "atlas residency {resident}");
    let started = Instant::now();
    let draws = renderer.gpu.upload_creations.draws.load(Ordering::Relaxed);
    for _ in 0..120 {
        renderer.submit(renderer.prepare(&frame).unwrap()).unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
        assert_eq!(renderer.usage().resources, resident);
        assert_eq!(renderer.usage().targets, target_resident);
    }
    let uploads = renderer.gpu.upload_creations.snapshot();
    // An optional white coverage texture is independent of glyph count.
    assert!(uploads[0] - warm[0] <= 120);
    assert!(uploads[2] - warm[2] <= 120);
    let draws = renderer.gpu.upload_creations.draws.load(Ordering::Relaxed) - draws;
    assert!(
        draws / 120 < glyph_count / 10,
        "draws must be per run/page, not per glyph"
    );
    eprintln!(
        "120 frames {:?}: atlas={resident} bytes; new textures={}, uploads={}, draws/frame={}",
        started.elapsed(),
        uploads[0] - warm[0],
        uploads[2] - warm[2],
        draws / 120
    );
    registry.retire(handle);
    renderer.retire(&[]);
    assert_eq!(
        renderer.usage().resources,
        0,
        "font retirement releases glyph pages after observed completion"
    );
}
fn usage(renderer: &Renderer) -> [u64; 3] {
    let u = renderer.usage();
    [u.targets, u.resources, u.transient]
}
fn empty(renderer: &Renderer) {
    renderer.targets.clear();
    assert_eq!(usage(renderer), [0; 3]);
    assert_eq!(renderer.admission.usage().tickets, 0);
    assert_eq!(renderer.budgets.resources.usage().tickets, 0);
}

#[test]
fn white_texture_upload_extent_cost() {
    assert_eq!(
        super::super::resources::upload_cost([1, 1]).unwrap(),
        256 + 8
    );
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
        let before = renderer.gpu.upload_creations.snapshot()[0];
        let prepared = renderer
            .prepare(&frame(workload(solids, clips, Color::WHITE), &pool))
            .unwrap();
        counts(&renderer, before + 1);
        assert_eq!(renderer.usage().resources, 264);
        assert_eq!(prepared.budget_scope.resources.usage().tickets, 1);
        drop(prepared);
        empty(&renderer);
        eprintln!(
            "solids={solids} clips={clips} actual=[texture,staging,copy]=[1,1,1] resource_bytes=264"
        );
    }
    let red = frame(workload(50, 50, Color::from_rgb8(255, 0, 0)), &pool);
    let blue = frame(workload(50, 50, Color::from_rgb8(0, 0, 255)), &pool);
    let before = renderer.gpu.upload_creations.snapshot()[0];
    let prepared = renderer.crossfade(&red, &blue, 0.25).unwrap();
    counts(&renderer, before + 1);
    assert_eq!(renderer.usage().resources, 264);
    let pixels = renderer.readback(prepared).unwrap();
    assert!(pixels.iter().all(|p| {
        p.iter()
            .zip([0.75, 0., 0.25, 1.])
            .all(|(a, b)| (*a - b).abs() < 0.006)
    }));
    empty(&renderer);
    eprintln!("crossfade=200 uses actual=[1,1,1] resource_bytes=264; analytical pixels passed");

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
    let before = renderer.gpu.upload_creations.snapshot()[0];
    let prepared = renderer.prepare(&frame(masked, &pool)).unwrap();
    counts(&renderer, before + 1);
    assert_eq!(renderer.usage().resources, 264);
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
fn white_texture_image_only_and_mixed_resources_are_independent() {
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::{
        peniko::{ImageAlphaType, ImageBrush, ImageFormat},
        resources::{ColorEncoding, ResourceData, ResourceRegistry},
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
    let ResourceData::Image { image, .. } = lease.data() else {
        unreachable!()
    };
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
    counts(&renderer, 2); // The submitted image survives cancellation; white remains private.
    assert_eq!(renderer.usage().resources, 272);
    let pixels = renderer.readback(p).unwrap();
    assert!(
        pixels
            .iter()
            .all(|p| p.iter().all(|c| (*c - 1.).abs() < 0.006))
    );
    renderer.targets.clear();
    assert_eq!(usage(&renderer), [0, 8, 0]);
    let cached = renderer.prepare(&image_only).unwrap();
    counts(&renderer, 2);
    assert_eq!(usage(&renderer)[1], 8);
    drop(cached);
    renderer.retire(std::slice::from_ref(lease.version()));
    empty(&renderer);

    let mut limited = self::renderer(super::super::GpuLimits {
        resource_bytes: 264,
        ..Default::default()
    });
    let prepared = limited.prepare(&image_only).unwrap();
    limited.readback(prepared).unwrap();
    let held = limited.prepare(&image_only).unwrap();
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
    let target =
        super::super::targets::target_bytes([64, 48], renderer.gpu.quality([64, 48]).unwrap())
            .unwrap();
    assert_eq!(
        renderer.usage().targets,
        2 * target,
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
    empty(&renderer);
}

fn image_restore_fixture(
    size: [u32; 2],
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
        resources::{ColorEncoding, ResourceData, ResourceRegistry},
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
                ColorEncoding::Srgb,
            )
            .unwrap();
        let lease = registry.acquire(handle).unwrap();
        let ResourceData::Image { image, .. } = lease.data() else {
            unreachable!()
        };
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Image(ImageBrush::new(image.clone())),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(0., 0., 400., 300.)),
            composite: Composite::default(),
        });
        handles.push(handle);
    }
    let frame = frame_with_snapshot(scene, registry.freeze(&handles, &pool).unwrap(), &pool);
    (frame, registry, handles)
}

#[test]
fn image_plan_deduplicates_both_transition_inputs_before_admission() {
    let (frame, _, handles) = image_restore_fixture([4, 4]);
    let plan =
        super::super::image_plan::collect([&frame, &frame].into_iter(), 4096, |_| Ok(())).unwrap();
    assert_eq!(plan.len(), handles.len());
    for item in &plan {
        assert!(handles.contains(&item.source.handle()));
        assert_eq!(item.resident, 4 * 4 * 8);
        assert_eq!(item.staging, 4 * 256);
    }
    assert!(matches!(
        super::super::image_plan::collect([&frame].into_iter(), 2, |_| Ok(())),
        Err(GpuError::InvalidSize)
    ));
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn cold_restore_batches_survive_cancel_and_release_only_completed_staging() {
    let size = [1920, 1080];
    let (frame, mut registry, handles) = image_restore_fixture(size);
    let resident = u64::from(size[0]) * u64::from(size[1]) * 8;
    let mut renderer = renderer(super::super::GpuLimits {
        resource_bytes: 64 << 20,
        ..Default::default()
    });
    assert!(matches!(
        renderer.prepare(&frame),
        Err(GpuError::Pressure { .. })
    ));
    counts(&renderer, 2);
    assert_eq!(renderer.usage().resources, 4 * resident);
    assert!(
        renderer.has_pending(),
        "upload progress must wake a frame which was never submitted"
    );
    assert!(matches!(
        renderer.prepare(&frame),
        Err(GpuError::Pressure { .. })
    ));
    counts(&renderer, 2); // Retrying before completion does not recreate textures.
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert_eq!(
        renderer.usage().resources,
        4 * resident,
        "callbacks do not retire accounting"
    );
    assert!(
        renderer.poll().unwrap().is_empty(),
        "uploads have no presentation identity"
    );
    assert_eq!(renderer.usage().resources, 2 * resident);
    let first = renderer.prepare(&frame).unwrap();
    counts(&renderer, 3);
    drop(first); // Cancelled rendering retains independently submitted uploads.
    let second = renderer.prepare(&frame).unwrap();
    counts(&renderer, 3);
    renderer.submit(second).unwrap();
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert_eq!(renderer.poll().unwrap().len(), 1);
    assert_eq!(renderer.usage().resources, 3 * resident);
    let reused = renderer.prepare(&frame).unwrap();
    let pixels = renderer.readback(reused).unwrap();
    assert!(pixels.iter().all(|p| {
        p.iter()
            .zip([0., 0., 1., 1.])
            .all(|(a, b)| (*a - b).abs() < 0.006)
    }));
    counts(&renderer, 3);
    let retained = renderer.prepare(&frame).unwrap();
    for handle in handles {
        assert!(registry.retire(handle));
    }
    renderer.retire(&[]);
    assert_eq!(renderer.usage().resources, 3 * resident);
    drop(retained);
    empty(&renderer);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn cold_upload_progress_with_one_submission_slot() {
    let (frame, _, _) = image_restore_fixture([4, 4]);
    let mut renderer = renderer(super::super::GpuLimits {
        max_inflight: 1,
        ..Default::default()
    });
    assert!(matches!(renderer.prepare(&frame), Err(GpuError::Busy)));
    counts(&renderer, 3);
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert!(renderer.poll().unwrap().is_empty());
    renderer.submit(renderer.prepare(&frame).unwrap()).unwrap();
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert_eq!(renderer.poll().unwrap().len(), 1);
    counts(&renderer, 3);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn white_texture_refusal_pressure_and_cancel_do_not_publish_across_transactions() {
    let pool = pool();
    let good = frame(workload(100, 0, Color::WHITE), &pool);
    let renderer = renderer(super::super::GpuLimits {
        resource_bytes: 264,
        ..Default::default()
    });
    let first = renderer.prepare(&good).unwrap();
    let held = usage(&renderer);
    assert!(matches!(
        renderer.prepare(&good),
        Err(GpuError::Pressure {
            domain: "resources",
            requested: 264,
            available: 0
        })
    ));
    counts(&renderer, 1);
    assert_eq!(usage(&renderer), held);
    assert_eq!(renderer.admission.usage().tickets, 1);
    drop(first); // Never submitted: its upload cannot initialize the next transaction.
    empty(&renderer);
    let next = renderer.prepare(&good).unwrap();
    counts(&renderer, 2);
    drop(next);
    empty(&renderer);
    let mut bad = workload(1, 0, Color::WHITE);
    bad.push_group(Group {
        composite: Composite {
            blend: reel_ui::peniko::BlendMode::new(
                reel_ui::peniko::Mix::Multiply,
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
    counts(&renderer, 3); // Fails after creating white; all partial tickets must release.
    empty(&renderer);
    drop(renderer.prepare(&good).unwrap());
    counts(&renderer, 4);
    empty(&renderer);
    assert!(matches!(
        renderer.crossfade(&good, &bad, 0.5),
        Err(GpuError::Unsupported { .. })
    ));
    counts(&renderer, 5); // Second input fails after the first input initialized the slot.
    empty(&renderer);
    let small = self::renderer(super::super::GpuLimits {
        resource_bytes: 263,
        ..Default::default()
    });
    assert!(matches!(
        small.prepare(&good),
        Err(GpuError::Budget {
            domain: "resources",
            requested: 264,
            available: 263
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
    let mut target_limited = renderer(super::super::GpuLimits {
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
    let target_limited = renderer(super::super::GpuLimits {
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
    let transient_limited = renderer(super::super::GpuLimits {
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
fn white_texture_concurrent_contexts_and_delayed_completion_keep_tickets() {
    let pool = pool();
    let frame = frame(workload(50, 25, Color::WHITE), &pool);
    let mut renderer = renderer(super::super::GpuLimits {
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
    counts(&renderer, 8);
    assert_eq!(renderer.usage().resources, 8 * 264);
    assert_eq!(renderer.budgets.resources.usage().tickets, 8);
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
    counts(&old, 2);
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
    // Only the measurement test accepts a baseline expectation, allowing the
    // identical Rust fixture/counters to run on the unchanged baseline draw path.
    let expected: usize =
        std::env::var("REEL_R04_EXPECT_UPLOADS").map_or(1, |n| n.parse().unwrap());
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
            assert_eq!(
                std::array::from_fn::<_, 3, _>(|i| after[i] - before[i]),
                [expected; 3]
            );
            assert_eq!(renderer.usage().resources, expected as u64 * 264);
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
            "adapter={:?}; load={solids} solids+{clips} clips size=64x48 samples=30 quality={:?} uploads={expected} resource_bytes={} p50_us={:.3} p95_us={:.3}",
            renderer.gpu.adapter.get_info(),
            renderer.gpu.quality([64, 48]).unwrap(),
            expected * 264,
            samples[14].as_secs_f64() * 1e6,
            samples[28].as_secs_f64() * 1e6
        );
    }
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn sprite_batches_share_uploads_and_respect_paint_boundaries() {
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::{
        peniko::{ImageAlphaType, ImageBrush, ImageFormat},
        resources::{ColorEncoding, ResourceData, ResourceRegistry},
    };
    use std::sync::atomic::Ordering;
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 65264).unwrap();
    let bytes = [255, 255, 255, 255].repeat(16);
    let ticket = pool.reserve(bytes.len() as u64).unwrap();
    let handle = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("sprite".into()),
                version: ContentVersion([8; 32]),
            },
            OwnedBytes::new(bytes, ticket).unwrap(),
            [4, 4],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let lease = registry.acquire(handle).unwrap();
    let ResourceData::Image { image, .. } = lease.data() else {
        unreachable!()
    };
    let mut renderer = renderer(Default::default());
    eprintln!("sprite adapter: {:?}", renderer.gpu.adapter.get_info());
    for (particles, boundary) in [(128, false), (2048, false), (16384, false), (2048, true)] {
        let mut scene = Scene::new();
        for index in 0..particles {
            if boundary && index == particles / 2 {
                scene.push_group(Group {
                    composite: Composite {
                        alpha: 0.5,
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }
            scene.draw(Draw::Fill {
                transform: Affine::translate(((index % 100) as f64, (index / 100) as f64)),
                fill_rule: Default::default(),
                brush: Brush::Image(
                    ImageBrush::new(image.clone()).with_alpha((index % 10 + 1) as f32 / 10.),
                ),
                brush_transform: Some(Affine::scale(0.5)),
                shape: Geometry::Rect(Rect::new(0., 0., 2., 2.)),
                composite: Composite::default(),
            });
        }
        if boundary {
            scene.pop_group();
        }
        let frame = frame_with_snapshot(scene, registry.freeze(&[handle], &pool).unwrap(), &pool);
        let before = [
            &renderer.gpu.upload_creations.draws,
            &renderer.gpu.upload_creations.mesh_buffers,
            &renderer.gpu.upload_creations.brush_groups,
        ]
        .map(|v| v.load(Ordering::Relaxed));
        let start = Instant::now();
        let prepared = renderer.prepare(&frame).unwrap();
        let preparation = start.elapsed();
        let transient_peak = prepared.budget_scope.transient.usage().peak_bytes;
        let resources_peak = prepared.budget_scope.resources.usage().peak_bytes;
        let after = [
            &renderer.gpu.upload_creations.draws,
            &renderer.gpu.upload_creations.mesh_buffers,
            &renderer.gpu.upload_creations.brush_groups,
        ]
        .map(|v| v.load(Ordering::Relaxed));
        let batches = if boundary { 2 } else { 1 };
        assert_eq!(
            std::array::from_fn::<_, 3, _>(|i| after[i] - before[i]),
            [batches, batches * 2, batches]
        );
        renderer.submit(prepared).unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
        eprintln!(
            "{particles} sprites group_boundary={boundary}: draws={batches}, mesh_uploads={}, brush_groups={batches}, prepare={preparation:?}, prepare_submit_complete={:?}, transient_peak_bytes={transient_peak}, image_peak_bytes={resources_peak}",
            batches * 2,
            start.elapsed()
        );
    }
    counts(&renderer, 1);
}
