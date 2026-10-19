//! Host-side runtime costs, without a window, renderer, or device claims.
use aside_vm::{GcPhase, snapshot::SnapshotBudget};
use reel_engine::{
    Epoch, Session, SessionId, SessionIdentity,
    aside::{SessionVm, SliceBudget, VmLimits},
};
use reel_player::{aside_session::AsideSessionUi, story_runtime::StoryRuntime};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, time::Instant};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn runtime(source: &str) -> StoryRuntime {
    StoryRuntime::new(
        AsideSessionUi::from_source(
            Session::new(SessionId(91), Epoch(1)).unwrap(),
            source,
            "runtime_cost",
            1,
            "main",
            "en",
            VmLimits::default(),
        )
        .unwrap(),
    )
}

fn settle(runtime: &mut StoryRuntime, now: u64) -> (usize, u128) {
    let mut max_ns = 0;
    for steps in 1..1_000_000 {
        let start = Instant::now();
        runtime.tick(SliceBudget::default(), || now).unwrap();
        max_ns = max_ns.max(start.elapsed().as_nanos());
        if !runtime.needs_tick() || runtime.tick_interval() > std::time::Duration::from_millis(1) {
            assert_eq!(
                runtime.driver().adapter().vm().heap().phase(),
                GcPhase::Idle
            );
            return (steps, max_ns);
        }
    }
    panic!("runtime did not settle");
}

fn live_delta(region: &Region<'_, System>) -> i128 {
    let stats = region.change();
    stats.bytes_allocated as i128 - stats.bytes_deallocated as i128
}

#[test]
fn history_and_script_collection_clear_reclaim_before_sleeping() {
    for clear in ["game.clear_history();", "history.clear();"] {
        let source = if clear.starts_with("game") {
            format!(
                r#"
                fn main() {{
                    let history = game.history();
                    for i in 0..1000 {{ game.record(f"line {{i}}"); }}
                    await run(delay(1));
                    {clear}
                    if history.len() != 0 {{ throw "history not cleared"; }}
                    await run(delay(1));
                }}
            "#
            )
        } else {
            r#"
                fn main() {
                    let history = [];
                    for i in 0..1000 { history.append([i]); }
                    await run(delay(1));
                    history.clear();
                    if history.len() != 0 { throw "array not cleared"; }
                    await run(delay(1));
                }
            "#
            .into()
        };
        let mut runtime = runtime(&source);
        settle(&mut runtime, 0);
        let before = runtime.driver().adapter().vm().heap().stats();
        settle(&mut runtime, 2_000_000_000);
        let after = runtime.driver().adapter().vm().heap().stats();
        assert!(after.collections > before.collections, "{clear}");
        assert!(
            after.live_objects < before.live_objects / 2,
            "{clear}: {before:?} -> {after:?}"
        );
        assert_eq!(after.pending_bytes, 0);
        assert!(!runtime.driver().adapter().vm().heap().collection_due());
    }
}

