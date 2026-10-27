//! GPU geometry reuse and ordered batches.
use super::*;

#[test]
fn recording_geometry_borrows_stable_data_and_bounds_changing_candidates() {
    use crate::common::{
        mesh,
        recording_cache::{Cache, Scope, ShapeDependency},
    };
    let pool = pool();
    let mut widget = RecordWidget::new(workload(1, 0, Color::WHITE));
    widget.clip = false;
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    let frame = build(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let command = frame
        .commands()
        .iter()
        .find(|command| matches!(command.op, reel_ui::frame::FrameOp::Draw(_)))
        .unwrap();
    let reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::Fill { shape, .. }) = &command.op
    else {
        panic!("fill fixture")
    };
    let limits = crate::gpu::GpuLimits::default();
    let shared = Budgets::new(limits);
    let local = Budgets::new(limits);
    let mut cache = Cache::default();
    let dependency = |x| ShapeDependency {
        transform: Affine::translate((x, 0.)),
        size: [400, 300],
        uv: Affine::IDENTITY,
        color: [1.; 4],
        sampling: Default::default(),
    };
    let allocation = || mesh::Allocation::new(&shared, &local, limits.transient_bytes, 1 << 26);
    let generate = |cache: &mut Cache<_, _>, x, enabled| {
        cache
            .shape(
                frame.drawing(),
                command.origin,
                (shape, Default::default(), None),
                dependency(x),
                allocation(),
                Scope {
                    budgets: &shared,
                    limit: limits.transient_bytes,
                    enabled,
                },
            )
            .unwrap()
    };
    assert!(generate(&mut cache, 0., true).identity.is_none());
    let admitted = generate(&mut cache, 0., true);
    assert!(admitted.identity.is_some());
    drop(admitted);
    let first = generate(&mut cache, 0., false);
    let second = generate(&mut cache, 0., false);
    assert_eq!(first.vertices.as_ptr(), second.vertices.as_ptr());
    assert_eq!(first.indices.as_ptr(), second.indices.as_ptr());
    drop((first, second));
    drop(generate(&mut cache, 1., true));
    let candidate_bytes = cache.bytes();
    for step in 2..242 {
        let mesh = generate(&mut cache, f64::from(step), true);
        assert!(
            mesh.identity.is_none(),
            "changing transforms never copy into residency"
        );
        drop(mesh);
        assert_eq!(cache.bytes(), candidate_bytes);
    }
    eprintln!("moving_frames=240 cached_mesh_copies=0 candidate_bytes={candidate_bytes}");
    drop(bridge);
    assert!(
        cache
            .get(frame.drawing(), command.origin, &dependency(241.))
            .is_none()
    );
    assert_eq!(cache.bytes(), 0);
    assert_eq!(shared.transient.usage().bytes, 0);
    assert_eq!(local.transient.usage().bytes, 0);
}

