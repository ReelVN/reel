//! Resource uploads, restoration and retirement.
use super::*;

#[test]
#[ignore = "requires actual graphics adapter; submits without pixel readback"]
fn retired_decoded_images_reuse_completed_storage_across_resource_versions() {
    let mut renderer = renderer(Default::default());
    for _ in 0..8 {
        let (frame, mut registry, handles) = image_restore_fixture([8, 8], true);
        let prepared = renderer.prepare(&frame).unwrap();
        for image in prepared.images.values() {
            assert_eq!(image.texture.format(), wgpu::TextureFormat::Rgba16Float);
            assert_eq!(image.residency.as_ref().unwrap().bytes(), 8 * 8 * 8);
        }
        assert_eq!(renderer.gpu.upload_creations.snapshot()[0], 3);
        let versions: Vec<_> = frame
            .resources()
            .leases()
            .iter()
            .map(|lease| lease.version().clone())
            .collect();
        renderer.submit(prepared).unwrap();
        for handle in handles {
            registry.retire(handle);
        }
        renderer.retire(&versions);
        drop(frame);
        drop(registry);
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
    }
    assert_eq!(renderer.usage().cached_images, 0);
    assert_eq!(renderer.usage().retired_images, 3 * 8 * 8 * 8);
    assert_eq!(renderer.usage().retired_image_readers, 0);
    for _ in 0..100 {
        renderer.poll().unwrap();
    }
    assert_eq!(renderer.usage().retired_images, 3 * 8 * 8 * 8);
    let blank = frame(Scene::new(), &pool());
    for age in 1..=16 {
        let prepared = renderer.prepare(&blank).unwrap();
        renderer.submit(prepared).unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
        assert_eq!(
            renderer.usage().retired_images,
            if age < 16 { 3 * 8 * 8 * 8 } else { 0 }
        );
    }
    empty(&renderer);
    eprintln!(
        "decoded RGBA: 8 three-image sets create3 textures instead of24; 100 polls preserve reuse; 16 cold submissions retire1536B without pressure"
    );
}