#[test]
fn finite_object_quota_can_reclaim_short_lived_values() {
    let driver = AsideSessionUi::from_source(
        Session::new(SessionId(91), Epoch(1)).unwrap(),
        "fn main() { for i in 0..1000 { let garbage = [i]; } narrator(\"done\", history: false); }",
        "finite_cost",
        1,
        "main",
        "en",
        VmLimits {
            heap: aside_vm::HeapLimits {
                max_objects: 256,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let mut runtime = StoryRuntime::new(driver);
    for _ in 0..100_000 {
        runtime
            .tick(
                SliceBudget {
                    instructions: 1,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap();
        if !runtime.needs_tick() {
            assert!(matches!(runtime.driver().ui().session().view().interaction,
                Some(reel_engine::InteractionView::Dialogue(dialogue)) if dialogue.text() == "done"));
            return;
        }
    }
    panic!("finite quota failed to settle");
}

#[test]
#[ignore = "reproducible runtime allocation and latency measurement"]
fn runtime_cost() {
    let rows = std::env::var("REEL_COST_ROWS")
        .ok()
        .map(|s| s.parse::<usize>().unwrap())
        .unwrap_or(10_000);
    let source = format!(
        r#"
        let items = [];
        fn main() {{
            for i in 0..{rows} {{ items.append([i, i]); }}
            let label = text("stable");
            stage.replace(label);
            await run(delay(1));
            for i in 0..10000 {{ label.x = i; }}
            if items.len() != {rows} || items[{last}][1] != {last} || label.x != 9999 {{ throw "lost retained data"; }}
            for i in 0..100 {{ await run(delay(1)); label.y = i; }}
            if label.y != 99 {{ throw "lost interaction update"; }}
            await run(delay(1));
            narrator("done", history: false);
        }}
    "#,
        last = rows - 1
    );
    let mut runtime = runtime(&source);
    settle(&mut runtime, 0);
    let before = runtime.driver().adapter().vm().heap().stats();
    let region = Region::new(GLOBAL);
    let start = Instant::now();
    let (steps, max_ns) = settle(&mut runtime, 2_000_000_000);
    let elapsed = start.elapsed();
    let after = runtime.driver().adapter().vm().heap().stats();
    let allocation = region.change();
    eprintln!(
        "stable_updates,rows={rows},updates=10000,elapsed_ns={},steps={steps},max_tick_ns={max_ns},gc_cycles={},gc_units={},allocations={},allocated_bytes={},live_delta_bytes={},live_objects={},payload_bytes={}",
        elapsed.as_nanos(),
        after.collections - before.collections,
        after.gc_units - before.gc_units,
        allocation.allocations,
        allocation.bytes_allocated,
        live_delta(&region),
        after.live_objects,
        after.payload_bytes
    );

    let before = after;
    let region = Region::new(GLOBAL);
    let start = Instant::now();
    let mut max_ns = 0;
    let mut steps = 0;
    for i in 1..=100 {
        let (count, maximum) = settle(&mut runtime, (i + 1) * 2_000_000_000);
        steps += count;
        max_ns = max_ns.max(maximum);
    }
    let after = runtime.driver().adapter().vm().heap().stats();
    let allocation = region.change();
    eprintln!(
        "stable_wakeups,rows={rows},wakeups=100,elapsed_ns={},steps={steps},max_tick_ns={max_ns},gc_cycles={},gc_units={},allocations={},allocated_bytes={},live_delta_bytes={},live_objects={},payload_bytes={}",
        start.elapsed().as_nanos(),
        after.collections - before.collections,
        after.gc_units - before.gc_units,
        allocation.allocations,
        allocation.bytes_allocated,
        live_delta(&region),
        after.live_objects,
        after.payload_bytes
    );
    assert!(
        after.collections - before.collections < 20,
        "low-allocation wakeups repeatedly scanned the retained graph"
    );

    let driver = runtime.driver();
    let program = driver.adapter().vm().program().clone();
    let region = Region::new(GLOBAL);
    let start = Instant::now();
    let begin = Instant::now();
    let mut capture = driver
        .adapter()
        .snapshot_begin(driver.ui().session())
        .unwrap();
    let begin_ns = begin.elapsed().as_nanos();
    let mut peak = live_delta(&region);
    let mut max_ns = 0;
    let mut steps = 0;
    loop {
        let slice = Instant::now();
        let progress = capture.step(SnapshotBudget { units: 256 }).unwrap();
        max_ns = max_ns.max(slice.elapsed().as_nanos());
        steps += 1;
        peak = peak.max(live_delta(&region));
        if progress.complete {
            break;
        }
    }
    let finish = Instant::now();
    let mut image = capture.finish().unwrap();
    let finish_ns = finish.elapsed().as_nanos();
    peak = peak.max(live_delta(&region));
    eprintln!(
        "capture,rows={rows},elapsed_ns={},begin_ns={begin_ns},finish_ns={finish_ns},steps={steps},max_slice_ns={max_ns},allocated_bytes={},live_delta_bytes={},observed_peak_delta_bytes={peak}",
        start.elapsed().as_nanos(),
        region.change().bytes_allocated,
        live_delta(&region)
    );
    let region = Region::new(GLOBAL);
    let start = Instant::now();
    let begin = Instant::now();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(92),
            epoch: Epoch(2),
        },
        Default::default(),
        None,
        None,
    )
    .unwrap();
    let begin_ns = begin.elapsed().as_nanos();
    let mut peak = live_delta(&region);
    let mut max_ns = 0;
    let mut steps = 0;
    loop {
        let slice = Instant::now();
        let progress = restore.step(SnapshotBudget { units: 256 }).unwrap();
        max_ns = max_ns.max(slice.elapsed().as_nanos());
        steps += 1;
        peak = peak.max(live_delta(&region));
        if progress.complete {
            break;
        }
    }
    let finish = Instant::now();
    let (_session, restored) = restore.finish().unwrap();
    let finish_ns = finish.elapsed().as_nanos();
    peak = peak.max(live_delta(&region));
    eprintln!(
        "restore,rows={rows},elapsed_ns={},begin_ns={begin_ns},finish_ns={finish_ns},steps={steps},max_slice_ns={max_ns},allocated_bytes={},live_delta_bytes={},observed_peak_delta_bytes={peak},candidate_charged_bytes={}",
        start.elapsed().as_nanos(),
        region.change().bytes_allocated,
        live_delta(&region),
        restored.vm().heap().stats().charged_bytes
    );
}
