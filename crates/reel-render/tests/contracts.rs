#[path = "support/color.rs"]
mod color;
#[allow(dead_code, reason = "shared real U02 fixtures")]
#[path = "../../reel-ui/tests/support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared real R01 fixtures")]
#[path = "../../reel-ui/tests/support/r01_fixture.rs"]
mod r01_fixture;
use r01_fixture::*;
use reel_core::{
    budget::{BudgetPool, OwnedBytes},
    identity::{AssetKey, ContentVersion, FrameId, Scope, SurfaceEpoch},
    result::{RejectCode, Terminal},
};
use reel_render::*;
use reel_ui::{bridge::*, frame::*, resources::*};
use reel_ui::{
    imaging::{
        Composite,
        record::{Draw, Geometry, Scene},
    },
    kurbo::{Affine, Rect},
    peniko::{Brush, ImageBrush},
};
fn limits() -> ConsumerLimits {
    ConsumerLimits {
        max_uploads: 2,
        max_resources: 4,
        max_frames: 2,
        max_events: 16,
        max_upload_bytes: 1024,
        max_retained_bytes: 4096,
        max_metadata_bytes: 8192,
        max_fixed_bytes: 16384,
    }
}
fn surface() -> SurfaceInfo {
    SurfaceInfo {
        physical_size: [400, 300],
        scale_factor: 1.,
        surface_epoch: SurfaceEpoch(1),
        color_encoding: ColorEncoding::Srgb,
        alpha_mode: AlphaMode::Premultiplied,
    }
}
fn version(n: u8) -> ResourceVersion {
    ResourceVersion {
        key: AssetKey("image".into()),
        version: ContentVersion([n; 32]),
    }
}
fn batch(resource: ResourceLease, pool: &BudgetPool, stride: u64) -> UploadBatch {
    let data = vec![255, 0, 0, 255];
    let bytes = OwnedBytes::new(data, pool.reserve(4).unwrap()).unwrap();
    let items = vec![UploadItem {
        resource,
        payload: UploadPayload::Pixels {
            size: [1, 1],
            stride,
            format: PixelFormat::Rgba8,
            encoding: ColorEncoding::Srgb,
            alpha: AlphaMode::Straight,
            bytes,
        },
    }];
    let storage = pool
        .reserve((items.capacity() * size_of::<UploadItem>()) as u64)
        .unwrap();
    UploadBatch {
        items,
        scope: Scope::Process,
        storage,
    }
}
fn image_scene(image: &reel_ui::peniko::ImageData) -> Scene {
    let mut scene = Scene::new();
    for _ in 0..2 {
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Image(ImageBrush::new(image.clone())),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(0., 0., 20., 20.)),
            composite: Composite::default(),
        });
    }
    scene
}
#[test]
fn borrowed_present_retains_old_versions_until_last_lease_and_completion() {
    let pool = pool();
    let source_pool = r01_fixture::pool();
    let mut pixels = Vec::with_capacity(128);
    pixels.extend([255, 0, 0, 255]);
    let capacity = pixels.capacity() as u64;
    let pixels = OwnedBytes::new(pixels, source_pool.reserve(capacity).unwrap()).unwrap();
    let mut registry = ResourceRegistry::new(2, 8192).unwrap();
    let old = version(1);
    let h = registry
        .register_image(
            old.clone(),
            pixels,
            [1, 1],
            reel_ui::peniko::ImageFormat::Rgba8,
            reel_ui::peniko::ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let lease = registry.acquire(h).unwrap();
    let ResourceData::Image { image, .. } = lease.data() else {
        unreachable!()
    };
    let image = image.clone();
    let weak = lease.downgrade();
    let mut bridge = widget_bridge(RecordWidget::new(image_scene(&image)), Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[h], &pool).unwrap(),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        r01_fixture::limits(),
        &pool,
    )
    .unwrap();
    build.step(step_budget()).unwrap();
    registry.retire(h);
    assert!(registry.acquire(h).is_err());
    assert_eq!(
        registry.register(
            old.clone(),
            ResourceData::Image {
                image: image.clone(),
                encoding: ColorEncoding::Srgb
            },
            pool.reserve(4).unwrap(),
        ),
        Err(ResourceError::RetiredOrForeign),
        "registration cannot resurrect a retired version while its old leases exist"
    );
    let mut backend = RecordingBackend::new(limits()).unwrap();
    backend.configure(surface()).unwrap();
    let upload = backend.upload(batch(lease, &pool, 4)).unwrap();
    assert!(bridge.accepted_key().is_none());
    assert_eq!(backend.pending_upload_count(), 1);
    backend.retire(std::slice::from_ref(&old));
    assert!(backend.complete_upload(upload, Terminal::Completed(())));
    assert!(!backend.complete_upload(upload, Terminal::Completed(())));
    while !build.step(step_budget()).unwrap().complete {}
    let frame = build.finish().unwrap();
    assert!(bridge.accepted_key().is_none());
    let resources: Vec<_> = frame
        .commands()
        .iter()
        .filter_map(|c| {
            if let FrameOp::Draw(FrameDraw::Fill {
                brush: Brush::Image(b),
                ..
            }) = &c.op
            {
                Some(b.image.resource)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(resources, vec![h, h]);
    assert_eq!(frame.resources().leases().len(), 1);
    let result = backend.render_present(&frame, || {});
    assert_eq!(result, Ok(PresentOutcome::PresentAccepted(FrameId(1))));
    let submit = backend.last_submission().unwrap();
    assert_eq!(
        bridge.resolve(presentation_notice(&frame, SurfaceEpoch(1), result).unwrap()),
        NoticeResult::Accepted
    );
    // A new content version can coexist while the retired old slot remains leased.
    let new_image = fixture::image_data([0, 0, 255, 255]);
    let newer = registry
        .register(
            version(2),
            ResourceData::Image {
                image: new_image,
                encoding: ColorEncoding::Srgb,
            },
            pool.reserve(4).unwrap(),
        )
        .unwrap();
    assert_ne!(newer.slot, h.slot);
    drop(frame);
    bridge.clear();
    assert!(
        !weak.is_released(),
        "borrowed submission retains an owned immutable frame"
    );
    let mut events = vec![];
    backend.poll(&mut events, 16).unwrap();
    assert!(events.iter().any(|e|matches!(e,RenderEvent::UploadFinished {id,result:Terminal::Completed(()),..} if *id==upload)));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, RenderEvent::ResourcesRetired { .. }))
    );
    assert!(backend.complete_render(submit));
    assert!(!backend.complete_render(submit));
    assert!(weak.is_released());
    backend.poll(&mut events, 1).unwrap();
    backend.poll(&mut events, 16).unwrap();
    assert_eq!(backend.resident_count(), 0);
    assert!(events.iter().any(
        |e| matches!(e,RenderEvent::ResourcesRetired {versions,..} if versions==&vec![old.clone()])
    ));
    let reused = registry
        .register(
            version(3),
            ResourceData::External { token: 9 },
            pool.reserve(0).unwrap(),
        )
        .unwrap();
    assert_eq!(reused.slot, h.slot);
    assert!(reused.generation > h.generation);
    assert!(registry.acquire(h).is_err());
    assert_eq!(source_pool.usage().bytes, capacity);
    drop(image);
    assert_eq!(
        source_pool.usage().bytes,
        capacity,
        "Masonry producer caches retain the blob"
    );
    drop(bridge);
    assert_eq!(
        source_pool.usage().bytes,
        0,
        "the final producer Blob releases its full capacity"
    );
}
#[test]
fn owned_font_storage_follows_producer_blob_aliases_after_retirement() {
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 8192).unwrap();
    let mut bytes = Vec::with_capacity(64);
    bytes.extend([1, 2, 3]);
    let capacity = bytes.capacity() as u64;
    let bytes = OwnedBytes::new(bytes, pool.reserve(capacity).unwrap()).unwrap();
    let handle = registry.register_font(version(1), bytes).unwrap();
    let lease = registry.acquire(handle).unwrap();
    let ResourceData::Font(font) = lease.data() else {
        unreachable!()
    };
    let producer = font.clone();
    assert_eq!(producer.as_ref(), &[1, 2, 3]);
    registry.retire(handle);
    drop(lease);
    drop(registry);
    assert_eq!(pool.usage().bytes, capacity);
    drop(producer);
    assert_eq!(pool.usage().bytes, 0);
}
#[test]
fn invalid_upload_returns_original_busy_terminal_capacity_and_null_skip() {
    let pool = pool();
    let mut registry = ResourceRegistry::new(3, 8192).unwrap();
    let image = fixture::image_data([255, 0, 0, 255]);
    let h = registry
        .register(
            version(1),
            ResourceData::Image {
                image: image.clone(),
                encoding: ColorEncoding::Srgb,
            },
            pool.reserve(4).unwrap(),
        )
        .unwrap();
    let mut backend = RecordingBackend::new(limits()).unwrap();
    let rejected = backend
        .upload(batch(registry.acquire(h).unwrap(), &pool, u64::MAX))
        .unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::InvalidRequest);
    assert_eq!(rejected.request.items[0].resource.handle(), h);
    assert_eq!(backend.pending_upload_count(), 0);
    drop(rejected);
    let baseline = pool.usage();
    let upload = backend
        .upload(batch(registry.acquire(h).unwrap(), &pool, 4))
        .unwrap();
    let rejected = backend
        .upload(batch(registry.acquire(h).unwrap(), &pool, 4))
        .unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    drop(rejected);
    assert!(backend.complete_upload(upload, Terminal::Cancelled));
    assert!(!backend.complete_upload(upload, Terminal::Cancelled));
    assert_eq!(pool.usage().tickets, baseline.tickets);
    assert_eq!(pool.usage().bytes, baseline.bytes);
    let mut null = NullBackend::new(limits()).unwrap();
    assert!(!null.capabilities().rasterizes);
    null.configure(surface()).unwrap();
    let mut bridge = widget_bridge(RecordWidget::new(Scene::new()), Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let result = null.render_present(&frame, || {});
    assert_eq!(
        bridge.resolve(presentation_notice(&frame, SurfaceEpoch(1), result).unwrap()),
        NoticeResult::NotPresented
    );
    assert!(bridge.accepted_key().is_none());
    let mut tiny = limits();
    tiny.max_events = 1;
    let mut blocked = RecordingBackend::new(tiny).unwrap();
    let err = blocked
        .upload(batch(registry.acquire(h).unwrap(), &pool, 4))
        .unwrap_err();
    assert_eq!(err.reason.code, RejectCode::Busy);
    assert_eq!(blocked.pending_upload_count(), 0);
}
#[test]
fn linear_alpha_mask_group_and_crossfade_analytic_expectations() {
    use color::*;
    let gray = linear_premultiplied([0.5, 0.5, 0.5, 0.5]);
    assert!((gray[0] - 0.10702057024111628).abs() < 1e-12);
    assert!((luminance_mask(gray) - 0.10702057024111628).abs() < 1e-12);
    assert_eq!(alpha_mask(gray), 0.5);
    let red = linear_premultiplied([1., 0., 0., 1.]);
    let blue = linear_premultiplied([0., 0., 1., 1.]);
    let overlapped = over(blue, red);
    assert_eq!(
        group_result(group_result(overlapped, 1., 0.5), 1., 0.5),
        [0., 0., 0.25, 0.25]
    );
    assert_eq!(crossfade(red, blue, 0.25), [0.75, 0., 0.25, 1.]);
    assert!((luminance_mask(linear_premultiplied([1., 0., 0., 0.5])) - 0.1063).abs() < 1e-12);
}

