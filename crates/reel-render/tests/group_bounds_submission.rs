#![cfg(any(feature = "wgpu", feature = "gles2"))]
//! Submit bounded isolated groups through production replay without reading pixels.
#[allow(dead_code, reason = "shared UI fixture")]
#[path = "../../reel-ui/tests/support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared frame compiler fixture")]
#[path = "../../reel-ui/tests/support/r01_fixture.rs"]
mod r01_fixture;

use r01_fixture::*;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
#[global_allocator]
static GLOBAL: &StatsAlloc<std::alloc::System> = &INSTRUMENTED_SYSTEM;
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

fn source_over_frame() -> RenderFrame {
    let mut scene = Scene::new();
    for _ in 0..4 {
        scene.push_group(Group::default());
        fill(
            &mut scene,
            Rect::new(30., 40., 430., 340.),
            Affine::IDENTITY,
        );
    }
    fill(
        &mut scene,
        Rect::new(120., 150., 600., 430.),
        Affine::IDENTITY,
    );
    for _ in 0..4 {
        scene.pop_group();
    }
    // A clipped single rectangle can carry its own group's opacity. Keep this
    // in the one-target contract so both backends exercise the shared folding plan.
    scene.push_group(Group {
        composite: Composite {
            alpha: 0.5,
            ..Default::default()
        },
        ..Default::default()
    });
    scene.push_clip(reel_ui::imaging::record::Clip::Fill {
        transform: Affine::IDENTITY,
        shape: Geometry::Rect(Rect::new(30., 40., 430., 340.)),
        fill_rule: Default::default(),
    });
    fill(&mut scene, Rect::new(0., 0., 960., 544.), Affine::IDENTITY);
    scene.pop_clip();
    scene.pop_group();
    let mut widget = RecordWidget::new(scene);
    widget.clip = false;
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    let pool = pool();
    let mut build = FrameBuild::new(
        bridge.record(FrameId(3)).unwrap(),
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
    assert_eq!(
        frame
            .commands()
            .iter()
            .filter(|c| matches!(c.op, FrameOp::PushGroup(_)))
            .count(),
        5
    );
    frame
}

fn nested_isolation_frame() -> RenderFrame {
    let mut scene = Scene::new();
    for index in 0..GROUPS {
        scene.push_group(Group::default());
        scene.push_group(Group {
            composite: Composite {
                alpha: 0.5,
                ..Default::default()
            },
            ..Default::default()
        });
        let transform = Affine::translate((
            20. + (index % 4) as f64 * 220.,
            20. + (index / 4) as f64 * 120.,
        ));
        for x in [0., 40.] {
            fill(&mut scene, Rect::new(x, 0., x + 80., 60.), transform);
        }
        scene.pop_group();
        scene.pop_group();
    }
    let mut widget = RecordWidget::new(scene);
    widget.clip = false;
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    let pool = pool();
    let mut build = FrameBuild::new(
        bridge.record(FrameId(4)).unwrap(),
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
    assert_eq!(
        frame
            .commands()
            .iter()
            .filter(|c| matches!(c.op, FrameOp::PushGroup(_)))
            .count(),
        GROUPS as usize * 2
    );
    frame
}

const NESTED_TARGETS: u64 = 2;
// The isolated 120 x 60 source includes raster padding and rounds to a 32-pixel bucket.
const NESTED_TARGET_BYTES: u64 = TARGET_BYTES + 128 * 64 * 4;
const ATLAS_TARGET_BYTES: u64 = TARGET_BYTES + 128 * 544 * 4;

#[cfg(feature = "wgpu")]
#[test]
#[ignore = "requires an actual native WGPU adapter"]
fn wgpu_nested_isolation_keeps_only_required_targets() {
    wgpu_isolation(1, false);
}
#[cfg(feature = "wgpu")]
#[test]
#[ignore = "requires an actual native WGPU adapter"]
fn wgpu_independent_groups_batch_within_the_target_budget() {
    for width in [2, 4, 8] {
        wgpu_isolation(width, false);
    }
    wgpu_isolation(8, true);
}
#[cfg(feature = "wgpu")]
fn wgpu_isolation(width: u64, atlas: bool) {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    let frame = nested_isolation_frame();
    let target_bytes = if atlas { ATLAS_TARGET_BYTES } else { TARGET_BYTES + width * (NESTED_TARGET_BYTES - TARGET_BYTES) };
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits {
            target_bytes,
            ..Default::default()
        },
    ))
    .unwrap();
    let mut renderer = Renderer::new(gpu);
    let mut warm = std::time::Instant::now();
    let mut warm_allocations = 0;
    let mut warm_bytes = 0;
    let mut warm_submit_us = 0;
    let mut peak_live = 0;
    for index in 0..=8 {
        let region = Region::new(GLOBAL);
        let submitting = std::time::Instant::now();
        #[cfg(feature = "profiling")]
        let before = reel_core::profile::Sample::snapshot();
        let prepared = renderer.prepare(&frame).unwrap();
        assert_eq!(
            renderer.usage().targets,
            target_bytes
        );
        peak_live = peak_live.max(renderer.usage().transient);
        renderer.submit(prepared).unwrap();
        let submit_us = submitting.elapsed().as_micros();
        renderer
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
        #[cfg(feature = "profiling")]
        {
            use reel_core::profile::{Phase, Sample};
            let delta = Sample::snapshot().delta(before);
            assert_eq!(
                delta.calls[Phase::CompositeBatch as usize],
                if width == 1 { 0 } else { GROUPS / width }
            );
            assert_eq!(
                delta.calls[Phase::BufferCreate as usize],
                if index == 0 { GROUPS } else { 0 }
            );
        }
        assert_eq!(renderer.usage().transient, renderer.usage().cached_geometry);
        if index > 0 {
            let allocation = region.change();
            warm_allocations += allocation.allocations;
            warm_bytes += allocation.bytes_allocated;
            warm_submit_us += submit_us;
        }
        if index == 0 {
            warm = std::time::Instant::now();
        }
    }
    eprintln!(
        "wgpu geometry_cost warm_allocations={warm_allocations} warm_allocated_bytes={warm_bytes} submit_us={warm_submit_us} transient_live_peak={peak_live} cached={}",
        renderer.usage().cached_geometry
    );
    eprintln!(
        "wgpu nested_isolation warm_frames=8 batch_width={width} atlas={atlas} targets={} target_bytes={target_bytes} submit_complete_us={}",
        if atlas { 2 } else { 1 + width },
        warm.elapsed().as_micros()
    );
}

