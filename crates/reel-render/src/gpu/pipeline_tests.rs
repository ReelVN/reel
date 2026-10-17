//! Actual device regressions; counters are compiled only into the library test build.
use super::*;
#[allow(dead_code, reason = "shared actual UI fixture")]
#[path = "../../../reel-ui/tests/support/fixture.rs"]
pub(super) mod fixture;
#[allow(dead_code, reason = "shared R01 compiler fixture")]
#[path = "../../../reel-ui/tests/support/r01_fixture.rs"]
pub(super) mod r01_fixture;
use r01_fixture::*;
use reel_core::identity::FrameId;
use reel_ui::frame::{FrameBuild, FrameInfo};
use reel_ui::{
    imaging::{
        Composite,
        record::{Draw, Geometry, Group, Scene},
    },
    kurbo::{Affine, Rect},
    peniko::{Brush, Color},
};
use std::time::{Duration, Instant};

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
fn scene(color: Color) -> Scene {
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(color),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., 400., 300.)),
        composite: Composite::default(),
    });
    scene
}
fn frame(scene: Scene, size: [u32; 2], pool: &BudgetPool) -> RenderFrame {
    let mut widget = RecordWidget::new(scene);
    widget.clip = false;
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(pool),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: size,
        },
        limits(),
        pool,
    )
    .unwrap();
    while !build.step(step_budget()).unwrap().complete {}
    build.finish().unwrap()
}
fn counts(renderer: &Renderer, sets: usize) {
    assert_eq!(renderer.pipelines.initialized_count(), sets);
    assert_eq!(
        renderer.pipelines.creations.snapshot(),
        [3 * sets, 3 * sets, 3 * sets, 3 * sets, 4 * sets]
    );
}
fn empty(renderer: &Renderer) {
    renderer.targets.clear();
    let usage = renderer.usage();
    assert_eq!([usage.targets, usage.resources, usage.transient], [0; 3]);
    assert_eq!(renderer.admission.usage().tickets, 0);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn pipeline_reuse_all_tiers_and_cpu_measurement() {
    // One immutable budget makes size alone select each existing quality tier.
    let pool = pool();
    let frames =
        [[400, 300], [800, 600], [1000, 800]].map(|size| frame(scene(Color::WHITE), size, &pool));
    let limits = super::super::GpuLimits {
        target_bytes: 4_800_000,
        ..Default::default()
    };
    let mut renderer = renderer(limits);
    eprintln!(
        "adapter={:?}; counts=[bind layouts, shaders, pipeline layouts, render pipelines, samplers]",
        renderer.gpu.adapter.get_info()
    );
    counts(&renderer, 0);
    for (tier, frame) in frames.iter().enumerate() {
        let expected = [
            (wgpu::TextureFormat::Rgba16Float, 4),
            (wgpu::TextureFormat::Rgba16Float, 1),
            (wgpu::TextureFormat::Rgba8Unorm, 1),
        ][tier];
        assert_eq!(
            (
                renderer
                    .gpu
                    .quality(frame.info().physical_size)
                    .unwrap()
                    .format,
                renderer
                    .gpu
                    .quality(frame.info().physical_size)
                    .unwrap()
                    .samples
            ),
            expected,
            "this device must support all three tiers for this explicit regression"
        );
        let start = Instant::now();
        let prepared = renderer.prepare(frame).unwrap();
        let cold = start.elapsed();
        assert_eq!(
            (prepared.quality.format, prepared.quality.samples),
            expected
        );
        drop(prepared);
        counts(&renderer, tier + 1);
        let quality = renderer.gpu.quality(frame.info().physical_size).unwrap();
        let pipelines = renderer.pipelines.get(&renderer.gpu, quality).unwrap();
        assert!(
            Arc::ptr_eq(
                &pipelines,
                &renderer
                    .pipelines
                    .get(
                        &renderer.gpu,
                        Quality {
                            degraded: !quality.degraded,
                            ..quality
                        }
                    )
                    .unwrap()
            ),
            "only actual format and samples determine pipeline identity"
        );
        // Total 100 preparations: one cold plus 99 warm, with no submit/readback in timing.
        let mut warm = Vec::new();
        for _ in 1..100 {
            let start = Instant::now();
            let prepared = renderer.prepare(frame).unwrap();
            warm.push(start.elapsed());
            drop(prepared);
            empty(&renderer);
        }
        warm.sort_unstable();
        eprintln!(
            "tier={expected:?} size={:?} prepares=100 cold_us={:.3} warm_p50_us={:.3} warm_p95_us={:.3} creations={:?}",
            frame.info().physical_size,
            cold.as_secs_f64() * 1e6,
            warm[49].as_secs_f64() * 1e6,
            warm[94].as_secs_f64() * 1e6,
            renderer.pipelines.creations.snapshot()
        );
        counts(&renderer, tier + 1);
        let pixels = renderer.readback(renderer.prepare(frame).unwrap()).unwrap();
        assert!(
            pixels
                .iter()
                .all(|p| p.iter().all(|c| (*c - 1.).abs() < 0.006))
        );
        empty(&renderer);
    }
    // Return to every tier after all three slots have been filled.
    for frame in &frames {
        drop(renderer.prepare(frame).unwrap());
        counts(&renderer, 3);
        empty(&renderer);
    }
    drop(frames);
    assert_eq!(pool.usage().bytes, 0, "pipeline slots retain no CPU frames");
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn pipeline_reuse_concurrent_cold_preparations() {
    let pool = pool();
    let frame = frame(scene(Color::WHITE), [400, 300], &pool);
    let renderer = renderer(super::super::GpuLimits {
        max_inflight: 8,
        ..Default::default()
    });
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        let threads: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    renderer.prepare(&frame).unwrap()
                })
            })
            .collect();
        let prepared: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        counts(&renderer, 1);
        assert_eq!(renderer.admission.usage().tickets, 8);
        assert!(matches!(renderer.prepare(&frame), Err(GpuError::Busy)));
        drop(prepared);
    });
    empty(&renderer);
    counts(&renderer, 1);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn pipeline_reuse_instance_and_recovery_isolation() {
    let pool = pool();
    let frame = frame(scene(Color::WHITE), [400, 300], &pool);
    let old = renderer(Default::default());
    let mut same_generation = renderer(Default::default());
    let a = old.prepare(&frame).unwrap();
    let b = same_generation.prepare(&frame).unwrap();
    assert_eq!(old.gpu.generation, same_generation.gpu.generation);
    counts(&old, 1);
    counts(&same_generation, 1);
    assert!(!Arc::ptr_eq(
        &old.pipelines.get(&old.gpu, a.quality).unwrap(),
        &same_generation
            .pipelines
            .get(&same_generation.gpu, b.quality)
            .unwrap()
    ));
    assert!(matches!(
        same_generation.submit(a),
        Err(GpuError::WrongDevice)
    ));
    drop(b);
    empty(&old);
    empty(&same_generation);
    let stale = old.prepare(&frame).unwrap();
    old.gpu.device.destroy();
    let _ = old.gpu.device.poll(wgpu::PollType::wait_indefinitely());
    assert!(matches!(old.prepare(&frame), Err(GpuError::DeviceLost(_))));
    counts(&old, 1);
    let mut replacement = renderer(Default::default());
    counts(&replacement, 0);
    assert!(matches!(
        replacement.readback(stale),
        Err(GpuError::WrongDevice)
    ));
    empty(&old);
    let pixels = replacement
        .readback(replacement.prepare(&frame).unwrap())
        .unwrap();
    assert!(pixels.iter().any(|p| p[3] > 0.9));
    counts(&replacement, 1);
    empty(&replacement);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn pipeline_reuse_refusals_and_completion_lifecycle() {
    let pool = pool();
    let white = frame(scene(Color::WHITE), [400, 300], &pool);
    let blue = frame(scene(Color::from_rgb8(0, 0, 255)), [400, 300], &pool);
    let mut renderer = renderer(super::super::GpuLimits {
        target_bytes: 4_800_000 * 3,
        max_inflight: 3,
        ..Default::default()
    });
    let prepared = renderer.prepare(&white).unwrap();
    let baseline = renderer.usage();
    assert!(matches!(
        renderer.crossfade(&white, &blue, 0.5),
        Err(GpuError::Pressure { .. })
    ));
    assert_eq!(renderer.usage().targets, baseline.targets);
    assert_eq!(renderer.usage().resources, baseline.resources);
    assert_eq!(renderer.usage().transient, baseline.transient);
    assert_eq!(renderer.admission.usage().tickets, 1);
    counts(&renderer, 1);
    drop(prepared);
    empty(&renderer);
    let pixels = renderer
        .readback(renderer.crossfade(&white, &blue, 0.5).unwrap())
        .unwrap();
    let p = pixels[100 * 400 + 100];
    assert!((p[0] - 0.5).abs() < 0.006 && (p[2] - 1.).abs() < 0.006);
    empty(&renderer);
    counts(&renderer, 1);
    let mut nested = Scene::new();
    // Exceed the configured bytes even in the smallest whole-frame quality tier.
    for _ in 0..32 {
        nested.push_group(Group::default());
    }
    nested.append_transformed(&scene(Color::WHITE), Affine::IDENTITY);
    for _ in 0..32 {
        nested.pop_group();
    }
    let oversized = frame(nested, [400, 300], &pool);
    assert!(matches!(
        renderer.prepare(&oversized),
        Err(GpuError::Budget { .. })
    ));
    empty(&renderer);
    let mut bad = scene(Color::WHITE);
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
    let bad = frame(bad, [400, 300], &pool);
    assert!(matches!(
        renderer.prepare(&bad),
        Err(GpuError::Unsupported { .. })
    ));
    empty(&renderer);
    counts(&renderer, 1);
    let a = renderer.prepare(&white).unwrap();
    let b = renderer.prepare(&white).unwrap();
    let c = renderer.prepare(&white).unwrap();
    assert!(matches!(renderer.prepare(&white), Err(GpuError::Busy)));
    drop(c);
    let ids = vec![renderer.submit(a).unwrap(), renderer.submit(b).unwrap()];
    drop((white, blue, oversized, bad));
    assert!(pool.usage().bytes > 0);
    let used = renderer.usage();
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert_eq!(renderer.usage().targets, used.targets);
    assert_eq!(renderer.usage().resources, used.resources);
    assert_eq!(renderer.usage().transient, used.transient);
    assert_eq!(renderer.poll().unwrap(), ids);
    assert!(renderer.poll().unwrap().is_empty());
    empty(&renderer);
    assert_eq!(pool.usage().bytes, 0);
    counts(&renderer, 1);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn pipeline_reuse_construction_and_warm_driver_errors() {
    let pool = pool();
    let frame = frame(scene(Color::WHITE), [400, 300], &pool);
    let cold = renderer(Default::default());
    cold.pipelines
        .creations
        .invalid_shader
        .store(true, Ordering::Relaxed);
    assert!(matches!(cold.prepare(&frame), Err(GpuError::Driver(_))));
    empty(&cold);
    let attempted = cold.pipelines.creations.snapshot();
    assert_eq!(cold.pipelines.initialized_count(), 0);
    assert_eq!(
        attempted[3], 2,
        "both actual pipeline construction calls exercised"
    );
    assert!(matches!(cold.prepare(&frame), Err(GpuError::Driver(_))));
    assert_eq!(cold.pipelines.creations.snapshot(), attempted);
    let mut warm = renderer(Default::default());
    let prepared = warm.prepare(&frame).unwrap();
    let readback = warm.prepare(&frame).unwrap();
    counts(&warm, 1);
    // Deliver a real uncaptured device error after the slot was warmed.
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let _ = warm
                    .gpu
                    .device
                    .create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some("background warm driver fault"),
                        source: wgpu::ShaderSource::Wgsl("invalid WGSL".into()),
                    });
            })
            .join()
            .unwrap();
    });
    let _ = warm.gpu.device.poll(wgpu::PollType::Poll);
    assert!(matches!(warm.prepare(&frame), Err(GpuError::Driver(_))));
    assert!(matches!(warm.submit(prepared), Err(GpuError::Driver(_))));
    assert!(matches!(warm.readback(readback), Err(GpuError::Driver(_))));
    counts(&warm, 1);
    empty(&warm);
}

