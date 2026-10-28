#[allow(dead_code)]
#[path = "support/fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/r01_fixture.rs"]
mod r01_fixture;
use r01_fixture::*;
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
    texture::*,
};
use reel_ui::{
    frame::*,
    imaging::record::Scene,
    kurbo::{Affine, Rect},
    resources::*,
};

fn resource(pool: &BudgetPool) -> (ResourceRegistry, ResourceLease) {
    let mut registry = ResourceRegistry::new(1, 65536).unwrap();
    let descriptor = TextureDescriptor {
        storage_layout: TextureStorageLayout::Linear,
        mip_count: 1,
        format: TextureFormat::Rgba8,
        source_encoding: ColorEncoding::Srgb,
        layout: TextureLayout::identity([16, 8]),
    };
    let bytes = descriptor.byte_len().unwrap();
    let handle = registry
        .register_texture(
            ResourceVersion {
                key: AssetKey("sprites".into()),
                version: ContentVersion([1; 32]),
            },
            TexturePayload::new(
                descriptor,
                OwnedBytes::new(vec![255; bytes as usize], pool.reserve(bytes).unwrap()).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let lease = registry.acquire(handle).unwrap();
    (registry, lease)
}
fn instances(pool: &BudgetPool, n: usize) -> SpriteInstances {
    SpriteInstances::try_collect(pool, |emit| {
        for i in 0..n {
            emit(SpriteInstance {
                position: [i as f64, 20.],
                size: 10. + (i % 4) as f64,
                rotation: i as f64 * 0.1,
                alpha: 0.2 + (i % 4) as f32 * 0.2,
            });
        }
    })
    .unwrap()
}
#[test]
fn empty_sprite_recording_needs_no_storage_admission() {
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 0,
            max_bytes: 0,
        },
    );
    assert!(instances(&pool, 0).as_slice().is_empty());
    assert_eq!(pool.usage().bytes, 0);
}
fn widget(lease: ResourceLease, data: Result<SpriteInstances, BuildErrorKind>) -> RecordWidget {
    let mut w = RecordWidget::new(Scene::new());
    w.opacity = 0.5;
    w.sprite_draws.push((
        lease,
        Rect::new(0., 0., 16., 8.),
        Affine::translate((3., 4.)),
        data,
    ));
    w
}
#[test]
fn sprite_payload_obeys_slices_ownership_and_retained_resource_identity() {
    assert_eq!(std::mem::size_of::<SpriteInstance>(), 40);
    assert_eq!(std::mem::size_of::<FrameSprite>(), 40);
    let pool = pool();
    let (registry, lease) = resource(&pool);
    let data = instances(&pool, 97);
    let expected: Vec<_> = data
        .as_slice()
        .iter()
        .map(|s| s.compiled(Rect::new(0., 0., 16., 8.)))
        .collect();
    let mut bridge = widget_bridge(widget(lease.clone(), Ok(data)), Affine::translate((7., 9.)));
    let source = bridge.record(FrameId(1)).unwrap();
    let snapshots = ResourceSnapshot::capture([Ok(lease.clone())].into_iter(), &pool).unwrap();
    let mut build = FrameBuild::new(
        source,
        snapshots,
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        &pool,
    )
    .unwrap();
    let mut budget = StepBudget {
        max_commands: 1,
        max_units: 1,
        max_bytes: 1,
    };
    let mut yielded = 0;
    for _ in 0..10000 {
        let p = build.step(budget).unwrap();
        assert!(p.units <= budget.max_units && p.bytes <= budget.max_bytes && p.commands <= 1);
        if p.complete {
            break;
        }
        yielded += 1;
        budget.max_units = budget.max_units.max(p.minimum_units);
        budget.max_bytes = budget.max_bytes.max(p.minimum_bytes);
    }
    assert!(yielded > 7);
    let frame = build.finish().unwrap();
    let draws: Vec<_> = frame
        .commands()
        .iter()
        .filter_map(|c| {
            if let FrameOp::Draw(FrameDraw::Sprites(s)) = &c.op {
                Some(s)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(draws.len(), 1);
    assert_eq!(draws[0].instances, expected);
    assert_eq!(draws[0].transform, Affine::translate((10., 13.)));
    assert_eq!(draws[0].brush.image.resource, lease.handle());
    assert!(
        frame
            .commands()
            .iter()
            .any(|c| matches!(&c.op,FrameOp::PushGroup(g) if g.composite.alpha==0.5))
    );
    let mut retain = RetainBuild::new(&frame, None, limits(), &pool).unwrap();
    let mut slice = step_budget();
    for _ in 0..10000 {
        let p = retain.step(slice).unwrap();
        if p.complete {
            break;
        }
        slice.max_units = slice.max_units.max(p.minimum_units);
        slice.max_bytes = slice.max_bytes.max(p.minimum_bytes);
    }
    let retained = retain.finish().unwrap();
    assert_eq!(
        retained.drawing().resources().leases()[0].handle(),
        lease.handle()
    );
    let saved = retained.snapshot(|_| None).unwrap();
    let mut restore = saved.restore_begin(limits(), &pool).unwrap();
    let mut slice = step_budget();
    for _ in 0..10000 {
        let p = restore.step(slice).unwrap().build;
        if p.complete {
            break;
        }
        slice.max_units = slice.max_units.max(p.minimum_units);
        slice.max_bytes = slice.max_bytes.max(p.minimum_bytes);
    }
    let restored = restore.finish().unwrap();
    let batch = restored
        .drawing()
        .commands()
        .iter()
        .find_map(|c| {
            if let FrameOp::Draw(FrameDraw::Sprites(s)) = &c.op {
                Some(s)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(batch.instances, expected);
    assert_eq!(
        batch.brush.image.resource,
        restored.drawing().resources().leases()[0].handle()
    );
    drop((saved, restored));
    drop((retained, frame, bridge, registry, lease));
    assert_eq!(pool.usage().bytes, 0);
}
#[test]
fn sprite_recording_pressure_is_a_localized_frame_rejection() {
    let pool = pool();
    let (_registry, lease) = resource(&pool);
    let small = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 1,
            max_bytes: 64,
        },
    );
    let data = SpriteInstances::try_collect(&small, |emit| {
        emit(SpriteInstance {
            position: [0.; 2],
            size: 1.,
            rotation: 0.,
            alpha: 1.,
        })
    });
    assert!(matches!(data, Err(BuildErrorKind::Bytes)));
    assert_eq!(small.usage().bytes, 0);
    let mut bridge = widget_bridge(widget(lease.clone(), data), Affine::IDENTITY);
    let source = bridge.record(FrameId(1)).unwrap();
    let mut build = FrameBuild::new(
        source,
        ResourceSnapshot::capture([Ok(lease)].into_iter(), &pool).unwrap(),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        &pool,
    )
    .unwrap();
    let err = loop {
        match build.step(step_budget()) {
            Ok(p) => assert!(!p.complete),
            Err(e) => break e,
        }
    };
    assert_eq!(err.kind, BuildErrorKind::Bytes);
    assert_eq!(bridge.pending_count(), 0);
}
#[test]
fn cancelled_sprite_copy_releases_candidate_and_payload_capacity() {
    let pool = pool();
    let (registry, lease) = resource(&pool);
    let mut bridge = widget_bridge(
        widget(lease.clone(), Ok(instances(&pool, 1024))),
        Affine::IDENTITY,
    );
    let source = bridge.record(FrameId(1)).unwrap();
    let mut build = FrameBuild::new(
        source,
        ResourceSnapshot::capture([Ok(lease.clone())].into_iter(), &pool).unwrap(),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        &pool,
    )
    .unwrap();
    build
        .step(StepBudget {
            max_commands: 1,
            max_units: 64,
            max_bytes: 64,
        })
        .unwrap();
    bridge.clear();
    assert_eq!(
        build.step(step_budget()).unwrap_err().kind,
        BuildErrorKind::Cancelled
    );
    drop((build, bridge, registry, lease));
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
#[ignore = "release differential sprite recording and frame-build measurement"]
fn sprite_recording_cost() {
    use std::time::Instant;
    const N: usize = 4096;
    for compact in [false, true] {
        let mut times = Vec::new();
        let mut recorded = 0;
        let mut frame_bytes = 0;
        for iteration in 0..15 {
            let pool = pool();
            let (_registry, lease) = resource(&pool);
            let data = instances(&pool, N);
            let started = Instant::now();
            let mut w = RecordWidget::new(Scene::new());
            w.clip = false;
            if compact {
                let recorded = SpriteInstances::try_collect(&pool, |emit| {
                    for &sprite in data.as_slice() {
                        emit(sprite);
                    }
                })
                .unwrap();
                w.sprite_draws.push((
                    lease.clone(),
                    Rect::new(0., 0., 16., 8.),
                    Affine::IDENTITY,
                    Ok(recorded),
                ));
            } else {
                for s in data.as_slice() {
                    let transform = Affine::translate((s.position[0], s.position[1]))
                        * Affine::rotate(s.rotation)
                        * Affine::scale(s.size / 16.)
                        * Affine::translate((-8., -4.));
                    w.resource_draws.push((
                        lease.clone(),
                        Rect::new(0., 0., 16., 8.),
                        transform,
                        reel_ui::peniko::ImageSampler {
                            alpha: s.alpha,
                            ..Default::default()
                        },
                    ));
                }
            }
            let mut bridge = widget_bridge(w, Affine::IDENTITY);
            let source = bridge.record(FrameId(iteration)).unwrap();
            recorded = source
                .plan()
                .layers
                .iter()
                .filter_map(|l| {
                    if let reel_ui::app::VisualLayerKind::Scene(s) = &l.kind {
                        Some(s.commands().len())
                    } else {
                        None
                    }
                })
                .sum::<usize>();
            let mut build = FrameBuild::new(
                source,
                ResourceSnapshot::capture([Ok(lease)].into_iter(), &pool).unwrap(),
                FrameInfo {
                    logical_size: [400., 300.],
                    physical_size: [400, 300],
                },
                limits(),
                &pool,
            )
            .unwrap();
            let budget = StepBudget {
                max_commands: u32::MAX,
                max_units: u64::MAX,
                max_bytes: u64::MAX,
            };
            assert!(build.step(budget).unwrap().complete);
            let frame = build.finish().unwrap();
            times.push(started.elapsed().as_micros());
            frame_bytes = frame.accounted_bytes();
            let count = frame
                .commands()
                .iter()
                .map(|c| match &c.op {
                    FrameOp::Draw(FrameDraw::Sprites(s)) => s.instances.len(),
                    FrameOp::Draw(FrameDraw::Fill { .. }) => 1,
                    _ => 0,
                })
                .sum::<usize>();
            assert_eq!(count, N);
            std::hint::black_box(frame);
        }
        times.sort_unstable();
        println!(
            "sprites compact={compact} instances={N} median_us={} records={recorded} frame_bytes={frame_bytes} samples={times:?}",
            times[times.len() / 2]
        );
    }
}