#[cfg(feature = "gles2")]
#[test]
#[ignore = "requires a native GLES context"]
fn gles_nested_isolation_keeps_only_required_targets() {
    gles_isolation(1, false);
}
#[cfg(feature = "gles2")]
#[test]
#[ignore = "requires a native GLES context"]
fn gles_independent_groups_batch_within_the_target_budget() {
    for width in [2, 4, 8] {
        gles_isolation(width, false);
    }
    gles_isolation(8, true);
}
#[cfg(feature = "gles2")]
fn gles_isolation(width: u64, atlas: bool) {
    use reel_render::gles2::{GlesBackend, GlesLimits};
    use reel_render::{AlphaMode, RenderBackend, RenderEvent, SurfaceInfo};
    use reel_ui::resources::ColorEncoding;
    let _serial = GLES_TEST_LOCK.lock().unwrap();
    let frame = nested_isolation_frame();
    let target_bytes = if atlas { ATLAS_TARGET_BYTES } else { TARGET_BYTES + width * (NESTED_TARGET_BYTES - TARGET_BYTES) };
    let mut renderer = GlesBackend::headless(
        SIZE,
        GlesLimits {
            target_bytes,
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
    let mut warm = std::time::Instant::now();
    let mut warm_allocations = 0;
    let mut warm_bytes = 0;
    let mut warm_submit_us = 0;
    let mut peak_live = 0;
    let mut events = Vec::new();
    for index in 0..=8 {
        let region = Region::new(GLOBAL);
        let submitting = std::time::Instant::now();
        #[cfg(feature = "profiling")]
        let before = reel_core::profile::Sample::snapshot();
        renderer.render_present(&frame, || {}).unwrap();
        peak_live = peak_live.max(renderer.usage().peak_bytes);
        let submit_us = submitting.elapsed().as_micros();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            events.clear();
            renderer.poll(&mut events, 32).unwrap();
            if events
                .iter()
                .any(|e| matches!(e, RenderEvent::RenderFinished { .. }))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "nested submission did not complete"
            );
            std::thread::yield_now();
        }
        #[cfg(feature = "profiling")]
        {
            use reel_core::profile::{Phase, Sample};
            let delta = Sample::snapshot().delta(before);
            assert_eq!(
                delta.calls[Phase::CompositeBatch as usize],
                if width == 1 { 0 } else { GROUPS / width }
            );
            assert_eq!(
                delta.calls[Phase::SourceComposite as usize],
                GROUPS * (NESTED_TARGETS - 1)
            );
            // The root target and final screen are both cleared once.
            assert_eq!(
                delta.calls[Phase::Clear as usize],
                2 + if atlas { GROUPS / width } else { GROUPS * (NESTED_TARGETS - 1) }
            );
            assert_eq!(
                delta.calls[Phase::TargetCreate as usize],
                if index == 0 { if atlas { 2 } else { 1 + width } } else { 0 }
            );
            assert_eq!(
                delta.calls[Phase::BufferCreate as usize],
                if index == 0 { GROUPS } else { 0 }
            );
            if index == 1 {
                eprintln!(
                    "gles nested_isolation per_frame batch_width={width} batches={} clear={} composite={} draw={} buffer_create={}",
                    delta.calls[Phase::CompositeBatch as usize],
                    delta.calls[Phase::Clear as usize],
                    delta.calls[Phase::Composite as usize],
                    delta.calls[Phase::Draw as usize],
                    delta.calls[Phase::BufferCreate as usize]
                );
            }
        }
        if index > 0 {
            let allocation = region.change();
            warm_allocations += allocation.allocations;
            warm_bytes += allocation.bytes_allocated;
            warm_submit_us += submit_us;
        }
        if index == 0 {
            warm = std::time::Instant::now();
        }
    }
    eprintln!(
        "gles geometry_cost warm_allocations={warm_allocations} warm_allocated_bytes={warm_bytes} submit_us={warm_submit_us} total_peak={peak_live} cached={}",
        renderer.cached_geometry_bytes()
    );
    eprintln!(
        "gles nested_isolation warm_frames=8 batch_width={width} atlas={atlas} targets={} target_bytes={target_bytes} submit_complete_us={}",
        if atlas { 2 } else { 1 + width },
        warm.elapsed().as_micros()
    );
    renderer.close().unwrap();
}

