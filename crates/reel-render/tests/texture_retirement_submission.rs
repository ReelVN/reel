//! Existing frame leases keep bounded uploads and warm textures reusable after
//! registry retirement. These checks submit and wait without reading pixels.
#![cfg(feature = "gles2")]
#[allow(dead_code, reason = "shared UI fixture")]
#[path = "../../reel-ui/tests/support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared frame fixture")]
#[path = "../../reel-ui/tests/support/r01_fixture.rs"]
mod r01_fixture;
use r01_fixture::*;
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
    texture::{TextureDescriptor, TextureFormat, TextureLayout, TexturePayload},
};
use reel_ui::{
    frame::{FrameBuild, FrameInfo, RenderFrame},
    imaging::record::Scene,
    kurbo::{Affine, Rect},
    resources::{ColorEncoding, ResourceRegistry, ResourceWeak},
};
const SIZE: [u32; 2] = [64, 48];
const IMAGE_BYTES: u64 = 32 * 8 * 4;
const ROW_BYTES: u64 = 32 * 4;
const PAYLOAD_BYTES: u64 = 64 * 16 * 4;

fn retired_frame() -> (RenderFrame, ResourceWeak) {
    let pool = pool();
    let mut registry = ResourceRegistry::new(2, 8192).unwrap();
    let bytes = OwnedBytes::new(
        vec![128; PAYLOAD_BYTES as usize],
        pool.reserve(PAYLOAD_BYTES).unwrap(),
    )
    .unwrap();
    let descriptor = TextureDescriptor {
        format: TextureFormat::Rgba8,
        // Both axes have removable offline padding. The source stride stays
        // 64 pixels while GPU copies use 32, and the last eight rows are skipped.
        layout: TextureLayout {
            logical_size: [32, 8],
            content_size: [32, 8],
            storage_size: [64, 16],
        },
        source_encoding: ColorEncoding::Linear,
    };
    let handle = registry
        .register_texture(
            ResourceVersion {
                key: AssetKey("retained-texture".into()),
                version: ContentVersion([7; 32]),
            },
            TexturePayload::new(descriptor, bytes).unwrap(),
        )
        .unwrap();
    let lease = registry.acquire(handle).unwrap();
    let weak = lease.downgrade();
    let mut widget = RecordWidget::new(Scene::new());
    widget.clip = false;
    widget.resource_draws.push((
        lease,
        Rect::new(0., 0., 32., 8.),
        Affine::scale_non_uniform(2., 6.),
        Default::default(),
    ));
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[handle], &pool).unwrap(),
        FrameInfo {
            logical_size: [64., 48.],
            physical_size: SIZE,
        },
        limits(),
        &pool,
    )
    .unwrap();
    while !build.step(step_budget()).unwrap().complete {}
    let frame = build.finish().unwrap();
    assert!(registry.retire(handle));
    assert!(registry.acquire(handle).is_err());
    (frame, weak)
}

#[cfg(feature = "gles2")]
#[test]
#[ignore = "requires a native GLES context"]
fn gles_retired_frame_finishes_row_upload_once_and_keeps_warm_storage() {
    use reel_render::{
        AlphaMode, PresentOutcome, RenderBackend, RenderEvent, SurfaceInfo,
        gles2::{GlesBackend, GlesError, GlesLimits},
    };
    let (frame, weak) = retired_frame();
    let mut renderer = GlesBackend::headless(
        SIZE,
        GlesLimits {
            image_upload_bytes_per_prepare: ROW_BYTES,
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
    let mut uploads = 0;
    for presented in 0..3 {
        let mut accepted = false;
        for attempt in 0..8 {
            match renderer.render_present(&frame, || {}) {
                Ok(PresentOutcome::PresentAccepted(_)) => accepted = true,
                Err(GlesError::Busy) if presented == 0 && attempt < 7 => {}
                other => panic!("unexpected upload attempt {presented}/{attempt}: {other:?}"),
            }
            let start = std::time::Instant::now();
            let mut events = Vec::new();
            while renderer.has_pending() {
                events.clear();
                renderer.poll(&mut events, 32).unwrap();
                uploads += events
                    .iter()
                    .filter(|e| matches!(e, RenderEvent::ResourcesAvailable))
                    .count();
                assert!(start.elapsed() < std::time::Duration::from_secs(10));
                std::thread::yield_now();
            }
            if accepted {
                break;
            }
        }
        assert!(accepted, "the retained upload must reach its final row");
        assert_eq!(
            uploads, 7,
            "seven row uploads precede the first complete render; warm renders reuse it"
        );
    }
    let resident = renderer.usage().bytes;
    drop(frame);
    assert!(weak.is_released());
    renderer.poll(&mut Vec::new(), 32).unwrap();
    assert_eq!(
        resident - renderer.usage().bytes,
        IMAGE_BYTES,
        "final frame lease releases cached image residency"
    );
    renderer.close().unwrap();
}
