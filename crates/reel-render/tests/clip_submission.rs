#![cfg(feature = "wgpu")]
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
    let single = renderer
        .gpu()
        .adapter
        .get_texture_format_features(wgpu::TextureFormat::R16Float);
    let supported =
        single.allowed_usages.contains(
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        ) && single.flags.contains(
            wgpu::TextureFormatFeatureFlags::FILTERABLE
                | wgpu::TextureFormatFeatureFlags::BLENDABLE
                | wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE,
        ) && single.flags.sample_count_supported(4);
    // Two color attachments and one same-precision coverage attachment, with
    // the original all-RGBA path retained when the adapter cannot support it.
    let bytes_per_pixel = 2 * 8 * 5 + if supported { 2 * 5 } else { 8 * 5 };
    assert_eq!(
        renderer.usage().targets,
        u64::from(SIZE[0]) * u64::from(SIZE[1]) * bytes_per_pixel
    );
    renderer.submit(prepared).unwrap();
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
}

#[test]
#[ignore = "requires a WGPU adapter; capacity and submission only"]
fn multisample_clip_keeps_quality_at_single_channel_budget() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    let exact = u64::from(SIZE[0]) * u64::from(SIZE[1]) * 90;
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits {
            target_bytes: exact,
            ..Default::default()
        },
    ))
    .unwrap();
    let features = gpu
        .adapter
        .get_texture_format_features(wgpu::TextureFormat::R16Float);
    if !features
        .allowed_usages
        .contains(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING)
        || !features.flags.contains(
            wgpu::TextureFormatFeatureFlags::FILTERABLE
                | wgpu::TextureFormatFeatureFlags::BLENDABLE
                | wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE,
        )
        || !features.flags.sample_count_supported(4)
    {
        return;
    }
    let mut renderer = Renderer::new(gpu);
    let frame = clipped_frame();
    let prepared = renderer.prepare(&frame).unwrap();
    assert_eq!(prepared.quality().format, wgpu::TextureFormat::Rgba16Float);
    assert_eq!(prepared.quality().samples, 4);
    assert_eq!(renderer.usage().targets, exact);
    renderer.submit(prepared).unwrap();
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
}

#[test]
#[ignore = "requires a WGPU adapter; 8-bit coverage submission only"]
fn single_sample_path_clip_uses_r8_when_color_is_rgba8() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    let pixels = u64::from(SIZE[0]) * u64::from(SIZE[1]);
    let exact = pixels * 9;
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits {
            target_bytes: exact,
            ..Default::default()
        },
    ))
    .unwrap();
    let features = gpu
        .adapter
        .get_texture_format_features(wgpu::TextureFormat::R8Unorm);
    if !features
        .allowed_usages
        .contains(wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING)
        || !features.flags.contains(
            wgpu::TextureFormatFeatureFlags::FILTERABLE
                | wgpu::TextureFormatFeatureFlags::BLENDABLE,
        )
    {
        return;
    }
    let mut path = reel_ui::kurbo::BezPath::new();
    path.move_to((30., 30.));
    path.line_to((400., 80.));
    path.line_to((100., 400.));
    path.close_path();
    let mut scene = Scene::new();
    scene.push_clip(Clip::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        shape: Geometry::Path(path),
    });
    scene.draw(Draw::Fill {
        transform: Affine::IDENTITY,
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::WHITE),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., 500., 500.)),
        composite: Composite::default(),
    });
    scene.pop_clip();
    let frame = compile_scene(scene);
    let mut renderer = Renderer::new(gpu);
    let prepared = renderer.prepare(&frame).unwrap();
    assert_eq!(prepared.quality().format, wgpu::TextureFormat::Rgba8Unorm);
    assert_eq!(prepared.quality().samples, 1);
    assert_eq!(renderer.usage().targets, exact);
    renderer.submit(prepared).unwrap();
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
}