#[cfg(feature = "wgpu")]
#[test]
#[ignore = "requires an actual native WGPU adapter"]
fn wgpu_source_over_containers_use_one_target() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    let frame = source_over_frame();
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits {
            target_bytes: TARGET_BYTES,
            ..Default::default()
        },
    ))
    .unwrap();
    let mut renderer = Renderer::new(gpu);
    let start = std::time::Instant::now();
    for _ in 0..8 {
        let prepared = renderer.prepare(&frame).unwrap();
        assert_eq!(renderer.usage().targets, TARGET_BYTES);
        renderer.submit(prepared).unwrap();
        renderer
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
    }
    eprintln!(
        "wgpu source_over_containers frames=8 targets=1 target_bytes={TARGET_BYTES} submit_complete_us={}",
        start.elapsed().as_micros()
    );
}

#[cfg(feature = "gles2")]
#[test]
#[ignore = "requires a native GLES context"]
fn gles_source_over_containers_use_one_target() {
    use reel_render::gles2::{GlesBackend, GlesLimits};
    use reel_render::{AlphaMode, RenderBackend, RenderEvent, SurfaceInfo};
    use reel_ui::resources::ColorEncoding;
    let _serial = GLES_TEST_LOCK.lock().unwrap();
    let frame = source_over_frame();
    let mut renderer = GlesBackend::headless(
        SIZE,
        GlesLimits {
            target_bytes: TARGET_BYTES,
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
    let start = std::time::Instant::now();
    let mut events = Vec::new();
    for _ in 0..8 {
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
    }
    eprintln!(
        "gles source_over_containers frames=8 targets=1 target_bytes={TARGET_BYTES} submit_complete_us={}",
        start.elapsed().as_micros()
    );
    renderer.close().unwrap();
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
        completed.push((usage.bytes, renderer.cached_geometry_bytes()));
    }
    assert!(completed[0].0 >= TARGET_BYTES * 3);
    // Geometry remains accounted after completion. Excluding that measured
    // residency must still show exactly two retired scratch allocations.
    assert_eq!(
        (completed[0].0 - completed[0].1) - (completed[1].0 - completed[1].1),
        TARGET_BYTES * 2,
    );
    renderer.close().unwrap();
}

#[cfg(feature = "gles2")]
fn image_mask_frame(threshold: bool, progress: f32, invert: f32) -> RenderFrame {
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
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 64 << 10).unwrap();
    let pixels = vec![255, 64, 0, 255, 0, 128, 0, 64, 0, 0, 255, 192, 0, 0, 0, 0];
    let image = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("mask".into()),
                version: ContentVersion([0; 32]),
            },
            OwnedBytes::new(pixels, pool.reserve(16).unwrap()).unwrap(),
            [2, 2],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let program = if threshold {
        MaskProgram::image("mask".into())
    } else {
        MaskProgram::new(
            vec![
                MaskOp::Constant(0),
                MaskOp::Constant(1_f32.to_bits()),
                MaskOp::Sample {
                    image: 0,
                    channel: MaskChannel::Gray,
                    coordinates: [0, 0, 1, 1],
                    repeat: 3,
                },
                MaskOp::Parameter(0),
                MaskOp::Subtract(2, 3),
                MaskOp::Clamp(4, 0_f32.to_bits(), 1_f32.to_bits()),
            ],
            vec!["mask".into()],
        )
        .unwrap()
    };
    let mut scene = Scene::new();
    // Overlap requires isolation before either opacity or image coverage.
    for x in [20., 80.] {
        fill(
            &mut scene,
            Rect::new(x, 20., x + 140., 160.),
            Affine::IDENTITY,
        );
    }
    let mut widget = RecordWidget::new(scene);
    widget.clip = false;
    widget.opacity = 0.6;
    widget.invert = invert;
    widget.mask = Some(PaintImageMask {
        program,
        images: vec![registry.acquire(image).unwrap()].into(),
        bounds: Rect::new(0., 0., 400., 300.),
        parameters: [progress, 0.2, 0., 0., 0.],
    });
    let mut bridge = widget_bridge(
        widget,
        Affine::translate((40.5, 20.5)) * Affine::rotate(0.1),
    );
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[image], &pool).unwrap(),
        FrameInfo {
            logical_size: [960., 544.],
            physical_size: SIZE,
        },
        limits(),
        &pool,
    )
    .unwrap();
    while !build.step(step_budget()).unwrap().complete {}
    build.finish().unwrap()
}

