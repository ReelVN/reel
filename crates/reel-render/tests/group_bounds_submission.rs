#![cfg(any(feature = "wgpu", feature = "gles2"))]
//! Submit bounded isolated groups through production replay without reading pixels.
#[allow(dead_code, reason = "shared UI fixture")]
#[path = "../../reel-ui/tests/support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared frame compiler fixture")]
#[path = "../../reel-ui/tests/support/r01_fixture.rs"]
mod r01_fixture;

use r01_fixture::*;
use reel_core::identity::FrameId;
use reel_ui::{
    frame::{FrameBuild, FrameInfo, FrameOp, InkBounds, RenderFrame},
    imaging::{
        Composite, MaskMode,
        record::{AppliedMask, Draw, Geometry, Group, Mask, Scene},
    },
    kurbo::{Affine, Rect},
    peniko::{Brush, Color},
};

const SIZE: [u32; 2] = [960, 544];
const TARGET_BYTES: u64 = SIZE[0] as u64 * SIZE[1] as u64 * 4;
const GROUPS: u64 = 16;
const WARM_FRAMES: usize = 40;
#[cfg(feature = "gles2")]
static GLES_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn fill(scene: &mut Scene, rect: Rect, transform: Affine) {
    scene.draw(Draw::Fill {
        transform,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::from_rgba8(64, 128, 192, 192)),
        brush_transform: None,
        shape: Geometry::Rect(rect),
        composite: Composite::default(),
    });
}

fn frame() -> RenderFrame {
    let mut mask_scene = Scene::new();
    fill(
        &mut mask_scene,
        Rect::new(0., 0., 960., 544.),
        Affine::IDENTITY,
    );
    let mut scene = Scene::new();
    let mask = scene.define_mask(Mask::new(MaskMode::Alpha, mask_scene));
    for index in 0..GROUPS {
        scene.push_group(Group {
            mask: Some(AppliedMask::new(mask)),
            composite: Composite {
                alpha: 0.5,
                ..Default::default()
            },
            ..Default::default()
        });
        let transform = Affine::translate((
            40.5 + (index % 4) as f64 * 220.,
            30.5 + (index / 4) as f64 * 120.,
        )) * Affine::rotate(0.13);
        // Overlapping source primitives require group isolation; opacity cannot
        // be distributed to the draws. The mask is much larger than the source.
        for x in [0., 40.] {
            fill(&mut scene, Rect::new(x, 0., x + 80., 36.), transform);
        }
        scene.pop_group();
    }
    // Empty sources and a subsequent ordinary draw exercise scope restoration.
    scene.push_group(Group {
        composite: Composite {
            alpha: 0.5,
            ..Default::default()
        },
        ..Default::default()
    });
    scene.pop_group();
    fill(
        &mut scene,
        Rect::new(900., 500., 940., 530.),
        Affine::IDENTITY,
    );
    let mut widget = RecordWidget::new(scene);
    widget.clip = false;
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    let pool = pool();
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        FrameInfo {
            logical_size: [960., 544.],
            physical_size: SIZE,
        },
        limits(),
        &pool,
    )
    .unwrap();
    while !build.step(step_budget()).unwrap().complete {}
    let frame = build.finish().unwrap();
    let mut groups = 0;
    let mut pixels = 0;
    for command in frame.commands() {
        if let FrameOp::PushGroup(group) = &command.op
            && let InkBounds::Rect(rect) = group.ink_bounds
        {
            groups += 1;
            pixels += ((rect.x1.ceil() + 1.) - (rect.x0.floor() - 1.)) as u64
                * ((rect.y1.ceil() + 1.) - (rect.y0.floor() - 1.)) as u64;
        }
    }
    assert_eq!(groups, GROUPS);
    let previous = GROUPS * u64::from(SIZE[0]) * u64::from(SIZE[1]);
    assert!(pixels < previous / 50);
    eprintln!(
        "groups={GROUPS} full_composite_pixel_domain={previous} bounded_composite_pixel_domain={pixels} (coverage upper bounds, not GPU timings)"
    );
    frame
}

#[cfg(feature = "wgpu")]
#[test]
#[ignore = "requires an actual native WGPU adapter"]
fn wgpu_bounded_groups_and_retained_masks_submit() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    let frame = frame();
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits {
            target_bytes: TARGET_BYTES * 3,
            ..Default::default()
        },
    ))
    .unwrap();
    let mut renderer = Renderer::new(gpu);
    let mut measured = std::time::Instant::now();
    for index in 0..=WARM_FRAMES {
        let prepared = renderer.prepare(&frame).unwrap();
        assert_eq!(prepared.quality().samples, 1);
        assert_eq!(renderer.usage().targets, TARGET_BYTES * 3);
        renderer.submit(prepared).unwrap();
        renderer
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
        if index == 0 {
            measured = std::time::Instant::now();
        }
    }
    eprintln!(
        "wgpu bounded_groups frames={WARM_FRAMES} submit_complete_us={}",
        measured.elapsed().as_micros()
    );
}

