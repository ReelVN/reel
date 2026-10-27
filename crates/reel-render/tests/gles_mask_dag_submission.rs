#![cfg(feature = "gles2")]
//! Submit every register-bank capacity with the complete public mask operation set.
#[allow(dead_code, reason = "shared UI fixture")]
#[path = "../../reel-ui/tests/support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared frame compiler fixture")]
#[path = "../../reel-ui/tests/support/r01_fixture.rs"]
mod r01_fixture;

use r01_fixture::*;
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_render::{
    AlphaMode, PresentOutcome, RenderBackend, RenderEvent, SurfaceInfo,
    gles2::{GlesBackend, GlesLimits},
};
use reel_ui::{
    core::PaintImageMask,
    imaging::record::{Draw, Geometry, Scene},
    kurbo::{Affine, Rect},
    peniko::{Brush, Color, ImageAlphaType, ImageFormat},
    resources::{ColorEncoding, ResourceRegistry},
    stage::{MaskChannel, MaskOp, MaskProgram},
};

#[path = "support/mask_dag.rs"]
mod mask_dag;
use mask_dag::operations;

#[test]
#[ignore = "requires a native GLES context"]
fn full_mask_dag_completes_at_all_register_bank_capacities() {
    submit_masks(false);
}

#[cfg(feature = "wgpu")]
#[test]
#[ignore = "requires an actual WGPU adapter"]
fn split_masks_submit_parameter_updates_with_wgpu() {
    submit_masks(true);
}

fn submit_masks(wgpu: bool) {
    let pool = pool();
    let mut registry = ResourceRegistry::new(4, 65536).unwrap();
    let handles: Vec<_> = (0..4)
        .map(|index: u8| {
            registry
                .register_image(
                    ResourceVersion {
                        key: AssetKey(format!("mask-dag-{index}")),
                        version: ContentVersion([index; 32]),
                    },
                    OwnedBytes::new(
                        vec![
                            0, 0, 0, 0, 64, 32, 16, 192, 128, 64, 32, 128, 255, 255, 255, 255,
                        ],
                        pool.reserve(16).unwrap(),
                    )
                    .unwrap(),
                    [2, 2],
                    ImageFormat::Rgba8,
                    ImageAlphaType::Alpha,
                    if index.is_multiple_of(2) {
                        ColorEncoding::Linear
                    } else {
                        ColorEncoding::Srgb
                    },
                )
                .unwrap()
        })
        .collect();
    let mut renderer =
        (!wgpu).then(|| GlesBackend::headless([400, 300], GlesLimits::default()).unwrap());
    #[cfg(feature = "wgpu")]
    let mut wgpu_renderer = wgpu.then(|| {
        use reel_render::gpu::{DeviceGeneration, Gpu, Renderer};
        Renderer::new(
            pollster::block_on(Gpu::request(
                &wgpu::Instance::default(),
                None,
                DeviceGeneration(1),
                Default::default(),
            ))
            .unwrap(),
        )
    });
    let mut events = Vec::new();
    let mut shared_sample = operations()[..13].to_vec();
    shared_sample[4] = MaskOp::Sample {
        image: 0,
        channel: MaskChannel::Gray,
        coordinates: [0, 0, 1, 1],
        repeat: 0,
    };
    let mut alpha_only = operations()[..13].to_vec();
    if let MaskOp::Sample { channel, .. } = &mut alpha_only[4] {
        *channel = MaskChannel::Alpha;
    }
    let cases = [
        operations()[..8].to_vec(),
        operations()[..16].to_vec(),
        operations(),
        mask_dag::compact_operations(),
        shared_sample,
        alpha_only,
    ];
    for (case, operations) in cases.into_iter().enumerate() {
        #[cfg(feature = "wgpu")]
        let mut wgpu_resident = None;
        let count = operations.len();
        let program = MaskProgram::new(
            operations,
            (0..4)
                .map(|index| format!("image-{index}").into())
                .collect(),
        )
        .unwrap();
        for (step, progress) in [0., 0.35, 0.7, 1.].into_iter().enumerate() {
            let mut scene = Scene::new();
            scene.draw(Draw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Solid(Color::WHITE),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., 0., 32., 32.)),
                composite: Default::default(),
            });
            let mut widget = RecordWidget::new(scene);
            widget.clip = false;
            widget.mask = Some(PaintImageMask {
                program: program.clone(),
                images: handles
                    .iter()
                    .map(|&handle| registry.acquire(handle).unwrap())
                    .collect(),
                bounds: Rect::new(0., 0., 32., 32.),
                parameters: [progress, 0.2, -0.25, 0.75, 0.],
            });
            let mut bridge = widget_bridge(widget, Affine::IDENTITY);
            let id = FrameId((case * 4 + step + 1) as u64);
            let source = bridge.record(id).unwrap();
            let snapshot = registry.freeze(&handles, &pool).unwrap();
            let frame = build(source, snapshot, &pool);
            #[cfg(feature = "wgpu")]
            if let Some(wgpu_renderer) = &mut wgpu_renderer {
                let prepared = wgpu_renderer.prepare(&frame).unwrap();
                wgpu_renderer.submit(prepared).unwrap();
                wgpu_renderer
                    .gpu()
                    .device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                wgpu_renderer.poll().unwrap();
                let bytes = wgpu_renderer.usage().resources;
                if let Some(previous) = wgpu_resident {
                    assert_eq!(
                        bytes, previous,
                        "split parameters reuse pipeline and CPU plan storage"
                    );
                }
                wgpu_resident = Some(bytes);
            }
            let Some(renderer) = &mut renderer else {
                continue;
            };
            renderer
                .configure(SurfaceInfo {
                    physical_size: [400, 300],
                    scale_factor: 1.,
                    surface_epoch: frame.surface_epoch(),
                    color_encoding: ColorEncoding::Srgb,
                    alpha_mode: AlphaMode::Opaque,
                })
                .unwrap();
            assert_eq!(
                renderer.render_present(&frame, || {}).unwrap(),
                PresentOutcome::PresentAccepted(id)
            );
            let start = std::time::Instant::now();
            loop {
                events.clear();
                renderer.poll(&mut events, 16).unwrap();
                if events.iter().any(|event| matches!(event, RenderEvent::RenderFinished { frame, .. } if *frame == id)) { break; }
                assert!(
                    start.elapsed() < std::time::Duration::from_secs(10),
                    "{count}-node completion timeout"
                );
                std::thread::yield_now();
            }
        }
    }
    if let Some(renderer) = renderer {
        renderer.close().unwrap();
    }
}