#[cfg(feature = "gles2")]
#[test]
#[ignore = "requires a native GLES context; submits without pixel readback"]
fn gles_simple_image_masks_composite_without_an_intermediate_target() {
    use reel_render::gles2::{GlesBackend, GlesLimits};
    use reel_render::{AlphaMode, RenderBackend, RenderEvent, SurfaceInfo};
    use reel_ui::resources::ColorEncoding;
    let _serial = GLES_TEST_LOCK.lock().unwrap();
    // Inversion retains the general path and its additional expression target.
    for (threshold, invert, targets) in [(false, 0., 2), (true, 0., 2), (false, 0.25, 3)] {
        let mut renderer = GlesBackend::headless(
            SIZE,
            GlesLimits {
                target_bytes: TARGET_BYTES * targets,
                ..Default::default()
            },
        )
        .unwrap();
        for progress in [0., 0.45, 1.] {
            let frame = image_mask_frame(threshold, progress, invert);
            renderer
                .configure(SurfaceInfo {
                    physical_size: SIZE,
                    scale_factor: 1.,
                    surface_epoch: frame.surface_epoch(),
                    color_encoding: ColorEncoding::Srgb,
                    alpha_mode: AlphaMode::Opaque,
                })
                .unwrap();
            #[cfg(feature = "profiling")]
            let before = reel_core::profile::Sample::snapshot();
            renderer.render_present(&frame, || {}).unwrap();
            #[cfg(feature = "profiling")]
            {
                use reel_core::profile::{Phase, Sample};
                let delta = Sample::snapshot().delta(before);
                // The shared paint planner removes a threshold's hidden endpoint
                // before replay; only the cleared output and presentation remain.
                let (clears, draws, composites) = if threshold && progress == 0. {
                    assert!(
                        frame
                            .commands()
                            .iter()
                            .all(|command| !matches!(command.op, FrameOp::Draw(_)))
                    );
                    (2, 1, 0)
                } else {
                    // Each offscreen target and the replaced output surface
                    // start with a full clear.
                    // The two adjacent fills share one geometry batch.
                    (targets + 1, targets + 1, 1)
                };
                assert_eq!(delta.calls[Phase::Clear as usize], clears);
                assert_eq!(delta.calls[Phase::Draw as usize], draws);
                assert_eq!(delta.calls[Phase::Composite as usize], composites);
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut events = Vec::new();
            loop {
                events.clear();
                renderer.poll(&mut events, 32).unwrap();
                if events
                    .iter()
                    .any(|event| matches!(event, RenderEvent::RenderFinished { .. }))
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "submission did not complete"
                );
                std::thread::yield_now();
            }
        }
        renderer.close().unwrap();
    }
}
