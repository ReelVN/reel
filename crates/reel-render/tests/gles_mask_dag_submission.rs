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

fn operations() -> Vec<MaskOp> {
    use MaskChannel::{Alpha, Gray};
    use MaskOp::*;
    let sample = |image, channel, coordinates, repeat| Sample {
        image,
        channel,
        coordinates,
        repeat,
    };
    vec![
        Constant(0_f32.to_bits()),
        Constant(1_f32.to_bits()),
        Parameter(0),
        sample(0, Alpha, [0, 0, 1, 1], 0),
        sample(1, Gray, [2, 0, 1, 1], 1),
        Subtract(3, 4),
        Multiply(5, 2),
        Clamp(6, 0_f32.to_bits(), 1_f32.to_bits()),
        Constant(2_f32.to_bits()),
        Pow(7, 2_f32.to_bits()),
        Constant((-1_f32).to_bits()),
        Add(9, 10),
        Smoothstep(10, 1, 11),
        sample(2, Gray, [7, 2, 1, 1], 2),
        Subtract(12, 13),
        Clamp(14, (-2_f32).to_bits(), 2_f32.to_bits()),
        sample(3, Alpha, [15, 14, 2, 1], 3),
        Pow(16, 0_f32.to_bits()),
        Add(15, 17),
        Multiply(18, 8),
        Clamp(19, (-1_f32).to_bits(), 3_f32.to_bits()),
        Smoothstep(0, 0, 20),
        Pow(20, 0.5_f32.to_bits()),
        Subtract(22, 17),
        Multiply(23, 13),
        Add(24, 21),
        sample(3, Gray, [23, 25, 2, 9], 3),
        Smoothstep(17, 10, 26),
        Subtract(27, 26),
        Add(28, 18),
        Multiply(29, 7),
        Clamp(30, 0_f32.to_bits(), 1_f32.to_bits()),
    ]
}

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
    for (case, count) in [8, 16, 32].into_iter().enumerate() {
        let program = MaskProgram::new(
            operations()[..count].to_vec(),
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
