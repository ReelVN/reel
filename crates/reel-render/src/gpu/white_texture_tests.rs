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
    kurbo::{Affine, Rect},
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
            shape: Geometry::Rect(Rect::new(0., 0., 400., 300.)),
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
fn usage(renderer: &Renderer) -> [u64; 3] {
    let u = renderer.usage();
    [u.targets, u.resources, u.transient]
}
fn empty(renderer: &Renderer) {
    assert_eq!(usage(renderer), [0; 3]);
    assert_eq!(renderer.admission.usage().tickets, 0);
    assert_eq!(renderer.budgets.resources.usage().tickets, 0);
}

#[test]
fn white_texture_upload_extent_cost() {
    assert_eq!(
        super::super::resources::upload_cost([1, 1]).unwrap(),
        16 + 256 + 256 + 8
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
        assert_eq!(renderer.usage().resources, 536);
        assert_eq!(prepared.budget_scope.resources.usage().tickets, 1);
        drop(prepared);
        empty(&renderer);
        eprintln!(
            "solids={solids} clips={clips} actual=[texture,staging,copy]=[1,1,1] resource_bytes=536"
        );
    }
    let red = frame(workload(50, 50, Color::from_rgb8(255, 0, 0)), &pool);
    let blue = frame(workload(50, 50, Color::from_rgb8(0, 0, 255)), &pool);
    let before = renderer.gpu.upload_creations.snapshot()[0];
    let prepared = renderer.crossfade(&red, &blue, 0.25).unwrap();
    counts(&renderer, before + 1);
    assert_eq!(renderer.usage().resources, 536);
    let pixels = renderer.readback(prepared).unwrap();
    assert!(pixels.iter().all(|p| {
        p.iter()
            .zip([0.75, 0., 0.25, 1.])
            .all(|(a, b)| (*a - b).abs() < 0.006)
    }));
    empty(&renderer);
    eprintln!("crossfade=200 uses actual=[1,1,1] resource_bytes=536; analytical pixels passed");

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
    assert_eq!(renderer.usage().resources, 536);
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
    let mut registry = ResourceRegistry::new(1, 65536).unwrap();
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
    assert_eq!(renderer.usage().resources, 536);
    drop(p);
    empty(&renderer);
    let p = renderer.prepare(&mixed).unwrap();
    counts(&renderer, 3); // Independent image and shared white, even at the same 1x1 size.
    assert_eq!(renderer.usage().resources, 1072);
    let pixels = renderer.readback(p).unwrap();
    assert!(
        pixels
            .iter()
            .all(|p| p.iter().all(|c| (*c - 1.).abs() < 0.006))
    );
    empty(&renderer);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn white_texture_refusal_pressure_and_cancel_do_not_publish_across_transactions() {
    let pool = pool();
    let good = frame(workload(100, 0, Color::WHITE), &pool);
    let renderer = renderer(super::super::GpuLimits {
        resource_bytes: 536,
        ..Default::default()
    });
    let first = renderer.prepare(&good).unwrap();
    let held = usage(&renderer);
    assert!(matches!(
        renderer.prepare(&good),
        Err(GpuError::Pressure {
            domain: "resources",
            requested: 536,
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
        resource_bytes: 535,
        ..Default::default()
    });
    assert!(matches!(
        small.prepare(&good),
        Err(GpuError::Budget {
            domain: "resources",
            requested: 536,
            available: 535
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
    assert_eq!(renderer.usage().resources, 8 * 536);
    assert_eq!(renderer.budgets.resources.usage().tickets, 8);
    assert!(matches!(renderer.prepare(&frame), Err(GpuError::Busy)));
    let mut ids = Vec::new();
    for p in prepared {
        ids.push(renderer.submit(p).unwrap());
    }
    drop(frame);
    let held = usage(&renderer);
    assert!(pool.usage().bytes > 0);
    renderer.gpu.device.poll(wgpu::PollType::Wait).unwrap();
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
    assert!(matches!(other.submit(p), Err(GpuError::DeviceLost)));
    empty(&old);
    counts(&other, 0);
    let p = old.prepare(&frame).unwrap();
    old.gpu.device.destroy();
    let _ = old.gpu.device.poll(wgpu::PollType::Poll);
    assert!(matches!(old.submit(p), Err(GpuError::DeviceLost)));
    assert!(matches!(old.prepare(&frame), Err(GpuError::DeviceLost)));
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
            assert_eq!(renderer.usage().resources, expected as u64 * 536);
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
            expected * 536,
            samples[14].as_secs_f64() * 1e6,
            samples[28].as_secs_f64() * 1e6
        );
    }
}
