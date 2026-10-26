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
    stage::MaskProgram,
};

#[path = "support/mask_dag.rs"]
mod mask_dag;
use mask_dag::operations;

#[test]
#[ignore = "requires a native GLES context"]
fn full_mask_dag_completes_at_all_register_bank_capacities() {
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
                    if index % 2 == 0 {
                        ColorEncoding::Linear
                    } else {
                        ColorEncoding::Srgb
                    },
                )
                .unwrap()
        })
        .collect();
    let mut renderer = GlesBackend::headless([400, 300], GlesLimits::default()).unwrap();
    let mut events = Vec::new();
    let cases = [
        operations()[..8].to_vec(),
        operations()[..16].to_vec(),
        operations(),
        mask_dag::compact_operations(),
    ];
    for (case, operations) in cases.into_iter().enumerate() {
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
    renderer.close().unwrap();
}