#[test]
#[ignore = "requires native WGPU; no pixel readback"]
fn one_moving_triangle_updates_one_aligned_range_without_splitting_the_draw() {
    use std::sync::atomic::Ordering;
    let (mut bridge, middle, pool) = moving_triangle_fixture();
    let mut renderer = renderer(Default::default());
    for id in 1..=3 {
        let frame = build(
            bridge.record(FrameId(id)).unwrap(),
            empty_snapshot(&pool),
            &pool,
        );
        renderer.submit(renderer.prepare(&frame).unwrap()).unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
    }
    bridge.edit_widget(middle, |mut widget| {
        widget.ctx.set_transform(Affine::translate((1., 0.)))
    });
    let frame = build(
        bridge.record(FrameId(4)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let bytes = renderer.geometry.uploaded_bytes.load(Ordering::Relaxed);
    let draws = renderer.gpu.upload_creations.draws.load(Ordering::Relaxed);
    renderer.submit(renderer.prepare(&frame).unwrap()).unwrap();
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    assert_eq!(
        renderer.geometry.uploaded_bytes.load(Ordering::Relaxed) - bytes,
        3 * 48 + 8
    );
    assert_eq!(
        renderer.gpu.upload_creations.draws.load(Ordering::Relaxed) - draws,
        1
    );
}

#[cfg(all(feature = "gles2", feature = "profiling"))]
#[test]
#[ignore = "requires native GLES; no pixel readback"]
fn one_moving_triangle_gles_updates_one_native_range() {
    use crate::{
        RenderBackend,
        gles2::{GlesBackend, GlesLimits},
    };
    use reel_core::profile::GeometryUploads;
    let (mut bridge, middle, pool) = moving_triangle_fixture();
    let mut renderer = GlesBackend::headless([400, 300], GlesLimits::default()).unwrap();
    renderer
        .configure(crate::SurfaceInfo {
            physical_size: [400, 300],
            scale_factor: 1.,
            surface_epoch: reel_core::identity::SurfaceEpoch(1),
            color_encoding: reel_ui::resources::ColorEncoding::Srgb,
            alpha_mode: crate::AlphaMode::Opaque,
        })
        .unwrap();
    let complete = |renderer: &mut GlesBackend| {
        let start = Instant::now();
        while renderer.has_pending() {
            renderer.poll(&mut Vec::new(), 32).unwrap();
            assert!(start.elapsed().as_secs() < 10);
            std::thread::yield_now();
        }
    };
    for id in 1..=3 {
        let frame = build(
            bridge.record(FrameId(id)).unwrap(),
            empty_snapshot(&pool),
            &pool,
        );
        renderer.render_present(&frame, || {}).unwrap();
        complete(&mut renderer);
    }
    bridge.edit_widget(middle, |mut widget| {
        widget.ctx.set_transform(Affine::translate((1., 0.)))
    });
    let frame = build(
        bridge.record(FrameId(4)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    );
    let before = GeometryUploads::snapshot();
    renderer.render_present(&frame, || {}).unwrap();
    complete(&mut renderer);
    let upload = GeometryUploads::snapshot().delta(before);
    assert_eq!(upload.bytes, 3 * 48 + 6);
    assert_eq!(upload.calls, 2);
    renderer.close().unwrap();
}

#[test]
#[ignore = "requires actual graphics adapter; submits without pixel readback"]
fn ordered_draw_segments_share_uploads_across_scissors_and_stop_at_groups() {
    use std::sync::atomic::Ordering;
    let mut scene = workload(10, 0, Color::WHITE);
    scene.push_clip(Clip::Fill {
        transform: Affine::IDENTITY,
        shape: Geometry::Rect(Rect::new(10., 20., 300., 200.)),
        fill_rule: Default::default(),
    });
    fill(&mut scene, Color::from_rgba8(128, 64, 32, 128));
    fill(&mut scene, Color::from_rgba8(32, 64, 128, 64));
    scene.pop_clip();
    scene.push_group(Group {
        composite: Composite {
            alpha: 0.5,
            ..Default::default()
        },
        ..Default::default()
    });
    fill(&mut scene, Color::WHITE);
    fill(&mut scene, Color::from_rgba8(0, 0, 0, 128));
    scene.pop_group();
    fill(&mut scene, Color::WHITE);
    let pool = pool();
    let frame = frame(scene, &pool);
    let mut renderer = renderer(crate::gpu::GpuLimits {
        target_bytes: 64 * 48 * 4 * 2,
        ..Default::default()
    });
    let mut prepared = renderer.prepare(&frame).unwrap();
    assert_eq!(prepared.quality().samples, 1);
    let pipelines = renderer
        .pipelines
        .get(&renderer.gpu, prepared.quality())
        .unwrap();
    let solid = pipelines.direct_draws[crate::common::mesh::DirectPaint::Solid as usize]
        .load(Ordering::Relaxed);
    let opaque = pipelines.opaque_draws[crate::common::mesh::OpaquePaint::Solid as usize]
        .load(Ordering::Relaxed);
    assert!(
        opaque > 0 && solid > opaque,
        "opaque and translucent solids select their own blend policy"
    );
    assert_eq!(
        pipelines.direct_draws[crate::common::mesh::DirectPaint::Coverage as usize]
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        renderer.gpu.upload_creations.draws.load(Ordering::Relaxed),
        5,
        "opaque and translucent solids split the isolated group's ordered entries"
    );
    assert_eq!(
        renderer
            .gpu
            .upload_creations
            .mesh_buffers
            .load(Ordering::Relaxed),
        6
    );
    let surface = renderer
        .gpu
        .device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("opaque solid presentation submission"),
            size: wgpu::Extent3d {
                width: 64,
                height: 48,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
    crate::gpu::SurfacePass::new(&renderer.gpu.device, wgpu::TextureFormat::Rgba8UnormSrgb)
        .unwrap()
        .encode(
            &renderer.gpu.device,
            &mut prepared,
            &surface.create_view(&Default::default()),
        );
    renderer.submit(prepared).unwrap();
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    let cached = renderer.usage().cached_geometry;
    assert!(cached > 0);
    assert_eq!(renderer.usage().transient, cached);
    // Two prepared encoders must each own their uploads. Cancelling one cannot
    // rewrite the other's pending geometry or return either slot before a fence.
    let first = renderer.prepare(&frame).unwrap();
    assert_eq!(
        renderer
            .gpu
            .upload_creations
            .mesh_buffers
            .load(Ordering::Relaxed),
        6
    );
    let cancelled = renderer.prepare(&frame).unwrap();
    assert_eq!(
        renderer
            .gpu
            .upload_creations
            .mesh_buffers
            .load(Ordering::Relaxed),
        12
    );
    drop(cancelled);
    renderer.submit(first).unwrap();
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    assert_eq!(renderer.usage().transient, cached);
    assert_eq!(renderer.usage().cached_geometry, cached);
    let warm = renderer.prepare(&frame).unwrap();
    assert_eq!(
        renderer
            .gpu
            .upload_creations
            .mesh_buffers
            .load(Ordering::Relaxed),
        12
    );
    renderer.submit(warm).unwrap();
    renderer
        .gpu
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    renderer.poll().unwrap();
    assert_eq!(renderer.usage().transient, cached);
    assert_eq!(
        renderer
            .gpu
            .upload_creations
            .geometry_retries
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        renderer
            .gpu
            .upload_creations
            .geometry_staging
            .load(Ordering::Relaxed),
        6
    );
    let capture = renderer
        .prepare_capture(&frame, frame.info().physical_size)
        .unwrap();
    assert!(
        capture
            .geometry
            .iter()
            .all(|slot| !slot.storage.has_staging())
    );
    assert_eq!(renderer.usage().cached_geometry, 0);
    assert_eq!(
        renderer
            .gpu
            .upload_creations
            .geometry_staging
            .load(Ordering::Relaxed),
        6
    );
    drop(capture);
    assert_eq!(renderer.usage().transient, 0);
    assert_eq!(renderer.admission.usage().tickets, 0);
}

#[test]
#[ignore = "requires actual graphics adapter; submits without pixel readback"]
fn geometry_staging_yields_to_whole_frame_admission() {
    use std::sync::atomic::Ordering;
    let mut scene = Scene::new();
    for _ in 0..16 {
        scene.push_group(Group {
            composite: Composite {
                alpha: 0.5,
                ..Default::default()
            },
            ..Default::default()
        });
        for _ in 0..8 {
            fill(&mut scene, Color::WHITE);
        }
        scene.pop_group();
    }
    let frame = frame(scene, &pool());
    let baseline = renderer(Default::default());
    let original = baseline
        .prepare_cached(
            &frame,
            None,
            frame.info().physical_size,
            false,
            &crate::gpu::geometry::UploadPolicy::new(false),
            false,
        )
        .unwrap();
    let required = original.budget_scope.transient.usage().peak_bytes;
    drop(original);
    drop(baseline);
    let mut renderer = renderer(crate::gpu::GpuLimits {
        transient_bytes: required,
        ..Default::default()
    });
    for expected_retries in 1..=2 {
        let prepared = renderer.prepare(&frame).unwrap();
        assert_eq!(
            renderer
                .gpu
                .upload_creations
                .geometry_retries
                .load(Ordering::Relaxed),
            expected_retries
        );
        assert!(
            prepared
                .geometry
                .iter()
                .all(|slot| !slot.storage.has_staging())
        );
        assert!(prepared.budget_scope.transient.usage().peak_bytes <= required);
        renderer.submit(prepared).unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
        assert_eq!(renderer.usage().transient, 0);
        assert_eq!(renderer.usage().cached_geometry, 0);
    }
    eprintln!("geometry low-budget immutable_peak={required} frames=2 bounded_retries=2");
}

#[cfg(all(feature = "gles2", feature = "profiling"))]
#[test]
#[ignore = "requires native GLES and REEL_R01_TEST_FONT; no pixel readback"]
fn long_text_gles_reuses_recorded_geometry_without_uploads() {
    use crate::{
        RenderBackend,
        gles2::{GlesBackend, GlesLimits},
    };
    use reel_core::profile::GeometryUploads;
    let (_pool, _registry, _handle, frame, bridge) = long_text_fixture();
    let mut renderer =
        GlesBackend::headless(frame.info().physical_size, GlesLimits::default()).unwrap();
    renderer
        .configure(crate::SurfaceInfo {
            physical_size: frame.info().physical_size,
            scale_factor: 1.,
            surface_epoch: frame.surface_epoch(),
            color_encoding: reel_ui::resources::ColorEncoding::Srgb,
            alpha_mode: crate::AlphaMode::Opaque,
        })
        .unwrap();
    let complete = |renderer: &mut GlesBackend| {
        let start = Instant::now();
        while renderer.has_pending() {
            renderer.poll(&mut Vec::new(), 32).unwrap();
            assert!(start.elapsed().as_secs() < 10);
            std::thread::yield_now();
        }
    };
    for _ in 0..3 {
        renderer.render_present(&frame, || {}).unwrap();
        complete(&mut renderer);
    }
    let before = GeometryUploads::snapshot();
    let mut prepare_us = 0;
    for _ in 0..8 {
        let start = Instant::now();
        renderer.render_present(&frame, || {}).unwrap();
        prepare_us += start.elapsed().as_micros();
        complete(&mut renderer);
    }
    let upload = GeometryUploads::snapshot().delta(before);
    assert_eq!(upload.bytes, 0);
    assert_eq!(upload.calls, 0);
    eprintln!(
        "GLES warm_frames=8 geometry_upload_bytes={} calls={} render_present_cpu_us={prepare_us}",
        upload.bytes, upload.calls
    );
    drop(bridge);
    let before = GeometryUploads::snapshot();
    let mut prepare_us = 0;
    for _ in 0..8 {
        let start = Instant::now();
        renderer.render_present(&frame, || {}).unwrap();
        prepare_us += start.elapsed().as_micros();
        complete(&mut renderer);
    }
    let upload = GeometryUploads::snapshot().delta(before);
    assert!(upload.bytes > 0);
    eprintln!(
        "GLES same_frame_without_live_recording frames=8 geometry_upload_bytes={} calls={} render_present_cpu_us={prepare_us}",
        upload.bytes, upload.calls
    );
    renderer.close().unwrap();
}

#[test]
#[ignore = "requires actual graphics adapter"]
fn sprite_batches_share_uploads_and_respect_paint_boundaries() {
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::{
        peniko::{ImageAlphaType, ImageBrush, ImageFormat},
        resources::{ColorEncoding, ResourceData, ResourceRegistry},
    };
    use std::sync::atomic::Ordering;
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 65264).unwrap();
    let bytes = [255, 255, 255, 255].repeat(16);
    let ticket = pool.reserve(bytes.len() as u64).unwrap();
    let handle = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("sprite".into()),
                version: ContentVersion([8; 32]),
            },
            OwnedBytes::new(bytes, ticket).unwrap(),
            [4, 4],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let lease = registry.acquire(handle).unwrap();
    let ResourceData::Image { image, .. } = lease.data() else {
        unreachable!()
    };
    let mut renderer = renderer(Default::default());
    eprintln!("sprite adapter: {:?}", renderer.gpu.adapter.get_info());
    for (particles, boundary) in [(128, false), (2048, false), (16384, false), (2048, true)] {
        let mut scene = Scene::new();
        for index in 0..particles {
            if boundary && index == particles / 2 {
                scene.push_group(Group {
                    composite: Composite {
                        alpha: 0.5,
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }
            scene.draw(Draw::Fill {
                transform: Affine::translate(((index % 100) as f64, (index / 100) as f64)),
                fill_rule: Default::default(),
                brush: Brush::Image(
                    ImageBrush::new(image.clone()).with_alpha((index % 10 + 1) as f32 / 10.),
                ),
                brush_transform: Some(Affine::scale(0.5)),
                shape: Geometry::Rect(Rect::new(0., 0., 2., 2.)),
                composite: Composite::default(),
            });
        }
        if boundary {
            scene.pop_group();
        }
        let frame = frame_with_snapshot(scene, registry.freeze(&[handle], &pool).unwrap(), &pool);
        let before = [
            &renderer.gpu.upload_creations.draws,
            &renderer.gpu.upload_creations.mesh_buffers,
            &renderer.gpu.upload_creations.brush_groups,
        ]
        .map(|v| v.load(Ordering::Relaxed));
        let start = Instant::now();
        let prepared = renderer.prepare(&frame).unwrap();
        let preparation = start.elapsed();
        let transient_peak = prepared.budget_scope.transient.usage().peak_bytes;
        let resources_peak = prepared.budget_scope.resources.usage().peak_bytes;
        let after = [
            &renderer.gpu.upload_creations.draws,
            &renderer.gpu.upload_creations.mesh_buffers,
            &renderer.gpu.upload_creations.brush_groups,
        ]
        .map(|v| v.load(Ordering::Relaxed));
        let batches = if boundary { 2 } else { 1 };
        assert_eq!(
            std::array::from_fn::<_, 3, _>(|i| after[i] - before[i]),
            [batches, if boundary { 0 } else { 2 }, batches]
        );
        renderer.submit(prepared).unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
        eprintln!(
            "{particles} sprites group_boundary={boundary}: draws={batches}, buffer_creations={}, brush_groups={batches}, prepare={preparation:?}, prepare_submit_complete={:?}, transient_peak_bytes={transient_peak}, image_peak_bytes={resources_peak}",
            after[1] - before[1],
            start.elapsed()
        );
    }
    counts(&renderer, 1);
}

#[test]
#[ignore = "requires actual graphics adapter; submits without pixel readback"]
fn image_sampling_changes_split_batches_and_share_resident_texture() {
    use reel_ui::{
        peniko::{ImageBrush, ImageQuality},
        resources::ResourceData,
    };
    use std::sync::atomic::Ordering;
    let pool = pool();
    let (_original, registry, handles) = image_restore_fixture([4, 4], true);
    let lease = registry.acquire(handles[0]).unwrap();
    let ResourceData::Image { image, .. } = lease.data() else {
        unreachable!()
    };
    let make_frame = |qualities: &[ImageQuality]| {
        let mut scene = Scene::new();
        for (index, &quality) in qualities.iter().enumerate() {
            scene.draw(Draw::Fill {
                transform: Affine::translate((index as f64 * 50., 0.)),
                fill_rule: Default::default(),
                brush: Brush::Image(ImageBrush::new(image.clone()).with_quality(quality)),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., 0., 40., 40.)),
                composite: Composite::default(),
            });
        }
        frame_with_snapshot(scene, registry.freeze(&handles[..1], &pool).unwrap(), &pool)
    };
    let mut renderer = renderer(Default::default());
    for (first, second) in [
        (ImageQuality::Low, ImageQuality::Medium),
        (ImageQuality::Medium, ImageQuality::Low),
    ] {
        let frame = make_frame(&[first, first, second, second, first]);
        let draws = renderer.gpu.upload_creations.draws.load(Ordering::Relaxed);
        let prepared = renderer.prepare(&frame).unwrap();
        assert_eq!(
            renderer.gpu.upload_creations.draws.load(Ordering::Relaxed) - draws,
            3,
            "each consecutive sampling mode forms one ordered batch"
        );
        counts(&renderer, 1);
        renderer.submit(prepared).unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
        assert_eq!(renderer.usage().resources, 4 * 4 * 8);
    }
    let frame = make_frame(&[ImageQuality::High]);
    assert!(matches!(
        renderer.prepare(&frame),
        Err(GpuError::Unsupported {
            record: "high-quality image sampling",
            ..
        })
    ));
    counts(&renderer, 1);
}
