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
#[cfg(feature = "gles2")]
static GLES_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn clipped_frame() -> RenderFrame {
    let mut scene = Scene::new();
    // A common parent scale turns otherwise integral UI rectangles into
    // fractional physical clips. Siblings exercise target reuse as well as cost.
    for index in 0..66 {
        let transform = Affine::translate((0.5, 0.5))
            * Affine::scale(0.6666667)
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
    compile_scene(scene)
}

fn compile_scene(scene: Scene) -> RenderFrame {
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

#[cfg(all(feature = "wgpu", feature = "profiling"))]
#[test]
#[ignore = "requires an actual native WGPU adapter"]
fn wgpu_effect_endpoints_reuse_scratch_targets() {
    use reel_core::profile::{Phase, Sample};
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    use reel_ui::imaging::record::Group;
    let mut scene = Scene::new();
    scene.push_group(Group {
        composite: Composite {
            alpha: 0.5,
            ..Default::default()
        },
        ..Default::default()
    });
    for x in [40., 80.] {
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(Color::WHITE),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(x, 10., x + 100., 60.)),
            composite: Composite::default(),
        });
    }
    scene.pop_group();
    let effect = compile_scene(scene);
    let plain = compile_scene(Scene::new());
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
    let before = Sample::snapshot();
    let mut resident = None;
    for frame in [&effect, &plain, &effect, &plain, &effect] {
        let prepared = renderer.prepare(frame).unwrap();
        renderer.submit(prepared).unwrap();
        renderer
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
        assert_eq!(
            renderer.usage().targets,
            *resident.get_or_insert(renderer.usage().targets)
        );
    }
    assert_eq!(
        Sample::snapshot().delta(before).calls[Phase::TargetCreate as usize],
        2
    );
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
    let _serial = GLES_TEST_LOCK.lock().unwrap();
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

#[cfg(feature = "gles2")]
#[test]
#[ignore = "requires a native GLES context"]
fn gles_nested_groups_retain_clip_composition() {
    let _serial = GLES_TEST_LOCK.lock().unwrap();
    use reel_render::gles2::{GlesBackend, GlesLimits};
    use reel_render::{AlphaMode, PresentOutcome, RenderBackend, RenderEvent, SurfaceInfo};
    use reel_ui::{imaging::record::Group, resources::ColorEncoding};
    let mut scene = Scene::new();
    scene.push_clip(Clip::Fill {
        transform: Affine::translate((0.5, 0.5)),
        fill_rule: Default::default(),
        shape: Geometry::Rect(Rect::new(0., 0., 120., 36.)),
    });
    scene.push_group(Group {
        composite: Composite {
            alpha: 0.5,
            ..Default::default()
        },
        ..Default::default()
    });
    for x in [0., 45.] {
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(Color::from_rgba8(64, 128, 192, 128)),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(x, -6., x + 90., 42.)),
            composite: Composite::default(),
        });
    }
    scene.pop_group();
    scene.pop_clip();
    let frame = compile_scene(scene);
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
    #[cfg(feature = "profiling")]
    let before = reel_core::profile::Sample::snapshot();
    assert!(matches!(
        renderer.render_present(&frame, || {}).unwrap(),
        PresentOutcome::PresentAccepted(_)
    ));
    let mut events = Vec::new();
    loop {
        renderer.poll(&mut events, 32).unwrap();
        if events
            .iter()
            .any(|event| matches!(event, RenderEvent::RenderFinished { .. }))
        {
            break;
        }
        std::thread::yield_now();
    }
    #[cfg(feature = "profiling")]
    {
        use reel_core::profile::{Phase, Sample};
        let measured = Sample::snapshot().delta(before);
        assert_eq!(measured.calls[Phase::SourceComposite as usize], 1);
        assert_eq!(measured.calls[Phase::CoverageComposite as usize], 1);
        assert_eq!(measured.calls[Phase::EffectComposite as usize], 0);
        assert_eq!(measured.calls[Phase::TargetCreate as usize], 3);
    }
    renderer.close().unwrap();
}
