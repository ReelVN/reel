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
    let resident = std::fs::read_to_string("/proc/self/status")
        .ok()
        .map(|status| {
            status
                .lines()
                .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_else(|| "os_resident_bytes=unavailable".into());
    println!(
        "phase={name} live_request_bytes_delta={} {resident}",
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
#[ignore = "CPU recording and compilation measurement"]
fn transparent_overlay_recording_cost() {
    use reel_ui::{
        core::Widget,
        kurbo::{Point, Size},
    };
    let mut root = RecordWidget::new(workload(1, 0));
    root.clip = false;
    for index in 0..24 {
        let mut overlay = RecordWidget::new(workload(8, 0));
        overlay.opacity = 0.;
        root.inner.add(
            overlay.prepare(),
            Point::new(index as f64, 0.),
            Size::new(320., 240.),
        );
    }
    let mut bridge = widget_bridge(root, Affine::IDENTITY);
    let pool = pool();
    drop(build(
        bridge.record(FrameId(1)).unwrap(),
        empty_snapshot(&pool),
        &pool,
    ));
    let region = Region::new(ALLOCATOR);
    let start = Instant::now();
    let mut commands = 0;
    let mut groups = 0;
    let mut frame_bytes = 0;
    for id in 2..202 {
        let frame = build(
            bridge.record(FrameId(id)).unwrap(),
            empty_snapshot(&pool),
            &pool,
        );
        commands += frame.commands().len();
        groups += frame
            .commands()
            .iter()
            .filter(|c| matches!(c.op, FrameOp::PushGroup(_)))
            .count();
        frame_bytes += frame.accounted_bytes();
    }
    let elapsed = start.elapsed();
    let cost = region.change();
    println!(
        "transparent_overlays frames=200 elapsed_us={} commands={} groups={} frame_bytes={} allocated_bytes={} allocations={}",
        elapsed.as_micros(),
        commands,
        groups,
        frame_bytes,
        cost.bytes_allocated,
        cost.allocations
    );
}

fn probe_workloads(case: Option<&str>) -> Result<&'static [(usize, usize)], &'static str> {
    match case {
        None => Ok(&[(512, 0), (16, 2048)]),
        Some("rect") => Ok(&[(512, 0)]),
        Some("path") => Ok(&[(16, 2048)]),
        Some(_) => Err("REEL_R01_PROBE_CASE must be rect, path, or unset"),
    }
}

#[test]
fn probe_case_selection_rejects_invalid_and_defaults_to_both_workloads() {
    assert_eq!(probe_workloads(None).unwrap(), &[(512, 0), (16, 2048)]);
    assert_eq!(probe_workloads(Some("rect")).unwrap(), &[(512, 0)]);
    assert_eq!(probe_workloads(Some("path")).unwrap(), &[(16, 2048)]);
    for case in ["typo", "", "RECT", " path"] {
        assert!(probe_workloads(Some(case)).is_err(), "accepted {case:?}");
    }
}

#[test]
#[ignore = "run alone with --ignored --nocapture; measures full record/export and cleanup"]
fn record_paused_finish_cancel_and_source_drop_cost() {
    let case = match std::env::var("REEL_R01_PROBE_CASE") {
        Ok(case) => Some(case),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => panic!("REEL_R01_PROBE_CASE must be rect, path, or unset: {error}"),
    };
    let workloads = probe_workloads(case.as_deref()).expect("invalid measurement case");
    let mut completed = 0;
    for &(count, payload) in workloads {
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
        if payload == 0 {
            assert_eq!(
                export_alloc.allocations, 0,
                "ordinary rectangle export must not allocate per command"
            );
        }
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
        completed += 1;
    }
    assert_eq!(
        completed,
        workloads.len(),
        "every selected workload completed"
    );
    assert!(completed > 0, "measurement must perform record/export work");
    println!("completed_workloads={completed}");
}

#[test]
#[ignore = "run alone with --ignored --nocapture; isolates retained target copy and overlap"]
fn retained_target_copy_cost() {
    use reel_ui::{
        core::Widget,
        kurbo::{Point, Size},
    };
    fn live() -> i64 {
        let stats = ALLOCATOR.stats();
        stats.bytes_allocated as i64 - stats.bytes_deallocated as i64
    }
    for (count, payload) in [(512, 0), (16, 2048)] {
        let initial = live();
        let mut parent = RecordWidget::new(workload(512, 0));
        let child = parent.inner.add(
            RecordWidget::new(workload(count, payload)).prepare(),
            Point::new(0., 0.),
            Size::new(400., 300.),
        );
        let mut bridge = widget_bridge(parent, Affine::IDENTITY);
        let pool = pool();
        let frame = build(
            bridge.record(FrameId(1)).unwrap(),
            empty_snapshot(&pool),
            &pool,
        );
        let before = live();
        let mut times = Vec::with_capacity(20);
        let mut peak = before;
        let mut maximum_step = std::time::Duration::ZERO;
        let mut minimum_bytes = 0;
        let mut minimum_units = 0;
        let mut allocations = None;
        let mut last = None;
        for index in 0..20 {
            let region = Region::new(ALLOCATOR);
            let start = Instant::now();
            let mut build = RetainBuild::new(&frame, Some(child), limits(), &pool).unwrap();
            let mut budget = step_budget();
            loop {
                let started = Instant::now();
                let progress = build.step(budget).unwrap();
                maximum_step = maximum_step.max(started.elapsed());
                peak = peak.max(live());
                assert!(
                    progress.commands <= budget.max_commands
                        && progress.bytes <= budget.max_bytes
                        && progress.units <= budget.max_units
                );
                if progress.complete {
                    break;
                }
                minimum_bytes = minimum_bytes.max(progress.minimum_bytes);
                minimum_units = minimum_units.max(progress.minimum_units);
                budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
                budget.max_units = budget.max_units.max(progress.minimum_units);
            }
            let retained = build.finish().unwrap();
            peak = peak.max(live());
            times.push(start.elapsed());
            assert_eq!(retained.drawing().commands().len(), count + 2);
            if index == 19 {
                allocations = Some(region.change());
                last = Some(retained);
            }
        }
        times.sort();
        let overlap = live() - before;
        drop(frame);
        drop(bridge);
        let retained_only = live() - initial;
        drop(last);
        assert_eq!(pool.usage().bytes, 0);
        println!(
            "retain count={count} path_elements={payload} p50/p95/max={:?}/{:?}/{:?} max_step={maximum_step:?} minimum_bytes={minimum_bytes} minimum_units={minimum_units} overlap_delta={overlap} peak_delta={} retained_only_delta={retained_only} allocations={:?}",
            times[9],
            times[18],
            times[19],
            peak - before,
            allocations.unwrap()
        );
    }
}
