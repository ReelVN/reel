use super::*;
use crate::test_support::r01_fixture as f;
use reel_core::identity::FrameId;
use reel_ui::{
    bridge::PresentationBridge,
    frame::{FrameOp, RenderFrame},
    imaging::record::{Draw, Geometry, Scene},
    kurbo::Rect,
    peniko::{Brush, Color},
};

struct Payload(u32);
impl StorageSize for Payload {
    fn bytes(&self) -> u64 {
        0
    }
}
fn frame(count: usize) -> (PresentationBridge, RenderFrame) {
    let pool = f::pool();
    let mut scene = Scene::new();
    for index in 0..count {
        scene.draw(Draw::Fill {
            transform: Affine::translate((index as f64, 0.)),
            fill_rule: Default::default(),
            brush: Brush::Solid(Color::WHITE),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(0., 0., 20., 30.)),
            composite: Default::default(),
        });
    }
    let mut bridge = f::widget_bridge(f::RecordWidget::new(scene), Affine::IDENTITY);
    let frame = f::build(
        bridge.record(FrameId(1)).unwrap(),
        f::empty_snapshot(&pool),
        &pool,
    );
    (bridge, frame)
}

#[test]
#[ignore = "CPU recording geometry lookup measurement; no graphics adapter"]
fn recording_lookup_cost() {
    use std::{hint::black_box, time::Instant};
    for count in [8, 64, 128] {
        let (_bridge, resident) = frame(count);
        let origins: Vec<_> = resident
            .commands()
            .iter()
            .filter(|command| matches!(command.op, FrameOp::Draw(_)))
            .map(|command| command.origin)
            .collect();
        let limits = crate::common::GpuLimits::default();
        let budgets = Budgets::new(limits);
        let mut cache = Cache::default();
        for (index, origin) in origins.iter().enumerate() {
            cache.insert(
                resident.drawing(),
                *origin,
                0u32,
                Payload(index as u32),
                &budgets,
                limits.transient_bytes,
            );
        }
        let before = cache.bytes();
        let (_miss_bridge, miss_frame) = frame(1);
        let miss = miss_frame
            .commands()
            .iter()
            .find(|command| matches!(command.op, FrameOp::Draw(_)))
            .unwrap()
            .origin;
        let ordered: Vec<_> = origins
            .iter()
            .map(|origin| (resident.drawing(), *origin))
            .collect();
        let mixed: Vec<_> = (0..count)
            .map(|index| {
                if index % 4 == 3 {
                    (miss_frame.drawing(), miss)
                } else {
                    (resident.drawing(), origins[(index * 13) % count])
                }
            })
            .collect();
        for (order, queries) in [("ordered", ordered), ("mixed", mixed)] {
            let began = Instant::now();
            let mut checksum = 0;
            for _ in 0..2000 {
                cache.begin_replay();
                for &(drawing, origin) in &queries {
                    checksum += u64::from(
                        cache
                            .get(black_box(drawing), origin, black_box(&0))
                            .map_or(0, |entry| entry.value().0 + 1),
                    );
                }
            }
            assert_eq!(before, cache.bytes());
            println!(
                "recording_lookup order={order} entries={count} frames=2000 elapsed_us={} metadata_bytes={before} checksum={checksum}",
                began.elapsed().as_micros()
            );
        }
    }
}

#[test]
fn ordered_lookup_falls_back_for_reordering_and_keeps_inflight_owners() {
    let (bridge, frame) = frame(8);
    let origins: Vec<_> = frame
        .commands()
        .iter()
        .filter(|command| matches!(command.op, FrameOp::Draw(_)))
        .map(|command| command.origin)
        .collect();
    let limits = crate::common::GpuLimits::default();
    let budgets = Budgets::new(limits);
    let mut cache = Cache::default();
    for (index, origin) in origins.iter().enumerate() {
        cache.insert(
            frame.drawing(),
            *origin,
            0u32,
            Payload(index as u32),
            &budgets,
            limits.transient_bytes,
        );
    }
    let flight = cache.get(frame.drawing(), origins[3], &0).unwrap();
    cache.begin_replay();
    for index in [7, 3, 4, 5, 0, 1, 2, 6] {
        assert_eq!(
            cache
                .get(frame.drawing(), origins[index], &0)
                .unwrap()
                .value()
                .0,
            index as u32
        );
    }
    assert!(cache.get(frame.drawing(), origins[3], &1).is_none());
    drop(bridge);
    cache.begin_replay();
    assert_eq!(cache.bytes(), 0);
    assert!(
        budgets.transient.usage().bytes > 0,
        "flight owns its immutable cache entry after retirement"
    );
    assert_eq!(flight.value().0, 3);
    drop(flight);
    assert_eq!(budgets.transient.usage().bytes, 0);
}

