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
        [3 * sets, 5 * sets, 5 * sets, 5 * sets, 4 * sets]
    );
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn nv12_submit_preserves_producer_and_reuses_only_matching_completed_planes() {
    use reel_core::{
        budget::BudgetTicket,
        identity::{AssetKey, ContentVersion, ResourceVersion},
        image::{Yuv420Image, Yuv420Layout, Yuv420Storage, YuvColor, YuvMatrix, YuvTransfer},
    };
    use reel_ui::{
        core::NewWidget,
        frame::ImageAlphaLayout,
        kurbo::{Point, Size},
        resources::{ResourceRegistry, ResourceSnapshot},
        widgets::Image,
    };
    #[derive(Debug)]
    struct Pixels {
        planes: [Vec<u8>; 3],
        _ticket: BudgetTicket,
    }
    impl Yuv420Storage for Pixels {
        fn plane(&self, i: usize) -> &[u8] {
            &self.planes[i]
        }
    }
    let pool = pool();
    let make_frame = |layout, version| {
        let (planes, strides) = match layout {
            Yuv420Layout::Planar => ([vec![16; 32], vec![128; 8], vec![128; 8]], [4, 2, 2]),
            // Last rows intentionally have no trailing padding.
            Yuv420Layout::Nv12 => ([vec![16; 60], vec![128; 28], vec![]], [8, 8, 0]),
        };
        let bytes = planes.iter().map(|p| p.len() as u64).sum();
        let pixels = Arc::new(Pixels {
            planes,
            _ticket: pool.reserve(bytes).unwrap(),
        });
        let weak = Arc::downgrade(&pixels);
        let image = Yuv420Image::with_layout(
            [4, 8],
            YuvColor {
                matrix: YuvMatrix::Bt601,
                full_range: false,
                transfer: YuvTransfer::Bt709,
            },
            layout,
            strides,
            pixels,
        )
        .unwrap();
        let mut registry = ResourceRegistry::new(1, 64 << 10).unwrap();
        let handle = registry
            .register_yuv420(
                ResourceVersion {
                    key: AssetKey(format!("video-{version}")),
                    version: ContentVersion([version; 32]),
                },
                image,
            )
            .unwrap();
        let mut widget = RecordWidget::new(Scene::new());
        widget.inner.add(
            NewWidget::new(
                Image::from_resource(registry.acquire(handle).unwrap())
                    .with_alpha_layout(ImageAlphaLayout::TopBottom),
            ),
            Point::ZERO,
            Size::new(400., 300.),
        );
        let mut bridge = widget_bridge(widget, Affine::IDENTITY);
        let snapshot =
            ResourceSnapshot::capture([registry.acquire(handle)].into_iter(), &pool).unwrap();
        let mut build = FrameBuild::new(
            bridge.record(FrameId(version.into())).unwrap(),
            snapshot,
            FrameInfo {
                logical_size: [400., 300.],
                physical_size: [400, 300],
            },
            limits(),
            &pool,
        )
        .unwrap();
        while !build.step(step_budget()).unwrap().complete {}
        (build.finish().unwrap(), weak)
    };
    // Keep two submitted pictures and their upload batches alive together.
    let mut renderer = renderer(super::super::GpuLimits {
        max_inflight: 4,
        ..Default::default()
    });
    let textures = || {
        renderer
            .gpu
            .upload_creations
            .textures
            .load(Ordering::Relaxed)
    };
    let baseline = textures();
    let (first, first_pixels) = make_frame(Yuv420Layout::Nv12, 1u8);
    let prepared = renderer.prepare(&first).unwrap();
    assert_eq!(
        textures() - baseline,
        2,
        "NV12 keeps one Y and one RG8 UV texture"
    );
    renderer.submit(prepared).unwrap();
    drop(first);
    assert!(
        first_pixels.upgrade().is_some(),
        "submission owns the producer lease"
    );
    let (second, second_pixels) = make_frame(Yuv420Layout::Nv12, 2);
    let prepared = renderer.prepare(&second).unwrap();
    assert_eq!(
        renderer
            .gpu
            .upload_creations
            .textures
            .load(Ordering::Relaxed)
            - baseline,
        4,
        "in-flight planes cannot be overwritten"
    );
    renderer.submit(prepared).unwrap();
    drop(second);
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    assert!(first_pixels.upgrade().is_none());
    assert!(second_pixels.upgrade().is_none());
    let (planar, _) = make_frame(Yuv420Layout::Planar, 3);
    let prepared = renderer.prepare(&planar).unwrap();
    assert_eq!(
        renderer
            .gpu
            .upload_creations
            .textures
            .load(Ordering::Relaxed)
            - baseline,
        7,
        "same-size NV12 textures cannot become planar textures"
    );
    renderer.submit(prepared).unwrap();
    drop(planar);
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    let (third, third_pixels) = make_frame(Yuv420Layout::Nv12, 4);
    let prepared = renderer.prepare(&third).unwrap();
    assert_eq!(
        renderer
            .gpu
            .upload_creations
            .textures
            .load(Ordering::Relaxed)
            - baseline,
        7,
        "completed NV12 planes are reused"
    );
    renderer.submit(prepared).unwrap();
    drop(third);
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    assert!(third_pixels.upgrade().is_none());
    assert_eq!(renderer.usage().transient, 0);
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn initialization_reuses_builtin_mask_without_holding_program_or_images() {
    use reel_ui::stage::MaskProgram;
    let renderer = renderer(Default::default());
    renderer.prepare_pipelines().unwrap();
    let sets = renderer.gpu.supported_qualities().count();
    counts(&renderer, sets);
    let initialized = renderer.usage();
    assert_eq!(initialized.targets, 0);
    assert_eq!(initialized.transient, 0);
    for quality in renderer.gpu.supported_qualities() {
        let pipelines = renderer.pipelines.get(&renderer.gpu, quality).unwrap();
        let first = MaskProgram::image("first".into());
        let weak = Arc::downgrade(&first);
        let a = pipelines
            .masks
            .get(&renderer.gpu, quality, &first, &renderer.budgets.resources)
            .unwrap();
        drop(first);
        assert!(weak.upgrade().is_none());
        pipelines.masks.prune();
        let second = MaskProgram::image("different".into());
        let b = pipelines
            .masks
            .get(&renderer.gpu, quality, &second, &renderer.budgets.resources)
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(pipelines.masks.len(), 1);
    }
    renderer.prepare_pipelines().unwrap();
    counts(&renderer, sets);
    assert_eq!(renderer.usage().resources, initialized.resources);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn zero_progress_transition_prepares_custom_mask_before_acceptance() {
    use reel_ui::{
        core::PaintImageMask,
        frame::{FrameTransition, RetainBuild},
        stage::{MaskOp, MaskProgram},
    };
    let renderer = renderer(Default::default());
    let pool = pool();
    let old = frame(scene(Color::BLACK), [400, 300], &pool);
    let mut retain = RetainBuild::new(&old, None, limits(), &pool).unwrap();
    while !retain.step(step_budget()).unwrap().complete {}
    let previous = retain.finish().unwrap();
    let program = MaskProgram::new(vec![MaskOp::Parameter(0)], vec![]).unwrap();
    let build_transition = |progress| {
        let mut widget = RecordWidget::new(scene(Color::WHITE));
        widget.clip = false;
        let mut bridge = widget_bridge(widget, Affine::IDENTITY);
        let mut build = FrameBuild::new(
            bridge.record(FrameId(1)).unwrap(),
            empty_snapshot(&pool),
            FrameInfo {
                logical_size: [400., 300.],
                physical_size: [400, 300],
            },
            limits(),
            &pool,
        )
        .unwrap();
        build
            .set_transitions(vec![FrameTransition {
                root: None,
                previous: previous.clone(),
                progress,
                mask: Some(PaintImageMask {
                    program: program.clone(),
                    images: Vec::new().into(),
                    bounds: Rect::new(0., 0., 400., 300.),
                    parameters: [0.; 5],
                }),
            }])
            .unwrap();
        while !build.step(step_budget()).unwrap().complete {}
        build.finish().unwrap()
    };
    let zero = build_transition(0.);
    let prepared = renderer.prepare(&zero).unwrap();
    let quality = prepared.quality();
    drop(prepared);
    let pipelines = renderer.pipelines.get(&renderer.gpu, quality).unwrap();
    assert_eq!(
        pipelines.masks.len(),
        1,
        "the zero-progress candidate must already own its compiled mask"
    );
    let first = pipelines
        .masks
        .get(
            &renderer.gpu,
            quality,
            &program,
            &renderer.budgets.resources,
        )
        .unwrap();
    let middle = build_transition(0.5);
    drop(renderer.prepare(&middle).unwrap());
    let second = pipelines
        .masks
        .get(
            &renderer.gpu,
            quality,
            &program,
            &renderer.budgets.resources,
        )
        .unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    drop((zero, middle));
    drop(program);
    pipelines.masks.prune();
    assert_eq!(
        pipelines.masks.len(),
        0,
        "custom programs still release their cache metadata"
    );
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn subtree_blend_pixels_preserve_transparency_and_nested_opacity() {
    use reel_ui::peniko::{BlendMode, Compose, Mix};
    let pool = pool();
    let mut renderer = renderer(Default::default());
    eprintln!("blend adapter={:?}", renderer.gpu.adapter.get_info());
    let linear = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    for backdrop_alpha in [0, 64, 192, 255] {
        for (blend, mode) in [
            (BlendMode::default(), 0),
            (BlendMode::new(Mix::Normal, Compose::PlusLighter), 1),
            (Mix::Multiply.into(), 2),
            (Mix::Screen.into(), 3),
        ] {
            let mut drawing = Scene::new();
            drawing.push_group(Group {
                composite: Composite {
                    alpha: 0.5,
                    ..Default::default()
                },
                ..Default::default()
            });
            drawing.append_transformed(
                &scene(Color::from_rgba8(64, 128, 192, backdrop_alpha)),
                Affine::IDENTITY,
            );
            drawing.push_group(Group {
                composite: Composite { alpha: 0.75, blend },
                ..Default::default()
            });
            drawing.append_transformed(
                &scene(Color::from_rgba8(192, 96, 32, 192)),
                Affine::IDENTITY,
            );
            drawing.pop_group();
            drawing.pop_group();
            let frame = frame(drawing, [400, 300], &pool);
            let pixels = renderer
                .readback(renderer.prepare(&frame).unwrap())
                .unwrap();
            let sa = 192. / 255. * 0.75;
            let da = backdrop_alpha as f32 / 255.;
            let expected: [f32; 4] = std::array::from_fn(|i| {
                if i == 3 {
                    return 0.5
                        * if mode == 1 {
                            (sa + da).min(1.)
                        } else {
                            sa + da * (1. - sa)
                        };
                }
                let s = linear([192., 96., 32.][i] / 255.) * sa;
                let d = linear([64., 128., 192.][i] / 255.) * da;
                0.5 * match mode {
                    0 => s + d * (1. - sa),
                    1 => (s + d).min(1.),
                    2 => s * d + s * (1. - da) + d * (1. - sa),
                    _ => s + d - s * d,
                }
            });
            for pixel in pixels {
                for i in 0..4 {
                    assert!(
                        (pixel[i] - expected[i]).abs() < 0.006,
                        "mode={mode} alpha={backdrop_alpha}: {pixel:?} != {expected:?}"
                    );
                }
            }
        }
    }
    counts(&renderer, 1);
    empty(&renderer);
}
fn empty(renderer: &Renderer) {
    renderer.targets.clear();
    let usage = renderer.usage();
    assert_eq!([usage.targets, usage.resources, usage.transient], [0; 3]);
    assert_eq!(renderer.admission.usage().tickets, 0);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn subtree_blend_mask_clip_and_backdrop_fit_the_planned_target_budget() {
    use reel_ui::{
        imaging::{
            MaskMode,
            record::{AppliedMask, Clip, Mask},
        },
        peniko::Mix,
    };
    let pool = pool();
    for masked in [false, true] {
        for clipped in [false, true] {
            let mut drawing = scene(Color::from_rgb8(255, 0, 0));
            let mask = masked.then(|| {
                drawing.define_mask(Mask::new(
                    MaskMode::Alpha,
                    scene(Color::from_rgba8(255, 255, 255, 128)),
                ))
            });
            drawing.push_group(Group {
                composite: Composite {
                    alpha: 0.5,
                    blend: Mix::Screen.into(),
                },
                mask: mask.map(AppliedMask::new),
                clip: clipped.then_some(Clip::Fill {
                    transform: Affine::IDENTITY,
                    shape: Geometry::Rect(Rect::new(0., 0., 200., 300.)),
                    fill_rule: Default::default(),
                }),
                ..Default::default()
            });
            drawing.append_transformed(&scene(Color::from_rgb8(0, 0, 255)), Affine::IDENTITY);
            drawing.pop_group();
            let frame = frame(drawing, [400, 300], &pool);
            let expected_targets = if masked || clipped { 4 } else { 3 };
            let planned =
                super::super::targets::peak_targets(&frame, [400, 300], false, 4, |_| Ok(()))
                    .unwrap();
            assert_eq!(planned, expected_targets);
            // Exactly one-sample RGBA16F storage: an omitted backdrop in planning
            // would choose a quality whose replay does not fit this budget.
            let mut renderer = renderer(super::super::GpuLimits {
                target_bytes: expected_targets * 400 * 300 * 8,
                ..Default::default()
            });
            let prepared = renderer.prepare(&frame).unwrap();
            assert_eq!(prepared.quality().samples, 1);
            assert_eq!(prepared.quality().format, wgpu::TextureFormat::Rgba16Float);
            let pixels = renderer.readback(prepared).unwrap();
            let coverage = 0.5 * if masked { 128. / 255. } else { 1. };
            for (index, pixel) in pixels.iter().enumerate() {
                let blue = if clipped && index % 400 >= 200 {
                    0.
                } else {
                    coverage
                };
                for (actual, expected) in pixel.iter().zip([1., 0., blue, 1.]) {
                    assert!(
                        (*actual - expected).abs() < 0.006,
                        "{pixel:?}, mask={masked}, clip={clipped}"
                    );
                }
            }
            empty(&renderer);
        }
    }
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
                reel_ui::peniko::Mix::ColorDodge,
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
        attempted[3], 4,
        "paint, video and both group pipelines were constructed"
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
            "paired_samples=30 size={:?} quality={:?} cold_p50_us={:.3} cold_p95_us={:.3} warm_p50_us={:.3} warm_p95_us={:.3}; per_renderer_creations=[3,5,5,5,4]",
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
    assert_eq!(attempted[3], 4);
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
                [3, 5, 5, 5, 4],
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

#[test]
#[ignore = "requires actual graphics adapter"]
fn subtree_invert_pixels_preserve_alpha_nesting_and_siblings() {
    use reel_ui::{
        core::Widget,
        kurbo::{Point, Size},
    };
    let pool = pool();
    let mut renderer = renderer(Default::default());
    eprintln!("invert adapter={:?}", renderer.gpu.adapter.get_info());
    let build_widget = |widget| {
        let mut bridge = widget_bridge(widget, Affine::IDENTITY);
        build(
            bridge.record(FrameId(1)).unwrap(),
            empty_snapshot(&pool),
            &pool,
        )
    };
    // Two overlapping translucent children must be inverted after composition.
    let mut painted = scene(Color::from_rgba8(128, 64, 32, 128));
    painted.append_transformed(
        &scene(Color::from_rgba8(32, 128, 64, 128)),
        Affine::IDENTITY,
    );
    let mut widget = RecordWidget::new(painted);
    widget.clip = false;
    widget.opacity = 0.5;
    widget.invert = 1.;
    let frame = build_widget(widget);
    assert_eq!(
        frame
            .commands()
            .iter()
            .filter(|c| matches!(c.op, reel_ui::frame::FrameOp::PushGroup(_)))
            .count(),
        1,
        "opacity and inversion share one compositing group"
    );
    let pixels = renderer
        .readback(renderer.prepare(&frame).unwrap())
        .unwrap();
    let linear = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let encoded = |v: f32| {
        if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1. / 2.4) - 0.055
        }
    };
    let a = 128. / 255.;
    let alpha = a + a * (1. - a);
    let expected: [f32; 4] = std::array::from_fn(|i| {
        if i == 3 {
            return alpha * 0.5;
        }
        let first = [128., 64., 32.][i] / 255.;
        let second = [32., 128., 64.][i] / 255.;
        let composed = (linear(second) * a + linear(first) * a * (1. - a)) / alpha;
        linear(1. - encoded(composed)) * alpha * 0.5
    });
    for pixel in &pixels {
        for i in 0..4 {
            assert!(
                (pixel[i] - expected[i]).abs() < 0.006,
                "{pixel:?} != {expected:?}"
            );
        }
    }
    let mut outer = RecordWidget::new(Scene::new());
    outer.clip = false;
    let mut parent = RecordWidget::new(Scene::new());
    parent.clip = false;
    parent.invert = 1.;
    let mut child = RecordWidget::new(scene(Color::from_rgb8(128, 64, 32)));
    child.clip = true;
    child.inner.clip = Rect::new(0., 0., 100., 100.);
    child.invert = 1.;
    parent
        .inner
        .add(child.prepare(), Point::ZERO, Size::new(100., 100.));
    outer
        .inner
        .add(parent.prepare(), Point::ZERO, Size::new(100., 100.));
    let mut sibling = RecordWidget::new(scene(Color::from_rgb8(32, 128, 64)));
    sibling.clip = true;
    sibling.inner.clip = Rect::new(0., 0., 100., 100.);
    outer.inner.add(
        sibling.prepare(),
        Point::new(200., 0.),
        Size::new(100., 100.),
    );
    let frame = build_widget(outer);
    let pixels = renderer
        .readback(renderer.prepare(&frame).unwrap())
        .unwrap();
    for (x, rgb) in [(50, [128., 64., 32.]), (250, [32., 128., 64.])] {
        let p = pixels[50 * 400 + x];
        for i in 0..3 {
            assert!(
                (p[i] - linear(rgb[i] / 255.)).abs() < 0.006,
                "nested/sibling {p:?}"
            );
        }
        assert!((p[3] - 1.).abs() < 0.006);
    }
    assert_eq!(
        pixels[150 * 400 + 150],
        [0.; 4],
        "transparent pixels stay transparent"
    );
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn subtree_invert_and_image_threshold_share_composite() {
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::{
        core::PaintImageMask,
        peniko::{ImageAlphaType, ImageFormat},
        resources::{ColorEncoding, ResourceRegistry},
    };
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 65536).unwrap();
    let handle = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("invert-mask".into()),
                version: ContentVersion([1; 32]),
            },
            OwnedBytes::new(vec![48, 96, 192, 128], pool.reserve(4).unwrap()).unwrap(),
            [1, 1],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let lease = registry.acquire(handle).unwrap();
    let mut renderer = renderer(Default::default());
    let program = reel_ui::stage::MaskProgram::image("mask".into());
    for (progress, invert) in [
        (0., false),
        (0.2, false),
        (0.5, false),
        (0.5, true),
        (1., false),
    ] {
        let mut widget = RecordWidget::new(scene(Color::from_rgb8(255, 0, 0)));
        widget.clip = false;
        widget.invert = 1.;
        widget.opacity = 0.5;
        widget.mask = Some(PaintImageMask {
            program: program.clone(),
            images: vec![lease.clone()].into(),
            bounds: Rect::new(0., 0., 400., 300.),
            parameters: [progress, 0., 0., 0., f32::from(invert)],
        });
        let mut bridge = widget_bridge(widget, Affine::IDENTITY);
        let frame = build(
            bridge.record(FrameId(1)).unwrap(),
            registry.freeze(&[handle], &pool).unwrap(),
            &pool,
        );
        assert_eq!(
            frame
                .commands()
                .iter()
                .filter(|c| matches!(c.op, reel_ui::frame::FrameOp::PushGroup(_)))
                .count(),
            usize::from(progress != 0.),
            "zero progress suppresses the subtree; otherwise all effects share one group"
        );
        let pixels = renderer
            .readback(renderer.prepare(&frame).unwrap())
            .unwrap();
        let coverage = if progress == 0. || progress == 0.2 || invert {
            0.
        } else if progress == 1. {
            0.5
        } else {
            0.5 * 128. / 255.
        };
        for p in pixels {
            assert!(p[0].abs() < 0.006);
            assert!(
                p[1..].iter().all(|v| (*v - coverage).abs() < 0.006),
                "{p:?} at progress {progress}"
            );
        }
    }
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn composed_masks_transform_sample_animate_and_retire() {
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::{
        core::PaintImageMask,
        peniko::{ImageAlphaType, ImageFormat},
        resources::{ColorEncoding, ResourceRegistry},
        stage::{MaskChannel, MaskOp, MaskProgram},
    };
    use std::sync::Arc;
    let pool = pool();
    let mut registry = ResourceRegistry::new(2, 65536).unwrap();
    let mut handles = Vec::new();
    let mut images = Vec::new();
    let mut versions = Vec::new();
    let alphas = [[0_u8, 128, 192, 255], [255_u8, 64, 128, 0]];
    for (index, values) in alphas.iter().enumerate() {
        let bytes = values
            .iter()
            .flat_map(|&a| [19, 231, 75, a])
            .collect::<Vec<_>>();
        let version = ResourceVersion {
            key: AssetKey(format!("mask-{index}")),
            version: ContentVersion([index as u8; 32]),
        };
        let handle = registry
            .register_image(
                version.clone(),
                OwnedBytes::new(bytes, pool.reserve(16).unwrap()).unwrap(),
                [2, 2],
                ImageFormat::Rgba8,
                ImageAlphaType::Alpha,
                ColorEncoding::Srgb,
            )
            .unwrap();
        let lease = registry.acquire(handle).unwrap();
        images.push(lease.clone());
        handles.push(handle);
        versions.push(version);
    }
    let c = |v: f32| MaskOp::Constant(v.to_bits());
    let program = MaskProgram::new(
        vec![
            c(0.),
            c(1.),
            c(-1.),
            MaskOp::Parameter(2),
            MaskOp::Parameter(0),
            MaskOp::Sample {
                image: 0,
                channel: MaskChannel::Alpha,
                coordinates: [3, 0, 1, 1],
                repeat: 1,
            },
            MaskOp::Sample {
                image: 1,
                channel: MaskChannel::Alpha,
                coordinates: [0, 1, 1, 2],
                repeat: 0,
            },
            MaskOp::Pow(5, 2_f32.to_bits()),
            MaskOp::Multiply(6, 4),
            MaskOp::Add(7, 8),
            MaskOp::Clamp(9, 0_f32.to_bits(), 1_f32.to_bits()),
            c(0.25),
            c(0.75),
            MaskOp::Smoothstep(11, 12, 10),
            c(0.7),
            MaskOp::Multiply(13, 14),
        ],
        vec!["a".into(), "b".into()],
    )
    .unwrap();
    let transform =
        Affine::translate((20., 30.)) * Affine::rotate(0.2) * Affine::scale_non_uniform(0.75, 0.6);
    let frame = |progress, offset| {
        let mut widget = RecordWidget::new(scene(Color::WHITE));
        widget.clip = false;
        widget.mask = Some(PaintImageMask {
            program: program.clone(),
            images: images.clone().into(),
            bounds: Rect::new(0., 0., 400., 300.),
            parameters: [progress, 0., offset, 0., 0.],
        });
        let mut bridge = widget_bridge(widget, transform);
        build(
            bridge.record(FrameId(1)).unwrap(),
            registry.freeze(&handles, &pool).unwrap(),
            &pool,
        )
    };
    let sample = |values: [u8; 4], uv: [f64; 2], repeat: bool| {
        let [x, y] = uv.map(|v| v * 2. - 0.5);
        let ix = x.floor() as i32;
        let iy = y.floor() as i32;
        let at = |x: i32, y: i32| {
            values[(y.clamp(0, 1) * 2
                + if repeat {
                    x.rem_euclid(2)
                } else {
                    x.clamp(0, 1)
                }) as usize] as f64
                / 255.
        };
        let top = at(ix, iy) * (1. - x.fract().rem_euclid(1.))
            + at(ix + 1, iy) * x.fract().rem_euclid(1.);
        let bottom = at(ix, iy + 1) * (1. - x.fract().rem_euclid(1.))
            + at(ix + 1, iy + 1) * x.fract().rem_euclid(1.);
        top * (1. - (y - y.floor())) + bottom * (y - y.floor())
    };
    let mut renderer = renderer(Default::default());
    eprintln!("mask adapter={:?}", renderer.gpu.adapter.get_info());
    let quality = renderer.gpu.quality([400, 300]).unwrap();
    let pipelines = renderer.pipelines.get(&renderer.gpu, quality).unwrap();
    let mut pipeline_identity = None;
    for (progress, offset) in [(0., 0.), (0.4, 0.25), (1., 0.75)] {
        let frame = frame(progress, offset);
        let start = Instant::now();
        let prepared = renderer.prepare(&frame).unwrap();
        eprintln!(
            "mask prepare progress={progress} elapsed_us={}",
            start.elapsed().as_micros()
        );
        let compiled = pipelines
            .masks
            .get(
                &renderer.gpu,
                quality,
                &program,
                &renderer.budgets.resources,
            )
            .unwrap();
        if let Some(weak) = &pipeline_identity {
            assert!(Arc::ptr_eq(
                &compiled,
                &std::sync::Weak::upgrade(weak).unwrap()
            ));
        } else {
            pipeline_identity = Some(Arc::downgrade(&compiled));
        }
        assert_eq!(pipelines.masks.len(), 1);
        let pixels = renderer.readback(prepared).unwrap();
        for y in (8..290).step_by(19) {
            for x in (8..390).step_by(23) {
                let local = transform.inverse()
                    * reel_ui::kurbo::Point::new(x as f64 + 0.5, y as f64 + 0.5);
                let uv = [local.x / 400., local.y / 300.];
                if uv
                    .iter()
                    .any(|v| v.abs() < 0.015 || (*v - 1.).abs() < 0.015)
                {
                    continue;
                }
                let expected = if uv.iter().any(|v| !(0. ..=1.).contains(v)) {
                    0.
                } else {
                    let a = sample(alphas[0], [uv[0] + offset as f64, uv[1]], true);
                    let b = sample(alphas[1], [uv[0], 1. - uv[1]], false);
                    let t = ((a * a + b * progress as f64 - 0.25) / 0.5).clamp(0., 1.);
                    t * t * (3. - 2. * t) * 0.7
                };
                assert!(
                    pixels[y * 400 + x]
                        .iter()
                        .all(|v| (*v as f64 - expected).abs() < 0.008),
                    "pixel ({x},{y}) uv={uv:?} actual={:?} expected={expected}",
                    pixels[y * 400 + x]
                );
            }
        }
    }
    let equivalent =
        MaskProgram::new(program.operations().to_vec(), vec!["c".into(), "d".into()]).unwrap();
    let compiled = pipelines
        .masks
        .get(
            &renderer.gpu,
            quality,
            &equivalent,
            &renderer.budgets.resources,
        )
        .unwrap();
    assert!(
        Arc::ptr_eq(
            &compiled,
            &pipeline_identity.as_ref().unwrap().upgrade().unwrap()
        ),
        "changing image paths preserves a shared structure pipeline"
    );
    drop(compiled);
    drop(equivalent);
    let last = frame(1., 0.75);
    let held = renderer.prepare(&last).unwrap();
    drop(last);
    drop(program);
    drop(images);
    pipelines.masks.prune();
    assert_eq!(pipelines.masks.len(), 1, "prepared frame owns the program");
    drop(held);
    let before = renderer.usage().resources;
    pipelines.masks.prune();
    assert_eq!(pipelines.masks.len(), 0);
    assert!(pipeline_identity.unwrap().upgrade().is_none());
    assert!(
        renderer.usage().resources < before,
        "released pipeline metadata refunds its ticket"
    );
    renderer.retire(&versions);
    assert_eq!(
        renderer.usage().resources,
        0,
        "retired textures and pipelines refund all resource storage"
    );
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn mask_expression_math_edges_and_budget_admission() {
    use reel_ui::{
        core::PaintImageMask,
        stage::{MaskOp, MaskProgram},
    };
    let mut renderer = renderer(Default::default());
    let pool = pool();
    let c = |v: f32| MaskOp::Constant(v.to_bits());
    let cases = [
        (vec![c(-2.), MaskOp::Pow(0, 2_f32.to_bits())], 0.),
        (vec![c(0.), MaskOp::Pow(0, 0_f32.to_bits())], 1.),
        (
            vec![c(1.), c(0.), c(0.25), MaskOp::Smoothstep(0, 1, 2)],
            0.84375,
        ),
        (vec![c(0.5), MaskOp::Smoothstep(0, 0, 0)], 1.),
        (vec![c(0.5), c(0.49), MaskOp::Smoothstep(0, 0, 1)], 0.),
    ];
    let quality = renderer.gpu.quality([400, 300]).unwrap();
    let pipelines = renderer.pipelines.get(&renderer.gpu, quality).unwrap();
    for (operations, expected) in cases {
        let program = MaskProgram::new(operations, vec![]).unwrap();
        let mut widget = RecordWidget::new(scene(Color::WHITE));
        widget.clip = false;
        widget.mask = Some(PaintImageMask {
            program,
            images: Vec::new().into(),
            bounds: Rect::new(0., 0., 400., 300.),
            parameters: [1., 0., 0., 0., 0.],
        });
        let mut bridge = widget_bridge(widget, Affine::IDENTITY);
        let frame = build(
            bridge.record(FrameId(1)).unwrap(),
            empty_snapshot(&pool),
            &pool,
        );
        let pixels = renderer
            .readback(renderer.prepare(&frame).unwrap())
            .unwrap();
        assert!(
            pixels[150 * 400 + 200]
                .iter()
                .all(|v| (v - expected).abs() < 0.006),
            "{expected} {:?}",
            pixels[150 * 400 + 200]
        );
    }
    pipelines.masks.prune();
    assert_eq!(pipelines.masks.len(), 0);
    let program = MaskProgram::new(vec![c(0.5)], vec![]).unwrap();
    let empty = reel_core::budget::BudgetPool::new(
        reel_core::budget::BudgetCategory::GpuResource,
        reel_core::budget::QueueLimits {
            max_requests: 100,
            max_bytes: 1,
        },
    );
    assert!(matches!(
        pipelines
            .masks
            .get(&renderer.gpu, quality, &program, &empty),
        Err(GpuError::Budget { .. })
    ));
    assert_eq!(empty.usage().bytes, 0);
    assert_eq!(pipelines.masks.len(), 0);
    let too_many = renderer
        .gpu
        .device
        .limits()
        .max_sampled_textures_per_shader_stage as usize;
    let program = MaskProgram::new(
        vec![c(0.5)],
        (0..too_many).map(|i| format!("image{i}").into()).collect(),
    )
    .unwrap();
    assert!(
        pipelines
            .masks
            .get(
                &renderer.gpu,
                quality,
                &program,
                &renderer.budgets.resources
            )
            .is_err()
    );
    assert_eq!(pipelines.masks.len(), 0);
    // Admission rejection leaves the renderer usable for the previously accepted content.
    let plain = frame(scene(Color::WHITE), [400, 300], &pool);
    let pixels = renderer
        .readback(renderer.prepare(&plain).unwrap())
        .unwrap();
    assert!(pixels[0].iter().all(|v| (*v - 1.).abs() < 0.006));
}