#[test]
fn queued_upload_bytes_are_cumulative_and_return_the_original_batch() {
    let pool = pool();
    let mut registry = ResourceRegistry::new(2, 8192).unwrap();
    let handles: Vec<_> = (1..=2)
        .map(|n| {
            registry
                .register(
                    version(n),
                    ResourceData::Image {
                        image: fixture::image_data([n, 0, 0, 255]),
                        encoding: ColorEncoding::Srgb,
                    },
                    pool.reserve(4).unwrap(),
                )
                .unwrap()
        })
        .collect();
    for terminal in [
        Terminal::Completed(()),
        Terminal::Cancelled,
        Terminal::Failed(RenderError::Busy),
    ] {
        let mut quota = limits();
        quota.max_upload_bytes = 4;
        quota.max_retained_bytes = 8;
        let mut backend = RecordingBackend::new(quota).unwrap();
        let first = backend
            .upload(batch(registry.acquire(handles[0]).unwrap(), &pool, 4))
            .unwrap();
        let second = batch(registry.acquire(handles[1]).unwrap(), &pool, 4);
        let original_items = second.items.as_ptr();
        let original_storage = second.storage.bytes();
        let usage = pool.usage();
        let rejected = backend
            .upload(second)
            .expect_err("two 4-byte pending batches exceed a 4-byte queue");
        assert_eq!(rejected.reason.code, RejectCode::Busy);
        assert_eq!(rejected.request.items.as_ptr(), original_items);
        assert_eq!(rejected.request.storage.bytes(), original_storage);
        assert_eq!(rejected.request.items[0].resource.handle(), handles[1]);
        assert_eq!(pool.usage(), usage);
        assert_eq!(backend.retained_bytes(), 4);
        assert_eq!(backend.queued_upload_bytes(), 4);
        assert!(backend.complete_upload(first, terminal));
        assert!(!backend.complete_upload(first, Terminal::Cancelled));
        assert_eq!(backend.queued_upload_bytes(), 0);
        let second = backend.upload(rejected.request).unwrap();
        assert_eq!(backend.pending_upload_count(), 1);
        assert_eq!(backend.queued_upload_bytes(), 4);
        assert!(backend.complete_upload(second, Terminal::Cancelled));
        assert_eq!(backend.queued_upload_bytes(), 0);
    }
}

