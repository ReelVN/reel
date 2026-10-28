#![cfg(feature = "wgpu")]
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
    let target_bytes = if atlas {
        ATLAS_TARGET_BYTES
    } else {
        TARGET_BYTES + width * (NESTED_TARGET_BYTES - TARGET_BYTES)
    };
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
        assert_eq!(renderer.usage().targets, target_bytes);
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

fn separable_frame(blend: reel_ui::frame::BlendMode, masked: bool) -> RenderFrame {
    let mut scene = Scene::new();
    for x in [20., 80.] {
        fill(
            &mut scene,
            Rect::new(x, 20., x + 140., 160.),
            Affine::IDENTITY,
        );
    }
    let mut widget = RecordWidget::new(scene);
    widget.opacity = 0.6;
    widget.blend = blend;
    if masked {
        widget.mask = Some(reel_ui::core::PaintImageMask {
            program: reel_ui::stage::MaskProgram::new(
                vec![reel_ui::stage::MaskOp::Constant(0.5_f32.to_bits())],
                vec![],
            )
            .unwrap(),
            images: vec![].into(),
            bounds: Rect::new(0., 0., 400., 300.),
            parameters: [0.; 5],
        });
    }
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
    build.finish().unwrap()
}
fn separable_modes() -> [reel_ui::frame::BlendMode; 7] {
    use reel_ui::peniko::Mix;
    [
        Mix::Overlay.into(),
        Mix::HardLight.into(),
        Mix::ColorDodge.into(),
        Mix::Lighten.into(),
        Mix::Difference.into(),
        Mix::SoftLight.into(),
        reel_ui::frame::BlendMode::LinearBurn,
    ]
}

#[cfg(feature = "wgpu")]
#[test]
#[ignore = "requires an actual WGPU adapter; submits without pixel readback"]
fn wgpu_separable_blends_and_masks_submit() {
    use reel_render::gpu::{DeviceGeneration, Gpu, Renderer};
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        Default::default(),
    ))
    .unwrap();
    let mut renderer = Renderer::new(gpu);
    for blend in separable_modes() {
        for masked in [false, true] {
            let frame = separable_frame(blend, masked);
            let prepared = renderer.prepare(&frame).unwrap();
            renderer.submit(prepared).unwrap();
            renderer
                .gpu()
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            renderer.poll().unwrap();
        }
    }
}
