#![cfg(any(feature = "wgpu", feature = "gles2"))]
//! Native submission checks for scaled rectangular clip admission. No readback.
#[allow(dead_code, reason = "shared UI fixture")]
#[path = "../../reel-ui/tests/support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared frame compiler fixture")]
#[path = "../../reel-ui/tests/support/r01_fixture.rs"]
mod r01_fixture;

use r01_fixture::*;
use reel_core::identity::FrameId;
use reel_ui::{
    frame::{FrameBuild, FrameInfo, RenderFrame},
    imaging::{
        Composite,
        record::{Clip, Draw, Geometry, Scene},
    },
    kurbo::{Affine, Rect},
    peniko::{Brush, Color},
};

const SIZE: [u32; 2] = [960, 544];
const TARGET_BYTES: u64 = SIZE[0] as u64 * SIZE[1] as u64 * 4;

fn clipped_frame() -> RenderFrame {
    let mut scene = Scene::new();
    // A common parent scale turns otherwise integral UI rectangles into
    // fractional physical clips. Siblings exercise target reuse as well as cost.
    for index in 0..66 {
        let transform = Affine::scale(0.6666667)
            * Affine::translate((
                30. + (index % 6) as f64 * 150.,
                30. + (index / 6) as f64 * 48.,
            ));
        scene.push_clip(Clip::Fill {
            transform,
            fill_rule: Default::default(),
            shape: Geometry::Rect(Rect::new(0., 0., 120., 36.)),
        });
        for (x, color) in [
            (0., Color::WHITE),
            (45., Color::from_rgba8(64, 128, 192, 128)),
        ] {
            scene.draw(Draw::Fill {
                transform,
                fill_rule: Default::default(),
                brush: Brush::Solid(color),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(x, -6., x + 90., 42.)),
                composite: Composite::default(),
            });
        }
        scene.pop_clip();
    }
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
    build.finish().unwrap()
}

#[cfg(feature = "wgpu")]
#[test]
#[ignore = "requires an actual native WGPU adapter"]
fn single_sample_rect_clips_fit_one_target() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
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
    eprintln!("adapter={:?}", gpu.adapter.get_info());
    let mut renderer = Renderer::new(gpu);
    let frame = clipped_frame();
    let mut measured = std::time::Instant::now();
    for index in 0..201 {
        let prepared = renderer.prepare(&frame).unwrap();
        assert_eq!(prepared.quality().samples, 1);
        assert_eq!(renderer.usage().targets, TARGET_BYTES);
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
        "wgpu clips=66 frames=200 submit_complete_us={} target_bytes={TARGET_BYTES}",
        measured.elapsed().as_micros()
    );
}

#[cfg(feature = "wgpu")]
#[test]
#[ignore = "requires an actual native WGPU adapter"]
fn multisample_rect_clips_retain_coverage_targets() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits {
            target_bytes: 96 << 20,
            ..Default::default()
        },
    ))
    .unwrap();
    let mut renderer = Renderer::new(gpu);
    let frame = clipped_frame();
    let prepared = renderer.prepare(&frame).unwrap();
    assert_eq!(prepared.quality().samples, 4);
    assert_eq!(renderer.usage().targets, TARGET_BYTES * 2 * 5 * 3);
    renderer.submit(prepared).unwrap();
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
}

#[cfg(feature = "gles2")]
#[test]
#[ignore = "requires a native GLES context"]
fn gles_single_sample_rect_clips_fit_one_target() {
    use reel_render::gles2::{GlesBackend, GlesLimits};
    use reel_render::{AlphaMode, RenderBackend, RenderEvent, SurfaceInfo};
    use reel_ui::resources::ColorEncoding;
    let frame = clipped_frame();
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
    let mut events = Vec::new();
    let mut measured = std::time::Instant::now();
    for index in 0..201 {
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
        "gles clips=66 frames=200 submit_complete_us={} target_bytes={TARGET_BYTES}",
        measured.elapsed().as_micros()
    );
    renderer.close().unwrap();
}
