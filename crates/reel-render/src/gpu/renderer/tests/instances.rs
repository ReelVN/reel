//! Submit real recorded sprite inputs without pixel readback.
use super::*;
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, ResourceVersion},
};
use reel_ui::{
    frame::{SpriteInstance, SpriteInstances},
    peniko::{ImageAlphaType, ImageFormat},
    resources::{ColorEncoding, ResourceRegistry},
};
use std::sync::atomic::Ordering;

#[test]
#[ignore = "requires actual graphics adapter; validates lazy shader variants without readback"]
fn sprite_instances_compile_the_original_fragment_variants_and_reuse_programs() {
    use crate::common::mesh::{DirectPaint as D, OpaquePaint as O};
    let renderer = renderer(Default::default());
    let quality = crate::gpu::Quality {
        format: wgpu::TextureFormat::Rgba8Unorm,
        samples: 1,
        degraded: true,
    };
    let pipelines = renderer.pipelines.get(&renderer.gpu, quality).unwrap();
    for (direct, bounded, opaque) in std::iter::once((None, false, None))
        .chain(
            [
                D::Rgba,
                D::Bc1,
                D::Bc3,
                D::RegionalRgba,
                D::RegionalBc1,
                D::RegionalBc3,
                D::Coverage,
                D::Solid,
            ]
            .map(|d| (Some(d), false, None)),
        )
        .chain([D::Rgba, D::Bc1, D::Bc3, D::Solid].map(|d| (Some(d), true, None)))
        .chain(
            [
                (D::Bc1, O::Bc1),
                (D::RegionalBc1, O::RegionalBc1),
                (D::Solid, O::Solid),
            ]
            .map(|(d, o)| (Some(d), false, Some(o))),
        )
        .chain([(D::Bc1, O::Bc1), (D::Solid, O::Solid)].map(|(d, o)| (Some(d), true, Some(o))))
    {
        let first = pipelines
            .instance_pipeline(&renderer.gpu, quality, direct, bounded, opaque)
            .unwrap();
        let second = pipelines
            .instance_pipeline(&renderer.gpu, quality, direct, bounded, opaque)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }
}

#[test]
#[ignore = "requires actual graphics adapter; no pixel readback"]
fn exact_sprite_instances_reduce_uploads_and_preserve_mixed_batches_and_retirement() {
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 1 << 20).unwrap();
    let bytes = [255, 255, 255, 255].repeat(16);
    let ticket = pool.reserve(bytes.len() as u64).unwrap();
    let handle = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("instance-source".into()),
                version: ContentVersion([19; 32]),
            },
            OwnedBytes::new(bytes, ticket).unwrap(),
            [4, 4],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Linear,
        )
        .unwrap();
    let make_frame = |counts: &[usize], mixed| {
        let mut widget = RecordWidget::new(Scene::new());
        if mixed {
            fill(&mut widget.own, Color::from_rgba8(64, 32, 16, 128));
        }
        for &count in counts {
            let instances = SpriteInstances::try_collect(&pool, |emit| {
                for index in 0..count {
                    emit(SpriteInstance {
                        position: [30. + (index % 100) as f64, 30. + (index / 100 % 100) as f64],
                        size: 4.,
                        rotation: index as f64 * 0.01,
                        alpha: 0.5,
                    });
                }
            });
            widget.sprite_draws.push((
                registry.acquire(handle).unwrap(),
                Rect::new(0., 0., 4., 4.),
                Affine::IDENTITY,
                instances,
            ));
        }
        if mixed {
            fill(&mut widget.post, Color::from_rgba8(32, 16, 8, 64));
        }
        frame_from_widget(widget, registry.freeze(&[handle], &pool).unwrap(), &pool)
    };
    for (counts, mixed, expected_instances) in [
        (vec![500, 500], false, 1),
        (vec![1000, 1000], false, 2),
        (vec![2], false, 0),
        (vec![64, 64], true, 0),
    ] {
        let frame = make_frame(&counts, mixed);
        assert_eq!(
            frame
                .commands()
                .iter()
                .filter(|c| matches!(
                    c.op,
                    reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::Sprites(_))
                ))
                .count(),
            counts.len()
        );
        let mut baseline = renderer(Default::default());
        baseline.sprite_instances_enabled = false;
        let old = baseline.prepare(&frame).unwrap();
        let old_draws = baseline.gpu.upload_creations.draws.load(Ordering::Relaxed);
        let old_bytes = baseline.geometry.uploaded_bytes.load(Ordering::Relaxed);
        let old_peak = old.budget_scope.transient.usage().peak_bytes;
        let mut optimized = renderer(Default::default());
        let prepared = optimized.prepare(&frame).unwrap();
        let instances = prepared
            .geometry
            .iter()
            .filter(|g| g.storage.instances.is_some())
            .count();
        assert_eq!(
            instances, expected_instances,
            "counts={counts:?} mixed={mixed}"
        );
        let draws = optimized.gpu.upload_creations.draws.load(Ordering::Relaxed);
        let bytes = optimized.geometry.uploaded_bytes.load(Ordering::Relaxed);
        assert!(draws <= old_draws, "preserve original draw opportunities");
        if expected_instances > 0 {
            assert_eq!(
                bytes,
                counts.iter().sum::<usize>() as u64 * 48 + 124 * expected_instances as u64
            );
            assert!(bytes * 2 < old_bytes);
            assert!(prepared.budget_scope.transient.usage().peak_bytes < old_peak);
        } else {
            assert_eq!(bytes, old_bytes);
            assert_eq!(draws, old_draws);
        }
        let peak = prepared.budget_scope.transient.usage().peak_bytes;
        baseline.submit(old).unwrap();
        optimized.submit(prepared).unwrap();
        assert_eq!(
            optimized.inflight[0]
                .geometry
                .iter()
                .filter(|g| g.storage.instances.is_some())
                .count(),
            expected_instances
        );
        for r in [&mut baseline, &mut optimized] {
            r.gpu
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            r.poll().unwrap();
        }
        let buffers = optimized
            .gpu
            .upload_creations
            .mesh_buffers
            .load(Ordering::Relaxed);
        let prepared = optimized.prepare(&frame).unwrap();
        assert_eq!(
            optimized
                .gpu
                .upload_creations
                .mesh_buffers
                .load(Ordering::Relaxed),
            buffers,
            "retired buffers must be reusable"
        );
        optimized.submit(prepared).unwrap();
        optimized
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        optimized.poll().unwrap();
        eprintln!(
            "counts={counts:?} mixed={mixed} instances={instances} draws={old_draws}->{draws} uploaded={old_bytes}->{bytes} transient_peak={old_peak}->{peak}"
        );
    }
}