#[cfg(feature = "gles2")]
#[test]
#[ignore = "requires a native GLES context"]
fn gles_bounded_groups_and_retained_masks_submit() {
    use reel_render::gles2::{GlesBackend, GlesLimits};
    use reel_render::{AlphaMode, RenderBackend, RenderEvent, SurfaceInfo};
    use reel_ui::resources::ColorEncoding;
    let _serial = GLES_TEST_LOCK.lock().unwrap();
    let frame = frame();
    let mut renderer = GlesBackend::headless(
        SIZE,
        GlesLimits {
            target_bytes: TARGET_BYTES * 3,
            ..Default::default()
        },
    )
    .unwrap();
    renderer
        .configure(SurfaceInfo {
            physical_size: SIZE,
            scale_factor: 1.,
            surface_epoch: frame.surface_epoch(),
            color_encoding: ColorEncoding::Srgb,
            alpha_mode: AlphaMode::Opaque,
        })
        .unwrap();
    let mut measured = std::time::Instant::now();
    let mut events = Vec::new();
    for index in 0..=WARM_FRAMES {
        renderer.render_present(&frame, || {}).unwrap();
        loop {
            events.clear();
            renderer.poll(&mut events, 32).unwrap();
            if events
                .iter()
                .any(|event| matches!(event, RenderEvent::RenderFinished { .. }))
            {
                break;
            }
            std::thread::yield_now();
        }
        if index == 0 {
            measured = std::time::Instant::now();
        }
    }
    eprintln!(
        "gles bounded_groups frames={WARM_FRAMES} submit_complete_us={}",
        measured.elapsed().as_micros()
    );
    // Prepare and cancel an independent output under the same three-target
    // ceiling. No poll_capture call or pixel readback is performed.
    let output = pool();
    let capture = reel_render::capture::CaptureBackend::capture(
        &mut renderer,
        &frame,
        SIZE,
        &output,
        reel_core::wake::WakeSignal::new(|| {}),
    )
    .unwrap();
    drop(capture);
    renderer.close().unwrap();
}

#[cfg(feature = "gles2")]
fn geometry_frame() -> RenderFrame {
    use reel_ui::kurbo::BezPath;
    let mut path = BezPath::new();
    // Disjoint contours retain 4096 vertices in one mesh. Its vertex storage
    // alone exceeds the 128 KiB left beside the previous three-target pool.
    for y in 0..32 {
        for x in 0..32 {
            let (x, y) = (f64::from(x) * 8., f64::from(y) * 8.);
            path.move_to((x, y));
            path.line_to((x + 6., y));
            path.line_to((x + 6., y + 6.));
            path.line_to((x, y + 6.));
            path.close_path();
        }
    }
    let mut scene = Scene::new();
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::WHITE),
        brush_transform: None,
        shape: Geometry::Path(path),
        composite: Composite::default(),
    });
    let mut widget = RecordWidget::new(scene);
    widget.clip = false;
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    let pool = pool();
    let mut build = FrameBuild::new(
        bridge.record(FrameId(2)).unwrap(),
        empty_snapshot(&pool),
        FrameInfo {
            logical_size: [960., 544.],
            physical_size: SIZE,
        },
        limits(),
        &pool,
    )
    .unwrap();
    while !build.step(step_budget()).unwrap().complete {}
    let frame = build.finish().unwrap();
    assert!(frame.commands().iter().all(|command| {
        matches!(
            command.op,
            FrameOp::Draw(_) | FrameOp::PushContext(_) | FrameOp::PopContext
        )
    }));
    assert_eq!(
        frame
            .commands()
            .iter()
            .filter(|command| matches!(command.op, FrameOp::Draw(_)))
            .count(),
        1
    );
    frame
}

#[cfg(feature = "gles2")]
#[test]
#[ignore = "requires a native GLES context"]
fn gles_retained_targets_yield_to_geometry_pressure() {
    use reel_render::gles2::{GlesBackend, GlesLimits};
    use reel_render::{AlphaMode, RenderBackend, RenderEvent, SurfaceInfo};
    use reel_ui::resources::ColorEncoding;
    let _serial = GLES_TEST_LOCK.lock().unwrap();
    let effect = frame();
    let geometry = geometry_frame();
    let total_bytes = TARGET_BYTES * 3 + (128 << 10);
    let mut renderer = GlesBackend::headless(
        SIZE,
        GlesLimits {
            target_bytes: TARGET_BYTES * 3,
            total_bytes,
            ..Default::default()
        },
    )
    .unwrap();
    renderer
        .configure(SurfaceInfo {
            physical_size: SIZE,
            scale_factor: 1.,
            surface_epoch: effect.surface_epoch(),
            color_encoding: ColorEncoding::Srgb,
            alpha_mode: AlphaMode::Opaque,
        })
        .unwrap();
    let mut completed = Vec::new();
    let mut events = Vec::new();
    for frame in [&effect, &geometry] {
        // The renderer must resolve reclaimable pressure within this call;
        // the test supplies no external retry or extra completion event.
        renderer.render_present(frame, || {}).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            events.clear();
            renderer.poll(&mut events, 32).unwrap();
            if events.iter().any(|event| {
                matches!(event, RenderEvent::RenderFinished { frame: id, .. } if *id == frame.frame_id())
            }) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "submission did not complete"
            );
            std::thread::yield_now();
        }
        let usage = renderer.usage();
        assert!(usage.peak_bytes <= total_bytes);
        completed.push(usage.bytes);
    }
    assert!(completed[0] >= TARGET_BYTES * 3);
    assert_eq!(completed[0] - completed[1], TARGET_BYTES * 2);
    renderer.close().unwrap();
}