#[test]
fn indexed_lookup_keeps_the_new_recording_when_a_slot_is_replaced() {
    let (_bridge, drawing) = frame(132);
    let origins: Vec<_> = drawing
        .commands()
        .iter()
        .filter(|command| matches!(command.op, FrameOp::Draw(_)))
        .map(|command| command.origin)
        .collect();
    let limits = crate::common::GpuLimits::default();
    let budgets = Budgets::new(limits);
    let mut cache = Cache::default();
    let mut charged_bytes = None;
    for _ in 0..64 {
        for (index, &origin) in origins.iter().enumerate() {
            cache.insert(
                drawing.drawing(),
                origin,
                0u32,
                Payload(index as u32),
                &budgets,
                limits.transient_bytes,
            );
        }
        assert_eq!(cache.bytes(), *charged_bytes.get_or_insert(cache.bytes()));
        assert_eq!(cache.bytes(), budgets.transient.usage().bytes);
    }
    cache.begin_replay();
    for (index, &origin) in origins.iter().enumerate().rev() {
        assert_eq!(
            cache
                .get(drawing.drawing(), origin, &0)
                .map(|entry| entry.value().0),
            (index >= 4).then_some(index as u32),
        );
    }
    cache.clear();
    assert_eq!(budgets.transient.usage().bytes, 0);
}

#[test]
fn moving_recordings_admit_after_settling_and_preserve_inflight_geometry() {
    let (_bridge, frame) = frame(8);
    let origins: Vec<_> = frame
        .commands()
        .iter()
        .filter(|command| matches!(command.op, FrameOp::Draw(_)))
        .map(|command| command.origin)
        .collect();
    let limits = crate::common::GpuLimits::default();
    let budgets = Budgets::new(limits);
    let mut cache = Cache::default();
    for (index, &origin) in origins.iter().enumerate() {
        cache.insert(
            frame.drawing(),
            origin,
            0u32,
            Payload(index as u32),
            &budgets,
            limits.transient_bytes,
        );
    }
    let flight = cache.get(frame.drawing(), origins[3], &0).unwrap();
    let resident_bytes = cache.bytes();
    for revision in 1..20 {
        cache.begin_replay();
        for &index in &[7, 3, 4, 5, 0, 1, 2, 6] {
            let origin = origins[index];
            assert!(cache.get(frame.drawing(), origin, &revision).is_none());
            assert!(!cache.observe(
                frame.drawing(),
                origin,
                &revision,
                &budgets,
                limits.transient_bytes
            ));
            assert!(cache.get(frame.drawing(), origin, &revision).is_none());
        }
        assert_eq!(cache.bytes(), resident_bytes);
        assert_eq!(flight.value().0, 3);
    }
    cache.begin_replay();
    for (index, &origin) in origins.iter().enumerate() {
        assert!(cache.observe(
            frame.drawing(),
            origin,
            &19,
            &budgets,
            limits.transient_bytes
        ));
        cache.insert(
            frame.drawing(),
            origin,
            19,
            Payload(100 + index as u32),
            &budgets,
            limits.transient_bytes,
        );
        assert_eq!(
            cache.get(frame.drawing(), origin, &19).unwrap().value().0,
            100 + index as u32
        );
    }
    assert_eq!(flight.value().0, 3);
    cache.clear();
    assert!(cache.get(frame.drawing(), origins[3], &19).is_none());
    assert!(budgets.transient.usage().bytes > 0);
    drop(flight);
    assert_eq!(budgets.transient.usage().bytes, 0);
}