#[test]
#[ignore = "CPU measurement requires actual graphics adapter; run explicitly"]
fn pipeline_reuse_cold_warm_cpu_distribution() {
    let pool = pool();
    let frames =
        [[400, 300], [800, 600], [1000, 800]].map(|size| frame(scene(Color::WHITE), size, &pool));
    // Fresh Renderer and device per sample, same immutable frame and workload.
    // Device creation, Prepared drop, and queue wait are outside each timing.
    for frame in &frames {
        let mut cold = Vec::<Duration>::new();
        let mut warm = Vec::<Duration>::new();
        let mut quality = None;
        for _ in 0..30 {
            let renderer = renderer(super::super::GpuLimits {
                target_bytes: 4_800_000,
                ..Default::default()
            });
            let start = Instant::now();
            let prepared = renderer.prepare(frame).unwrap();
            cold.push(start.elapsed());
            quality = Some(prepared.quality());
            drop(prepared);
            let start = Instant::now();
            let prepared = renderer.prepare(frame).unwrap();
            warm.push(start.elapsed());
            drop(prepared);
            counts(&renderer, 1);
            empty(&renderer);
            assert_eq!(
                quality.unwrap(),
                renderer.gpu.quality(frame.info().physical_size).unwrap()
            );
        }
        cold.sort_unstable();
        warm.sort_unstable();
        eprintln!(
            "paired_samples=30 size={:?} quality={:?} cold_p50_us={:.3} cold_p95_us={:.3} warm_p50_us={:.3} warm_p95_us={:.3}; per_renderer_creations=[2,2,2,2,1]",
            frame.info().physical_size,
            quality.unwrap(),
            cold[14].as_secs_f64() * 1e6,
            cold[28].as_secs_f64() * 1e6,
            warm[14].as_secs_f64() * 1e6,
            warm[28].as_secs_f64() * 1e6
        );
    }
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn pipeline_reuse_budget_domains_and_renderer_drop() {
    let pool = pool();
    let frame = frame(scene(Color::WHITE), [400, 300], &pool);
    for (limits, domain, sets) in [
        (
            super::super::GpuLimits {
                target_bytes: 1,
                ..Default::default()
            },
            "targets",
            0,
        ),
        (
            super::super::GpuLimits {
                resource_bytes: 0,
                ..Default::default()
            },
            "resources",
            1,
        ),
        (
            super::super::GpuLimits {
                transient_bytes: 0,
                ..Default::default()
            },
            "transient",
            0,
        ),
    ] {
        let renderer = renderer(limits);
        for _ in 0..2 {
            assert!(
                matches!(renderer.prepare(&frame), Err(GpuError::Budget { domain: actual, .. }) if actual == domain)
            );
            counts(&renderer, sets);
            empty(&renderer);
        }
    }
    let mut low = renderer(super::super::GpuLimits {
        transient_bytes: 3 << 20,
        ..Default::default()
    });
    let prepared = low.prepare(&frame).unwrap();
    assert!(matches!(
        low.readback(prepared),
        Err(GpuError::Budget {
            domain: "readback",
            ..
        })
    ));
    counts(&low, 1);
    empty(&low);
    drop(low.prepare(&frame).unwrap());
    counts(&low, 1);
    empty(&low);
    let mut renderer = renderer(Default::default());
    let prepared = renderer.prepare(&frame).unwrap();
    renderer.submit(prepared).unwrap();
    drop(frame);
    assert!(pool.usage().bytes > 0);
    drop(renderer); // Renderer drains actual GPU work before releasing leases.
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn pipeline_reuse_outer_scope_cannot_hide_construction_failure() {
    let pool = pool();
    let frame = frame(scene(Color::WHITE), [400, 300], &pool);
    let renderer = renderer(Default::default());
    let outer_scope = renderer
        .gpu
        .device
        .push_error_scope(wgpu::ErrorFilter::Validation);
    renderer
        .pipelines
        .creations
        .invalid_shader
        .store(true, Ordering::Relaxed);
    assert!(
        matches!(renderer.prepare(&frame), Err(GpuError::Driver(_))),
        "an outer scope must not turn invalid Pipelines into a successful Prepared"
    );
    renderer
        .pipelines
        .creations
        .invalid_shader
        .store(false, Ordering::Relaxed);
    assert_eq!(renderer.pipelines.initialized_count(), 0);
    let attempted = renderer.pipelines.creations.snapshot();
    assert_eq!(attempted[3], 2);
    assert!(matches!(renderer.prepare(&frame), Err(GpuError::Driver(_))));
    assert_eq!(renderer.pipelines.creations.snapshot(), attempted);
    // Renderer owns its construction error; the caller's scope stays balanced.
    assert!(pollster::block_on(outer_scope.pop()).is_none());
    assert!(matches!(renderer.gpu.check(), Err(GpuError::Driver(_))));
    empty(&renderer);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn pipeline_reuse_outer_scope_preserves_caller_errors_and_warm_pixels() {
    let pool = pool();
    let frame = frame(scene(Color::WHITE), [400, 300], &pool);
    let mut renderer = renderer(Default::default());
    let outer_scope = renderer
        .gpu
        .device
        .push_error_scope(wgpu::ErrorFilter::Validation);
    let _ = renderer
        .gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("caller-owned scoped fault"),
            source: wgpu::ShaderSource::Wgsl("invalid caller WGSL".into()),
        });
    assert!(renderer.gpu.check().is_ok());
    drop(renderer.prepare(&frame).unwrap());
    drop(renderer.prepare(&frame).unwrap());
    counts(&renderer, 1);
    assert!(matches!(
        pollster::block_on(outer_scope.pop()),
        Some(wgpu::Error::Validation { .. })
    ));
    assert!(renderer.gpu.check().is_ok());
    let pixels = renderer
        .readback(renderer.prepare(&frame).unwrap())
        .unwrap();
    assert!(
        pixels
            .iter()
            .all(|p| p.iter().all(|c| (*c - 1.).abs() < 0.006))
    );
    counts(&renderer, 1);
    empty(&renderer);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn pipeline_reuse_cross_slot_concurrency_preserves_outer_scope_stack() {
    for invalid in [false, true] {
        let renderer = renderer(Default::default());
        renderer
            .pipelines
            .creations
            .invalid_shader
            .store(invalid, Ordering::Relaxed);
        let outer_scope = renderer
            .gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let barrier = std::sync::Barrier::new(3);
        let results = std::thread::scope(|scope| {
            let threads: Vec<_> = [
                (wgpu::TextureFormat::Rgba16Float, 4),
                (wgpu::TextureFormat::Rgba16Float, 1),
                (wgpu::TextureFormat::Rgba8Unorm, 1),
            ]
            .into_iter()
            .map(|(format, samples)| {
                scope.spawn({
                    let barrier = &barrier;
                    let renderer = &renderer;
                    move || {
                        barrier.wait();
                        renderer.pipelines.get(
                            &renderer.gpu,
                            Quality {
                                format,
                                samples,
                                degraded: samples != 4,
                            },
                        )
                    }
                })
            })
            .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        if invalid {
            assert!(
                results
                    .iter()
                    .all(|result| matches!(result, Err(GpuError::Driver(_))))
            );
            assert_eq!(renderer.pipelines.initialized_count(), 0);
            assert_eq!(
                renderer.pipelines.creations.snapshot(),
                [3, 3, 3, 3, 4],
                "one failed constructor becomes sticky before another tier constructs"
            );
        } else {
            assert!(results.iter().all(Result::is_ok));
            counts(&renderer, 3);
        }
        assert!(pollster::block_on(outer_scope.pop()).is_none());
        // A subsequent scoped error must still belong to the caller, not a leaked inner scope.
        let outer_scope = renderer
            .gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let _ = renderer
            .gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("post-construction scope balance"),
                source: wgpu::ShaderSource::Wgsl("invalid caller WGSL".into()),
            });
        assert!(matches!(
            pollster::block_on(outer_scope.pop()),
            Some(wgpu::Error::Validation { .. })
        ));
        assert_eq!(renderer.gpu.check().is_ok(), !invalid);
        empty(&renderer);
    }
}
