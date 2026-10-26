#![cfg(feature = "wgpu")]
#[allow(dead_code, reason = "shared actual Masonry fixture")]
#[path = "../../reel-ui/tests/support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared R01 compiler fixture")]
#[path = "../../reel-ui/tests/support/r01_fixture.rs"]
mod r01_fixture;
use r01_fixture::*;
use reel_core::identity::FrameId;
use reel_render::gpu::{DeviceGeneration, Gpu, GpuError, GpuLimits, Renderer};
use reel_ui::frame::{FrameBuild, FrameInfo, RenderFrame, StepBudget};
use reel_ui::{
    imaging::{
        Composite, MaskMode,
        record::{AppliedMask, Clip, Draw, Geometry, Group, Mask, Scene},
    },
    kurbo::{Affine, BezPath, Join, Rect, Stroke},
    peniko::{Brush, Color},
};

fn renderer_with_limits(limits: GpuLimits) -> Renderer {
    let instance = wgpu::Instance::default();
    Renderer::new(
        pollster::block_on(Gpu::request(&instance, None, DeviceGeneration(1), limits))
            .expect("actual adapter required"),
    )
}
fn fill(scene: &mut Scene) {
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::WHITE),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(10., 10., 200., 200.)),
        composite: Composite::default(),
    });
}
fn frame(scene: Scene, pool: &reel_core::budget::BudgetPool) -> RenderFrame {
    let mut widget = RecordWidget::new(scene);
    widget.clip = false;
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    build(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(pool),
        pool,
    )
}
#[test]
#[ignore = "requires actual graphics adapter"]
fn draw_and_clip_strokes_accept_small_miters_without_panic_or_leak() {
    let mut renderer = renderer_with_limits(GpuLimits::default());
    eprintln!("adapter={:?}", renderer.gpu().adapter.get_info());
    let pool = pool();
    for join in [Join::Miter, Join::Bevel, Join::Round] {
        for limit in [0., 0.5, 1.] {
            for clip in [false, true] {
                let shape = Geometry::Rect(Rect::new(20., 20., 100., 100.));
                let stroke = Stroke::new(8.).with_join(join).with_miter_limit(limit);
                let frame = frame(stroke_scene(shape.clone(), stroke, clip), &pool);
                let prepared = renderer.prepare(&frame).unwrap();
                let pixels = renderer.readback(prepared).unwrap();
                assert!(pixels.iter().any(|p| p[3] > 0.9));
                if join == Join::Miter && limit < 1. {
                    let reference = frame_for_stroke(
                        &mut renderer,
                        &pool,
                        shape,
                        Stroke::new(8.).with_join(Join::Bevel),
                        clip,
                    );
                    assert_eq!(pixels, reference, "miter below one must bevel");
                }
                assert_eq!(renderer.usage().targets, 0);
                assert_eq!(renderer.usage().resources, 0);
                assert_eq!(renderer.usage().transient, renderer.usage().cached_geometry);
            }
        }
    }
}

