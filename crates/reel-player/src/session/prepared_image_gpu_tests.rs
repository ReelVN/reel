//! Device verification uses the ordinary image path as the sampling oracle.
use super::*;
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    identity::FrameId,
};
use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
use reel_ui::{
    frame::{BuildLimits, FrameBuild, FrameInfo, RenderFrame, StepBudget},
    resources::ResourceSnapshot,
    stage::CameraView,
};

fn frame(ui: &SessionUi, bridge: &mut PresentationBridge, id: u64) -> RenderFrame {
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 32,
            max_bytes: 16 << 20,
        },
    );
    let resources =
        ResourceSnapshot::capture(ui.image_resources().iter().cloned().map(Ok), &pool).unwrap();
    let mut build = FrameBuild::new(
        bridge.record(FrameId(id)).unwrap(),
        resources,
        FrameInfo {
            logical_size: [640., 420.],
            physical_size: [640, 420],
        },
        BuildLimits { max_bytes: 8 << 20 },
        &pool,
    )
    .unwrap();
    while !build
        .step(StepBudget {
            max_commands: 256,
            max_units: 32768,
            max_bytes: 1 << 20,
        })
        .unwrap()
        .complete
    {}
    build.finish().unwrap()
}

fn scene(
    directory: &std::path::Path,
    tiled: bool,
) -> (
    SessionUi,
    PresentationBridge,
    WidgetId,
    Stage,
    reel_ui::stage::NodeId,
    mpsc::Receiver<()>,
) {
    let mut stage = Stage::default();
    let source = if tiled {
        "world.reel-image"
    } else {
        "crop.png"
    };
    let image = stage
        .create(
            NodeKind::Image {
                source: source.into(),
                fit: ObjectFit::Contain,
                slice: None,
                slice_scale: 1.,
                repeat: [false; 2],
                tile_scale: 1.,
                corner_colors: None,
                adjustment: Default::default(),
                slice_center: true,
                origin: Some("camera-tiles.aside:3:9".into()),
            },
            NodeProps {
                x: if tiled { 0. } else { 9472. },
                y: if tiled { 0. } else { 9728. },
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let camera = stage
        .create(
            NodeKind::Camera(CameraView {
                x: 9984.25,
                y: 10240.75,
                zoom: 1.35,
                rotation: 23.,
            }),
            NodeProps {
                width: Some(640.),
                height: Some(420.),
                clip: true,
                ..Default::default()
            },
            vec![image],
        )
        .unwrap();
    stage.replace(camera).unwrap();
    let mut ui = SessionUi::new(super::super::tests::session())
        .unwrap()
        .with_asset_options(AssetOptions {
            root: directory.into(),
            resident_bytes: 12 << 20,
            decode_bytes: 8 << 20,
            input_bytes: 2 << 20,
        });
    let (send, wake) = mpsc::channel();
    ui.set_resource_waker(Arc::new(move || {
        let _ = send.send(());
    }));
    ui.set_stage(&stage);
    let (bridge, widget) = super::super::tests::mount(&ui);
    (ui, bridge, widget, stage, camera, wake)
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn prepared_tiles_match_contiguous_image_through_rotated_camera() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/reel-artifacts/prepared-image-gpu")
        .join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(directory.join("world.reel-image.tiles/0")).unwrap();
    let manifest = PreparedImage::new(40001, 30001, 128).unwrap();
    fs::write(directory.join("world.reel-image"), manifest.encode()).unwrap();
    // Smooth repeating RGB ramps plus partial alpha expose both wrong UVs and
    // double-blended antialiased seams. Gutters sample actual adjacent pixels.
    let pixel = |x: u32, y: u32| {
        image::Rgba([
            (32 + (x % 512) / 3) as u8,
            (32 + (y % 512) / 3) as u8,
            (32 + ((x + y) % 512) / 3) as u8,
            192,
        ])
    };
    for y in 72..88 {
        for x in 70..88 {
            image::RgbaImage::from_fn(130, 130, |tx, ty| pixel(x * 128 + tx - 1, y * 128 + ty - 1))
                .save(directory.join(PreparedImage::tile_source("world.reel-image", 0, x, y)))
                .unwrap();
        }
    }
    image::RgbaImage::from_fn(1024, 1024, |x, y| pixel(x + 9472, y + 9728))
        .save(directory.join("crop.png"))
        .unwrap();
    let instance = wgpu::Instance::default();
    let gpu = pollster::block_on(Gpu::request(
        &instance,
        None,
        DeviceGeneration(1),
        GpuLimits::default(),
    ))
    .unwrap();
    eprintln!(
        "prepared camera adapter={:?} max_texture_dimension_2d={}",
        gpu.adapter.get_info(),
        gpu.device.limits().max_texture_dimension_2d
    );
    assert!(manifest.width > gpu.device.limits().max_texture_dimension_2d);
    let mut renderer = Renderer::new(gpu);
    let oracle_gpu = pollster::block_on(Gpu::request(
        &instance,
        None,
        DeviceGeneration(2),
        GpuLimits::default(),
    ))
    .unwrap();
    let mut oracle_renderer = Renderer::new(oracle_gpu);
    let (mut tiled, mut tiles_bridge, tiles_widget, mut tiles_stage, tiles_camera, tiles_wake) =
        scene(&directory, true);
    let (
        mut direct,
        mut direct_bridge,
        direct_widget,
        mut direct_stage,
        direct_camera,
        direct_wake,
    ) = scene(&directory, false);
    let mut gpu_peak = 0;
    let mut max_error = 0_f32;
    for (index, view) in [
        CameraView {
            x: 9984.25,
            y: 10240.75,
            zoom: 1.35,
            rotation: 23.,
        },
        CameraView {
            x: 10019.5,
            y: 10263.125,
            zoom: 1.1,
            rotation: -17.,
        },
    ]
    .into_iter()
    .enumerate()
    {
        for (ui, bridge, widget, stage, camera, wake) in [
            (
                &mut tiled,
                &mut tiles_bridge,
                tiles_widget,
                &mut tiles_stage,
                tiles_camera,
                &tiles_wake,
            ),
            (
                &mut direct,
                &mut direct_bridge,
                direct_widget,
                &mut direct_stage,
                direct_camera,
                &direct_wake,
            ),
        ] {
            let props = stage.node(camera).unwrap().props.clone();
            stage.update(camera, NodeKind::Camera(view), props).unwrap();
            ui.set_stage(stage);
            settle(ui, bridge, widget, wake);
        }
        let tiled_frame = frame(&tiled, &mut tiles_bridge, index as u64 + 1);
        let prepared = renderer.prepare(&tiled_frame).unwrap();
        gpu_peak = gpu_peak.max(renderer.usage().resources);
        let actual = renderer.readback(prepared).unwrap();
        assert!(
            (actual[210 * 640 + 250][0] - actual[210 * 640 + 350][0]).abs() > 0.01,
            "the compared viewport contains the patterned image"
        );
        let resident = renderer.usage().resources;
        let loads = tiled.images.loads;
        let warm = renderer.prepare(&tiled_frame).unwrap();
        // Ordinary clip masks share one per-preparation 1x1 white texture:
        // two padded staging rows plus three unpadded RGBA16 rows = 536 bytes.
        // Every tile is cached; none contributes an additional upload here.
        assert_eq!(renderer.usage().resources - resident, 2 * 256 + 3 * 8);
        assert_eq!(renderer.readback(warm).unwrap(), actual);
        assert_eq!(tiled.images.loads, loads);
        let direct_frame = frame(&direct, &mut direct_bridge, index as u64 + 1);
        let expected = oracle_renderer
            .readback(oracle_renderer.prepare(&direct_frame).unwrap())
            .unwrap();
        for y in 4..416 {
            for x in 4..636 {
                for channel in 0..4 {
                    let error =
                        (actual[y * 640 + x][channel] - expected[y * 640 + x][channel]).abs();
                    max_error = max_error.max(error);
                    assert!(
                        error < 0.008,
                        "camera view{index} seam/sample pixel({x},{y}) channel{channel}: actual={:?} expected={:?}",
                        actual[y * 640 + x],
                        expected[y * 640 + x]
                    );
                }
            }
        }
        assert!(
            tiled.images.loaded.len() < 80,
            "working set follows viewport, not full world"
        );
        eprintln!(
            "camera view={index} decoded_requests={} tiles={} cpu_resident={} cpu_peak={} gpu_tiled_resident={} gpu_upload_peak={} max_pixel_error={max_error}",
            tiled.images.loads,
            tiled.images.loaded.len(),
            tiled.images.pool.usage().bytes,
            tiled.images.pool.usage().peak_bytes,
            resident,
            gpu_peak
        );
    }
    assert!(tiled.images.pool.usage().peak_bytes < 8 << 20);
    assert!(gpu_peak < 32 << 20);
    assert_eq!(renderer.usage().targets, 0);
}