#[test]
fn accepted_notice_survives_frame_drop_and_render_completion() {
    let pool = pool();
    let mut bridge = widget_bridge(
        RecordWidget::new(Scene::new()),
        Affine::translate((17., 23.)),
    );
    let mut backend = RecordingBackend::new(limits()).unwrap();
    backend.configure(surface()).unwrap();
    for id in 1..=3 {
        let frame = build(
            bridge.record(FrameId(id)).unwrap(),
            empty_snapshot(&pool),
            &pool,
        );
        let expected_transform = frame.geometry().records()[0].window_transform;
        assert!(
            presentation_notice::<RenderError>(
                &frame,
                SurfaceEpoch(1),
                Ok(PresentOutcome::Preparing),
            )
            .is_none()
        );
        assert_eq!(bridge.pending_count(), 1);
        let result = backend.render_present(&frame, || {});
        let notice = presentation_notice(&frame, SurfaceEpoch(1), result).unwrap();
        let submission = backend.last_submission().unwrap();
        let duplicate = notice.clone();
        drop(frame);
        assert!(backend.complete_render(submission));
        assert_eq!(
            pool.usage().bytes,
            0,
            "notice retains no frame/output resources"
        );
        assert_eq!(
            bridge.pending_count(),
            1,
            "Accepted notice owns candidate eligibility"
        );
        assert!(bridge.snapshot_usage().bytes > 0);
        drop(notice);
        assert_eq!(
            bridge.pending_count(),
            1,
            "a surviving notice clone retains eligibility"
        );
        if id == 1 {
            assert_eq!(bridge.resolve(duplicate), NoticeResult::Accepted);
            let reel_ui::app::InputGeometrySource::Presented(Some(view)) =
                bridge.root().input_geometry_source()
            else {
                panic!("accepted view missing")
            };
            assert_eq!(view.records()[0].window_transform, expected_transform);
        } else if id == 2 {
            drop(duplicate);
            assert_eq!(
                bridge.pending_count(),
                0,
                "last unresolved notice drop cancels"
            );
            assert_eq!(bridge.accepted_key().unwrap().frame_id(), FrameId(1));
        } else {
            bridge.replace_surface(SurfaceEpoch(2));
            assert_eq!(bridge.resolve(duplicate), NoticeResult::StaleOrForeign);
        }
        let mut events = vec![];
        backend.poll(&mut events, 16).unwrap();
    }
    assert_eq!(bridge.snapshot_usage().bytes, 0);
}