fn stroke_scene(shape: Geometry, stroke: Stroke, clip: bool) -> Scene {
    let mut scene = Scene::new();
    if clip {
        scene.push_clip(Clip::Stroke {
            transform: Affine::IDENTITY,
            shape,
            stroke,
        });
        fill(&mut scene);
        scene.pop_clip();
    } else {
        scene.draw(Draw::Stroke {
            transform: Affine::IDENTITY,
            shape,
            stroke,
            brush: Brush::Solid(Color::WHITE),
            brush_transform: None,
            composite: Composite::default(),
        });
    }
    scene
}
fn frame_for_stroke(
    renderer: &mut Renderer,
    pool: &reel_core::budget::BudgetPool,
    shape: Geometry,
    stroke: Stroke,
    clip: bool,
) -> Vec<[f32; 4]> {
    let frame = frame(stroke_scene(shape, stroke, clip), pool);
    let prepared = renderer.prepare(&frame).unwrap();
    renderer.readback(prepared).unwrap()
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn dashed_draw_and_clip_follow_offsets_patterns_and_closed_curves() {
    use reel_ui::kurbo::Cap;
    let mut renderer = renderer_with_limits(GpuLimits::default());
    let pool = pool();
    let mut line = BezPath::new();
    line.move_to((20., 40.));
    line.line_to((180., 40.));
    for (pattern, offset, on_start, on_end, period) in [
        (vec![12., 8.], 0., 0., 12., 20.),
        (vec![12., 8.], 4., 0., 12., 20.),
        (vec![12., 8.], -4., 0., 12., 20.),
        (vec![12.], 3., 0., 12., 24.),
        (vec![0., 8., 12., 0.], 0., 8., 20., 20.),
        (vec![0., 0.], 0., 0., 20., 20.),
    ] {
        for clip in [false, true] {
            let stroke = Stroke::new(8.)
                .with_caps(Cap::Butt)
                .with_dashes(offset, pattern.clone());
            let pixels = frame_for_stroke(
                &mut renderer,
                &pool,
                Geometry::Path(line.clone()),
                stroke,
                clip,
            );
            for x in 20..180 {
                let phase = (x as f64 + 0.5 - 20. + offset).rem_euclid(period);
                let expected = if phase >= on_start && phase < on_end {
                    1.
                } else {
                    0.
                };
                assert!(
                    (pixels[40 * 400 + x][3] - expected).abs() < 0.01,
                    "pattern={pattern:?} offset={offset} clip={clip} x={x}"
                );
            }
        }
    }
    let mut curve = BezPath::new();
    curve.move_to((40., 60.));
    curve.curve_to((40., 20.), (150., 20.), (150., 60.));
    curve.line_to((150., 150.));
    curve.line_to((40., 150.));
    curve.close_path();
    let solid = frame_for_stroke(
        &mut renderer,
        &pool,
        Geometry::Path(curve.clone()),
        Stroke::new(8.),
        false,
    );
    let stroke = Stroke::new(8.).with_dashes(-7., [16., 8.]);
    let drawn = frame_for_stroke(
        &mut renderer,
        &pool,
        Geometry::Path(curve.clone()),
        stroke.clone(),
        false,
    );
    let clipped = frame_for_stroke(
        &mut renderer,
        &pool,
        Geometry::Path(curve.clone()),
        stroke,
        true,
    );
    for (draw, clip) in drawn.iter().zip(&clipped) {
        assert!((draw[3] - clip[3]).abs() < 0.002);
    }
    let coverage = |pixels: &[[f32; 4]]| pixels.iter().map(|p| f64::from(p[3])).sum::<f64>();
    assert!(coverage(&drawn) > 0.4 * coverage(&solid));
    assert!(coverage(&drawn) < 0.95 * coverage(&solid));

    let overflowing = frame(
        stroke_scene(
            Geometry::Path(curve.clone()),
            Stroke::new(8.).with_dashes(-1., [f64::MAX, f64::MAX]),
            false,
        ),
        &pool,
    );
    let at = overflowing
        .commands()
        .iter()
        .find_map(|command| {
            matches!(
                command.op,
                reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::Stroke { .. })
            )
            .then_some(command.origin.at)
        })
        .unwrap();
    assert!(
        matches!(renderer.prepare(&overflowing), Err(GpuError::Unsupported {at: actual, ..}) if actual == at)
    );
    assert_eq!(renderer.usage().transient, renderer.usage().cached_geometry);
    assert_eq!(renderer.usage().targets, 0);

    // Actual emitted mesh capacity remains governed by the existing quota.
    let limits = GpuLimits {
        transient_bytes: 16384,
        ..Default::default()
    };
    let small = renderer_with_limits(limits);
    let frame = frame(
        stroke_scene(
            Geometry::Path(curve),
            Stroke::new(8.).with_dashes(0., [1., 1.]),
            false,
        ),
        &pool,
    );
    assert!(matches!(
        small.prepare(&frame),
        Err(GpuError::Budget {
            domain: "mesh scratch",
            ..
        })
    ));
    assert_eq!(small.usage().transient, 0);
    assert_eq!(small.usage().targets, 0);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn shared_pressure_and_completion_differ_from_single_frame_oversize() {
    let limits = GpuLimits {
        target_bytes: 8 << 20,
        max_inflight: 32,
        ..Default::default()
    };
    let mut renderer = renderer_with_limits(limits);
    let pool = pool();
    let mut scene = Scene::new();
    // One author path exceeds the old 1 MiB tessellation ceiling while the
    // existing pressure checks still distinguish shared from permanent demand.
    let mut path = BezPath::new();
    for y in 0..64 {
        for x in 0..128 {
            let (x, y) = (f64::from(x) * 3., f64::from(y) * 4.);
            path.move_to((x, y));
            path.line_to((x + 2.9, y));
            path.line_to((x + 2.9, y + 3.9));
            path.line_to((x, y + 3.9));
            path.close_path();
        }
    }
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::WHITE),
        brush_transform: None,
        shape: Geometry::Path(path),
        composite: Composite::default(),
    });
    let simple = frame(scene, &pool);
    let mut held = Vec::new();
    let mut pressure = false;
    // Allow either quality fallback: even RGBA8/1x exhausts 8 MiB before 32 slots.
    for _ in 0..limits.max_inflight {
        match renderer.prepare(&simple) {
            Ok(prepared) => {
                assert!(renderer.usage().transient > 1 << 20);
                held.push(prepared);
            }
            Err(GpuError::Pressure {
                domain: "targets", ..
            }) => {
                pressure = true;
                break;
            }
            _ => panic!("only shared target pressure may stop this frame"),
        }
    }
    assert!(pressure && !held.is_empty());
    let used = renderer.usage().targets;
    assert!(used > 0 && used <= limits.target_bytes);
    assert!(matches!(
        renderer.prepare(&simple),
        Err(GpuError::Pressure {
            domain: "targets",
            ..
        })
    ));
    assert_eq!(renderer.usage().targets, used);
    let ids: Vec<_> = held
        .into_iter()
        .map(|prepared| renderer.submit(prepared).unwrap())
        .collect();
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert_eq!(renderer.usage().targets, used);
    assert_eq!(renderer.poll().unwrap(), ids);
    drop(
        renderer
            .prepare(&simple)
            .expect("completion makes the exact same state admissible"),
    );
    assert_eq!(renderer.usage().targets, 0);
    let mut large = Scene::new();
    for _ in 0..18 {
        large.push_group(Group::default());
    }
    fill(&mut large);
    for _ in 0..18 {
        large.pop_group();
    }
    let large = frame(large, &pool);
    let error = match renderer.prepare(&large) {
        Err(e) => e,
        Ok(_) => panic!("nineteen targets exceed quota even with RGBA8/1x"),
    };
    assert!(matches!(
        error,
        GpuError::Budget {
            domain: "targets",
            ..
        }
    ));
    assert!(!error.is_retryable());
    assert_eq!(renderer.usage().targets, 0);
}
#[test]
#[ignore = "requires actual graphics adapter"]
fn deeply_nested_masks_use_target_budget_and_preserve_pixels() {
    let pool = pool();
    let mut scene = Scene::new();
    fill(&mut scene);
    for _ in 0..96 {
        let mut outer = Scene::new();
        let mask = outer.define_mask(Mask::new(MaskMode::Alpha, scene));
        outer.push_group(Group {
            mask: Some(AppliedMask::new(mask)),
            ..Default::default()
        });
        fill(&mut outer);
        outer.pop_group();
        scene = outer;
    }
    let mut widget = RecordWidget::new(scene);
    widget.clip = false;
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [16, 16],
        },
        limits(),
        &pool,
    )
    .unwrap();
    let mut budget = StepBudget {
        max_commands: 64,
        max_units: 8192,
        max_bytes: 64 << 10,
    };
    loop {
        let progress = build.step(budget).unwrap();
        if progress.complete {
            break;
        }
        budget.max_units = budget.max_units.max(progress.minimum_units);
        budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
    }
    let frame = build.finish().unwrap();
    let low = renderer_with_limits(GpuLimits {
        target_bytes: 8 << 10,
        ..Default::default()
    });
    let error = match low.prepare(&frame) {
        Err(e) => e,
        Ok(_) => panic!("nested masks must reserve actual targets"),
    };
    assert!(matches!(
        error,
        GpuError::Budget {
            domain: "targets",
            ..
        }
    ));
    assert!(!error.is_retryable());
    assert_eq!(low.usage().targets, 0);
    assert_eq!(low.usage().transient, 0);
    let mut exact = renderer_with_limits(GpuLimits::default());
    eprintln!("96 nested masks: {:?}", exact.gpu().adapter.get_info());
    let pixels = exact.readback(exact.prepare(&frame).unwrap()).unwrap();
    assert!(pixels.iter().any(|p| p[3] > 0.9));
    assert_eq!(exact.usage().targets, 0);
    assert_eq!(exact.usage().transient, exact.usage().cached_geometry);
}
