#![cfg(feature = "gles2")]
//! Native execution of specialized and general GLES masks without pixel readback.
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
    imaging::{
        Composite,
        record::{Draw, Geometry, Scene},
    },
    kurbo::{Affine, Rect},
    peniko::{Brush, Color, ImageAlphaType, ImageFormat},
    resources::{ColorEncoding, ResourceRegistry},
    stage::{MaskChannel, MaskOp, MaskProgram},
};

#[test]
#[ignore = "requires a native GLES context"]
fn gray_progress_clamp_mask_executes_and_completes() {
    let pool = pool();
    let mut registry = ResourceRegistry::new(4, 65536).unwrap();
    let mut register = |index: u8| {
        registry
            .register_image(
                ResourceVersion {
                    key: AssetKey(format!("gray-mask-{index}")),
                    version: ContentVersion([index; 32]),
                },
                OwnedBytes::new(
                    vec![
                        0, 0, 0, 255, 64, 64, 64, 192, 128, 128, 128, 128, 255, 255, 255, 255,
                    ],
                    pool.reserve(16).unwrap(),
                )
                .unwrap(),
                [2, 2],
                ImageFormat::Rgba8,
                ImageAlphaType::Alpha,
                ColorEncoding::Srgb,
            )
            .unwrap()
    };
    let handles = [register(0), register(1), register(2), register(3)];
    let program = MaskProgram::new(
        vec![
            MaskOp::Constant(0),
            MaskOp::Constant(1_f32.to_bits()),
            MaskOp::Parameter(0),
            MaskOp::Parameter(2),
            MaskOp::Sample {
                image: 0,
                channel: MaskChannel::Gray,
                coordinates: [3, 0, 1, 1],
                repeat: 1,
            },
            MaskOp::Subtract(2, 4),
            MaskOp::Constant(3_f32.to_bits()),
            MaskOp::Multiply(5, 6),
            MaskOp::Add(7, 1),
            MaskOp::Clamp(8, 0_f32.to_bits(), 1_f32.to_bits()),
            MaskOp::Sample {
                image: 0,
                channel: MaskChannel::Alpha,
                coordinates: [0, 0, 1, 1],
                repeat: 0,
            },
            MaskOp::Multiply(9, 10),
        ],
        vec!["gray-mask".into()],
    )
    .unwrap();
    assert_eq!(program.operations().len(), 12);
    let chain = MaskProgram::new(
        program.operations()[..10].to_vec(),
        vec!["gray-mask".into()],
    )
    .unwrap();
    let threshold = MaskProgram::image("gray-mask".into());
    let mut renderer = GlesBackend::headless([400, 300], GlesLimits::default()).unwrap();
    let mut events = Vec::new();
    #[cfg(feature = "profiling")]
    let targets_before = reel_core::profile::Sample::snapshot().calls
        [reel_core::profile::Phase::TargetCreate as usize];
    for (case, program) in [program, chain, threshold].into_iter().enumerate() {
        for (index, progress) in [0., 0.35, 0.7, 1.].into_iter().enumerate() {
            let handle = handles[index];
            let lease = registry.acquire(handle).unwrap();
            let mut scene = Scene::new();
            scene.draw(Draw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Solid(Color::WHITE),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., 0., 400., 300.)),
                composite: Composite::default(),
            });
            let mut widget = RecordWidget::new(scene);
            widget.clip = false;
            widget.mask = Some(PaintImageMask {
                program: program.clone(),
                images: vec![lease.clone()].into(),
                bounds: Rect::new(0., 0., 400., 300.),
                parameters: [progress, 0., 0.125, 0., 0.],
            });
            let mut bridge = widget_bridge(widget, Affine::IDENTITY);
            let frame = build(
                bridge
                    .record(FrameId((case * 4 + index) as u64 + 1))
                    .unwrap(),
                registry.freeze(&[handle], &pool).unwrap(),
                &pool,
            );
            if case == 0 && index == 0 {
                renderer
                    .configure(SurfaceInfo {
                        physical_size: [400, 300],
                        scale_factor: 1.,
                        surface_epoch: frame.surface_epoch(),
                        color_encoding: ColorEncoding::Srgb,
                        alpha_mode: AlphaMode::Opaque,
                    })
                    .unwrap();
            }
            assert!(matches!(
                renderer.render_present(&frame, || {}).unwrap(),
                PresentOutcome::PresentAccepted(_)
            ));
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
    }
    #[cfg(feature = "profiling")]
    assert_eq!(
        reel_core::profile::Sample::snapshot().calls
            [reel_core::profile::Phase::TargetCreate as usize]
            - targets_before,
        2,
        "paired, chain and threshold masks share the root and isolated source targets"
    );
    renderer.close().unwrap();
    eprintln!("gles masks=paired,chain,threshold progress_frames=12 all_submissions_completed");
}
