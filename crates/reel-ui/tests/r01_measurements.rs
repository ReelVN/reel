//! Isolated real recording/export residency probe; no VM execution or graphics device.
#[allow(dead_code, reason = "shared real U02 fixtures")]
#[path = "support/fixture.rs"]
mod fixture;
#[allow(dead_code, reason = "shared real R01 fixtures")]
#[path = "support/r01_fixture.rs"]
mod r01_fixture;
use r01_fixture::*;
use reel_core::identity::FrameId;
use reel_ui::frame::*;
use reel_ui::{
    imaging::{
        Composite,
        record::{Draw, Geometry, Scene},
    },
    kurbo::{Affine, BezPath, Rect},
    peniko::{Brush, Color},
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, Stats, StatsAlloc};
use std::{alloc::System, time::Instant};
#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;
fn phase(name: &str, initial: Stats) {
    let stats = ALLOCATOR.stats();
    let live = stats.bytes_allocated as i64 - stats.bytes_deallocated as i64;
    let baseline = initial.bytes_allocated as i64 - initial.bytes_deallocated as i64;
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let rss = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .unwrap();
    let hwm = status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))
        .unwrap();
    println!(
        "phase={name} live_request_bytes_delta={} {rss} {hwm}",
        live - baseline
    );
}
fn workload(count: usize, payload: usize) -> Scene {
    let mut scene = Scene::new();
    for i in 0..count {
        let shape = if payload == 0 {
            Geometry::Rect(Rect::new(
                (i % 32) as f64 * 10.,
                (i / 32) as f64 * 10.,
                (i % 32) as f64 * 10. + 9.,
                (i / 32) as f64 * 10. + 9.,
            ))
        } else {
            let mut p = BezPath::with_capacity(payload + 1);
            p.move_to((0., 0.));
            for j in 0..payload {
                p.line_to(((j % 400) as f64, (j / 400) as f64));
            }
            Geometry::Path(p)
        };
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(Color::WHITE),
            brush_transform: None,
            shape,
            composite: Composite::default(),
        });
    }
    scene
}
#[test]
#[ignore = "run alone with --ignored --nocapture; measures full record/export and cleanup"]
fn record_paused_finish_cancel_and_source_drop_cost() {
    for (count, payload) in [(512, 0), (16, 2048)] {
        if std::env::var("REEL_R01_PROBE_CASE")
            .is_ok_and(|case| case != if payload == 0 { "rect" } else { "path" })
        {
            continue;
        }
        let initial = ALLOCATOR.stats();
        let mut bridge = widget_bridge(
            RecordWidget::new(workload(count, payload)),
            Affine::IDENTITY,
        );
        let pool = pool();
        phase("producer_before_record", initial);
        let region = Region::new(ALLOCATOR);
        let clock = Instant::now();
        let source = bridge.record(FrameId(1)).unwrap();
        let record_time = clock.elapsed();
        let record_alloc = region.change();
        phase("recorded_source", initial);
        let region = Region::new(ALLOCATOR);
        let clock = Instant::now();
        let mut build = FrameBuild::new(
            source,
            empty_snapshot(&pool),
            FrameInfo {
                logical_size: [400., 300.],
                physical_size: [400, 300],
            },
            limits(),
            &pool,
        )
        .unwrap();
        let setup_time = clock.elapsed();
        let setup_alloc = region.change();
        let region = Region::new(ALLOCATOR);
        let clock = Instant::now();
        build.step(step_budget()).unwrap();
        let first_step_time = clock.elapsed();
        let first_step_alloc = region.change();
        phase("paused_source_and_output", initial);
        let region = Region::new(ALLOCATOR);
        let clock = Instant::now();
        let mut max_step = first_step_time;
        let mut steps = 1;
        loop {
            let slice = Instant::now();
            let p = build.step(step_budget()).unwrap();
            max_step = max_step.max(slice.elapsed());
            steps += 1;
            if p.complete {
                break;
            }
        }
        let export_time = clock.elapsed();
        let export_alloc = region.change();
        phase("complete_before_source_release", initial);
        let region = Region::new(ALLOCATOR);
        let clock = Instant::now();
        let frame = build.finish().unwrap();
        let finish_time = clock.elapsed();
        let finish_alloc = region.change();
        let full_build_time = setup_time + first_step_time + export_time + finish_time;
        phase("immutable_frame_source_released", initial);
        assert!(frame.accounted_bytes() <= frame.reserved_bytes());
        let frame_accounted = frame.accounted_bytes();
        drop(frame);
        assert_eq!(bridge.pending_count(), 0);
        assert_eq!(pool.usage().bytes, 0);
        phase("frame_dropped_cache_retained", initial);
        let source = bridge.record(FrameId(2)).unwrap();
        let mut build = FrameBuild::new(
            source,
            empty_snapshot(&pool),
            FrameInfo {
                logical_size: [400., 300.],
                physical_size: [400, 300],
            },
            limits(),
            &pool,
        )
        .unwrap();
        build.step(step_budget()).unwrap();
        phase("paused_cancel", initial);
        let clock = Instant::now();
        build.cancel();
        let cancel_time = clock.elapsed();
        assert_eq!(bridge.pending_count(), 0);
        phase("cancelled", initial);
        let source = bridge.record(FrameId(3)).unwrap();
        let region = Region::new(ALLOCATOR);
        let clock = Instant::now();
        drop(source);
        let drop_time = clock.elapsed();
        let drop_alloc = region.change();
        phase("isolated_source_drop_cache_retained", initial);
        let mut warm_record = Vec::with_capacity(20);
        let mut warm_export = Vec::with_capacity(20);
        for id in 4..24 {
            let clock = Instant::now();
            let source = bridge.record(FrameId(id)).unwrap();
            warm_record.push(clock.elapsed());
            let clock = Instant::now();
            let frame = r01_fixture::build(source, empty_snapshot(&pool), &pool);
            warm_export.push(clock.elapsed());
            drop(frame);
        }
        warm_record.sort();
        warm_export.sort();
        let clock = Instant::now();
        drop(bridge);
        let producer_drop = clock.elapsed();
        phase("producer_cache_dropped", initial);
        println!(
            "workload count={count} path_elements={payload} record={record_time:?} setup={setup_time:?} first_step={first_step_time:?} remaining_export={export_time:?} full_setup_export_finish={full_build_time:?} finish_source_cleanup={finish_time:?} cancel={cancel_time:?} isolated_source_drop={drop_time:?} producer_drop={producer_drop:?} max_step={max_step:?} steps={steps} frame_accounted={frame_accounted}"
        );
        println!(
            "record_alloc={record_alloc:?} setup_alloc={setup_alloc:?} first_step_alloc={first_step_alloc:?} export_alloc={export_alloc:?} finish_alloc={finish_alloc:?} source_drop_alloc={drop_alloc:?}"
        );
        println!(
            "20 warm runs record p50/p95/max={:?}/{:?}/{:?}; full setup/export/finish p50/p95/max={:?}/{:?}/{:?}",
            warm_record[9],
            warm_record[18],
            warm_record[19],
            warm_export[9],
            warm_export[18],
            warm_export[19]
        );
    }
}
