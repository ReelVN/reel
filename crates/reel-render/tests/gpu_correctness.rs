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
use reel_ui::frame::{FrameOp, RenderFrame};
use reel_ui::{
    imaging::{
        Composite, MaskMode,
        record::{AppliedMask, Clip, Draw, Geometry, Group, Mask, Scene},
    },
    kurbo::{Affine, Join, Rect, Stroke},
    peniko::{Brush, Color},
};

fn renderer(limits: GpuLimits) -> Renderer {
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
fn draw_and_clip_strokes_reject_small_miters_without_panic_or_leak() {
    let mut renderer = renderer(GpuLimits::default());
    let pool = pool();
    for join in [Join::Miter, Join::Bevel, Join::Round] {
        for limit in [0., 0.5, 1.] {
            for clip in [false, true] {
                let stroke = Stroke::new(8.).with_join(join).with_miter_limit(limit);
                let mut scene = Scene::new();
                if clip {
                    scene.push_clip(Clip::Stroke {
                        transform: Affine::IDENTITY,
                        shape: Geometry::Rect(Rect::new(20., 20., 100., 100.)),
                        stroke,
                    });
                    fill(&mut scene);
                    scene.pop_clip();
                } else {
                    scene.draw(Draw::Stroke {
                        transform: Affine::IDENTITY,
                        shape: Geometry::Rect(Rect::new(20., 20., 100., 100.)),
                        stroke,
                        brush: Brush::Solid(Color::WHITE),
                        brush_transform: None,
                        composite: Composite::default(),
                    });
                }
                let frame = frame(scene, &pool); // R01 legally accepts every miter in this matrix.
                let expected = frame
                    .commands()
                    .iter()
                    .find(|c| {
                        if clip {
                            matches!(c.op, FrameOp::PushClip(Clip::Stroke { .. }))
                        } else {
                            matches!(
                                c.op,
                                FrameOp::Draw(reel_ui::frame::FrameDraw::Stroke { .. })
                            )
                        }
                    })
                    .unwrap()
                    .origin
                    .at;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    renderer.prepare(&frame)
                }))
                .expect("GPU stroke must never unwind");
                if limit < 1. {
                    assert!(
                        matches!(result, Err(GpuError::Unsupported { at, .. }) if at == expected),
                        "join={join:?} limit={limit} clip={clip}"
                    );
                    assert_eq!(renderer.usage().targets, 0);
                    assert_eq!(renderer.usage().resources, 0);
                    assert_eq!(renderer.usage().transient, 0);
                } else {
                    let pixels = renderer.readback(result.unwrap()).unwrap();
                    assert!(pixels.iter().any(|p| p[3] > 0.9));
                }
                // Failure must release both admission and residency, allowing the next case.
                assert_eq!(renderer.usage().targets, 0);
            }
        }
    }
}
#[test]
#[ignore = "requires actual graphics adapter"]
fn shared_pressure_and_completion_differ_from_single_frame_oversize() {
    let limits = GpuLimits {
        target_bytes: 8 << 20,
        max_inflight: 32,
        ..Default::default()
    };
    let mut renderer = renderer(limits);
    let pool = pool();
    let mut scene = Scene::new();
    fill(&mut scene);
    let simple = frame(scene, &pool);
    let mut held = Vec::new();
    let mut pressure = false;
    // Allow either quality fallback: even RGBA8/1x exhausts 8 MiB before 32 slots.
    for _ in 0..limits.max_inflight {
        match renderer.prepare(&simple) {
            Ok(prepared) => held.push(prepared),
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
    renderer.gpu().device.poll(wgpu::PollType::Wait).unwrap();
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
fn recursive_mask_depth_reports_original_location_and_boundary() {
    let pool = pool();
    let mut scene = Scene::new();
    let mut coverage = Scene::new();
    let mut leaf = Scene::new();
    fill(&mut leaf);
    let inner = coverage.define_mask(Mask::new(MaskMode::Alpha, leaf));
    coverage.push_group(Group {
        mask: Some(AppliedMask::new(inner)),
        ..Default::default()
    });
    fill(&mut coverage);
    coverage.pop_group();
    let mask = scene.define_mask(Mask::new(MaskMode::Alpha, coverage));
    scene.push_group(Group {
        mask: Some(AppliedMask::new(mask)),
        ..Default::default()
    });
    fill(&mut scene);
    scene.pop_group();
    let frame = frame(scene, &pool);
    let low = renderer(GpuLimits {
        max_depth: 2,
        target_bytes: 256 << 20,
        ..Default::default()
    });
    let error = match low.prepare(&frame) {
        Err(e) => e,
        Ok(_) => panic!("mask recursion must count root plus nested scopes"),
    };
    assert!(matches!(error, GpuError::DepthLimit { at, limit: 2 } if at.mask_depth > 0));
    assert!(!error.is_retryable());
    assert_eq!(low.usage().targets, 0);
    let mut exact = renderer(GpuLimits {
        max_depth: 3,
        target_bytes: 256 << 20,
        ..Default::default()
    });
    let pixels = exact.readback(exact.prepare(&frame).unwrap()).unwrap();
    assert!(pixels.iter().any(|p| p[3] > 0.9));
}