#[test]
fn image_plan_deduplicates_both_transition_inputs_before_admission() {
    let (frame, _, handles) = image_restore_fixture([4, 4], true);
    let plan = crate::gpu::image_plan::collect(
        [&frame, &frame].into_iter(),
        4096,
        false,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(plan.images.len(), handles.len());
    for item in &plan.images {
        assert!(handles.contains(&item.source.handle()));
        assert_eq!(item.resident, 4 * 4 * 8);
        assert_eq!(item.staging, 4 * 256);
    }
    assert!(matches!(
        crate::gpu::image_plan::collect(
            [&frame].into_iter(),
            2,
            false,
            &Default::default(),
            |_| Ok(())
        ),
        Err(GpuError::InvalidSize)
    ));
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn cold_restore_batches_survive_cancel_and_release_only_completed_staging() {
    let size = [1920, 1080];
    let (frame, mut registry, handles) = image_restore_fixture(size, true);
    let resident = u64::from(size[0]) * u64::from(size[1]) * 8;
    let mut renderer = renderer(crate::gpu::GpuLimits {
        resource_bytes: 64 << 20,
        // Exercise residency pressure independently of the per-prepare work quota.
        image_upload_bytes_per_prepare: 2 * resident,
        ..Default::default()
    });
    assert!(matches!(
        renderer.prepare(&frame),
        Err(GpuError::Pressure { .. })
    ));
    counts(&renderer, 2);
    assert_eq!(renderer.usage().resources, 4 * resident);
    assert!(
        renderer.has_pending(),
        "upload progress must wake a frame which was never submitted"
    );
    assert!(matches!(
        renderer.prepare(&frame),
        Err(GpuError::Pressure { .. })
    ));
    counts(&renderer, 2); // Retrying before completion does not recreate textures.
    renderer.images.clear_idle();
    assert_eq!(renderer.usage().resources, 4 * resident);
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert_eq!(
        renderer.usage().resources,
        4 * resident,
        "callbacks do not retire accounting"
    );
    assert!(
        renderer.poll().unwrap().is_empty(),
        "uploads have no presentation identity"
    );
    assert_eq!(renderer.usage().resources, 4 * resident);
    renderer.images.clear_idle(); // Completed staging is now cached and reclaimable.
    assert_eq!(renderer.usage().resources, 2 * resident);
    let first = renderer.prepare(&frame).unwrap();
    counts(&renderer, 3);
    drop(first); // Cancelled rendering retains independently submitted uploads.
    let second = renderer.prepare(&frame).unwrap();
    counts(&renderer, 3);
    renderer.submit(second).unwrap();
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert_eq!(renderer.poll().unwrap().len(), 1);
    assert_eq!(renderer.usage().resources, 4 * resident);
    renderer.images.clear_idle();
    assert_eq!(renderer.usage().resources, 3 * resident);
    let reused = renderer.prepare(&frame).unwrap();
    let pixels = renderer.readback(reused).unwrap();
    assert!(pixels.iter().enumerate().all(|(index, p)| {
        let mut expected = [0., 0., 0., 1.];
        expected[((index % 64) / 20).min(2)] = 1.;
        p.iter().zip(expected).all(|(a, b)| (*a - b).abs() < 0.006)
    }));
    counts(&renderer, 3);
    let retained = renderer.prepare(&frame).unwrap();
    let versions: Vec<_> = frame
        .resources()
        .leases()
        .iter()
        .map(|lease| lease.version().clone())
        .collect();
    for handle in handles {
        assert!(registry.retire(handle));
    }
    renderer.retire(&versions);
    assert_eq!(renderer.usage().resources, 3 * resident);
    drop(retained);
    empty(&renderer);
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn cold_upload_progress_with_one_submission_slot() {
    let (frame, mut registry, handles) = image_restore_fixture([4, 4], true);
    // Retirement prevents new registry leases, not rendering with the immutable
    // frame's existing leases. A transition/capture can require exactly this.
    for handle in handles {
        assert!(registry.retire(handle));
    }
    let mut renderer = renderer(crate::gpu::GpuLimits {
        max_inflight: 1,
        ..Default::default()
    });
    assert!(matches!(renderer.prepare(&frame), Err(GpuError::Busy)));
    counts(&renderer, 3);
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert!(renderer.poll().unwrap().is_empty());
    renderer.submit(renderer.prepare(&frame).unwrap()).unwrap();
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert_eq!(renderer.poll().unwrap().len(), 1);
    counts(&renderer, 3);
    drop(frame);
    renderer.poll().unwrap();
    empty(&renderer); // The cache does not own decoded pixels after completion.
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn image_upload_slices_share_one_prepare_quota_and_release_abandoned_work() {
    let (frame, mut registry, handles) = image_restore_fixture([64, 64], true);
    let mut renderer = renderer(crate::gpu::GpuLimits {
        max_inflight: 1,
        image_upload_bytes_per_prepare: 1024, // Two 512-byte rows, across all images.
        ..Default::default()
    });
    let mut attempts = 0;
    loop {
        attempts += 1;
        let before = renderer.gpu.upload_creations.snapshot();
        let result = renderer.prepare(&frame);
        let after = renderer.gpu.upload_creations.snapshot();
        assert!(
            after[1] - before[1] <= 1,
            "one slice, even at image boundaries"
        );
        match result {
            Ok(prepared) => {
                drop(prepared);
                break;
            }
            Err(error) => assert!(error.is_retryable(), "{error:?}"),
        }
        assert!(renderer.has_pending());
        assert!(renderer.has_pending_image_uploads());
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
        assert!(!renderer.has_pending_image_uploads());
        assert!(attempts <= 97);
    }
    assert_eq!(attempts, 97); // 96 slices followed by the admitted ready frame.
    assert_eq!(renderer.gpu.upload_creations.snapshot(), [3, 1, 96]);
    for handle in handles {
        assert!(registry.retire(handle));
    }
    drop(frame);
    renderer.poll().unwrap();
    renderer.images.clear_idle();
    assert_eq!(renderer.usage().resources, 0);
    assert_eq!(renderer.usage().transient, 0);

    let (candidate, _, _) = image_restore_fixture([64, 64], true);
    assert!(matches!(
        renderer.prepare(&candidate),
        Err(GpuError::Pressure { .. })
    ));
    assert!(renderer.usage().resources > 0);
    // Replacing the candidate drops its Partial; submitted staging lives only to completion.
    let plain = frame_from_widget(
        RecordWidget::new(Scene::new()),
        empty_snapshot(&pool()),
        &pool(),
    );
    assert!(matches!(renderer.prepare(&plain), Err(GpuError::Busy)));
    drop(candidate);
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    renderer.images.clear_idle();
    assert_eq!(renderer.usage().resources, 0);
    assert_eq!(renderer.usage().transient, 0);
}

#[test]
#[ignore = "requires actual graphics adapter; submits without pixel readback"]
fn retired_image_cooling_keeps_source_leases_until_the_last_reader_retires() {
    let mut renderer = renderer(Default::default());
    let (source, mut registry, handles) = image_restore_fixture([8, 8], true);
    let prepared = renderer.prepare(&source).unwrap();
    renderer.submit(prepared).unwrap();
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    let versions: Vec<_> = source
        .resources()
        .leases()
        .iter()
        .map(|l| l.version().clone())
        .collect();
    for handle in handles {
        registry.retire(handle);
    }
    renderer.retire(&versions);
    assert_eq!(renderer.usage().cached_images, 0);
    assert_eq!(renderer.usage().retired_image_readers, 3 * 8 * 8 * 8);
    let blank = frame(Scene::new(), &pool());
    for _ in 0..32 {
        let prepared = renderer.prepare(&blank).unwrap();
        renderer.submit(prepared).unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
    }
    assert_eq!(renderer.usage().retired_image_readers, 3 * 8 * 8 * 8);
    drop(source);
    drop(registry);
    renderer.poll().unwrap();
    assert_eq!(renderer.usage().retired_images, 0);
    empty(&renderer);
}

#[test]
#[ignore = "requires graphics adapter; no pixel readback"]
fn submitted_cached_images_release_and_restore_before_reupload() {
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::{
        peniko::{ImageAlphaType, ImageFormat},
        resources::{ColorEncoding, ResourceRegistry},
    };
    fn prepare_full(
        renderer: &mut Renderer,
        frame: &RenderFrame,
        source: &reel_ui::resources::ResourceLease,
    ) -> Prepared {
        loop {
            match renderer.prepare(frame) {
                Ok(prepared) => return prepared,
                Err(GpuError::Pressure { .. }) => {
                    renderer
                        .gpu
                        .device
                        .poll(wgpu::PollType::wait_indefinitely())
                        .unwrap();
                    renderer.poll().unwrap();
                    assert!(
                        source.pixels_resident(),
                        "partial upload must retain the CPU reader"
                    );
                }
                Err(error) => panic!("image preparation: {error}"),
            }
        }
    }
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 65536).unwrap();
    let version = ResourceVersion {
        key: AssetKey("reloadable-test".into()),
        version: ContentVersion([9; 32]),
    };
    let create = |registry: &mut ResourceRegistry| {
        registry
            .register_image(
                version.clone(),
                OwnedBytes::new(
                    vec![255; 4 * 1024 * 1024],
                    pool.reserve(4 * 1024 * 1024).unwrap(),
                )
                .unwrap(),
                [1024, 1024],
                ImageFormat::Rgba8,
                ImageAlphaType::Alpha,
                ColorEncoding::Srgb,
            )
            .unwrap()
    };
    let handle = create(&mut registry);
    let lease = registry.acquire(handle).unwrap();
    lease.permit_pixel_reload();
    let mut widget = RecordWidget::new(Scene::new());
    widget.resource_draws.push((
        lease.clone(),
        Rect::new(0., 0., 400., 300.),
        Affine::IDENTITY,
        Default::default(),
    ));
    let frame = frame_from_widget(widget, registry.freeze(&[handle], &pool).unwrap(), &pool);
    let before = pool.usage().bytes;
    let mut renderer = renderer(Default::default());
    let prepared = prepare_full(&mut renderer, &frame, &lease);
    renderer.submit(prepared).unwrap();
    assert!(
        lease.pixels_resident(),
        "submitted work must retain pixels before completion"
    );
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    assert!(
        !lease.pixels_resident(),
        "completed cached texture releases reloadable pixels"
    );
    let released = before - pool.usage().bytes;
    assert_eq!(released, 1024 * 1024 * 4);
    eprintln!(
        "completed 1024x1024 image: CPU bytes before={before}, after={}, released={released}",
        pool.usage().bytes
    );
    assert!(
        renderer.prepare(&frame).is_ok(),
        "cached GPU image needs no CPU reload"
    );
    renderer.images.retain_demand(&[]);
    let demand = match renderer.prepare(&frame) {
        Err(GpuError::PixelsNeeded(demand)) => demand,
        _ => panic!("cache miss must request exact source pixels"),
    };
    assert_eq!(demand.resources()[0].handle(), handle);
    drop(demand);
    let mut replacement_registry = ResourceRegistry::new(1, 65536).unwrap();
    let replacement_handle = create(&mut replacement_registry);
    let replacement = replacement_registry.acquire(replacement_handle).unwrap();
    lease.restore_pixels(&replacement).unwrap();
    drop(replacement);
    drop(replacement_registry);
    let prepared = prepare_full(&mut renderer, &frame, &lease);
    renderer.submit(prepared).unwrap();
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    assert!(!lease.pixels_resident());
}
