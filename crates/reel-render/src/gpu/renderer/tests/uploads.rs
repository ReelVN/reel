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
    empty(&renderer);
    eprintln!(
        "decoded RGBA: 8 three-image sets create3 working F16 textures instead of24; cleared GPU/CPU reservations"
    );
}

#[test]
fn image_plan_deduplicates_both_transition_inputs_before_admission() {
    let (frame, _, handles) = image_restore_fixture([4, 4], true);
    let plan =
        crate::gpu::image_plan::collect([&frame, &frame].into_iter(), 4096, false, |_| Ok(()))
            .unwrap();
    assert_eq!(plan.images.len(), handles.len());
    for item in &plan.images {
        assert!(handles.contains(&item.source.handle()));
        assert_eq!(item.resident, 4 * 4 * 8);
        assert_eq!(item.staging, 4 * 256);
    }
    assert!(matches!(
        crate::gpu::image_plan::collect([&frame].into_iter(), 2, false, |_| Ok(())),
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
    assert_eq!(renderer.usage().resources, 3 * resident);
    let reused = renderer.prepare(&frame).unwrap();
    let pixels = renderer.readback(reused).unwrap();
    assert!(pixels.iter().all(|p| {
        p.iter()
            .zip([0., 0., 1., 1.])
            .all(|(a, b)| (*a - b).abs() < 0.006)
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
