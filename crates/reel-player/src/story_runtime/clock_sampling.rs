//! Host continuation wakes preserve producer cadence; actual input samples time.
use super::*;
use reel_engine::{Epoch, InputAction, Session, SessionId, aside::VmLimits};

#[test]
fn runtime_control_footprint() {
    // Moving the scheduler or a save result transfers session ownership, not its VM buffers.
    assert!(size_of::<StoryRuntime>() < 4096);
    assert!(
        size_of::<
            Result<crate::saves::Capture, (Box<AsideSessionUi>, reel_engine::aside::AdapterError)>,
        >() < 512
    );
}

fn runtime(code: &str) -> StoryRuntime {
    StoryRuntime::new(
        AsideSessionUi::from_source(
            Session::new(SessionId(78), Epoch(1)).unwrap(),
            code,
            "clock_sampling",
            1,
            "main",
            "en",
            VmLimits::default(),
        )
        .unwrap(),
    )
}

fn settle(story: &mut StoryRuntime, now: u64) {
    for _ in 0..10_000 {
        story.tick(SliceBudget::default(), || now).unwrap();
        if !story.needs_work() {
            return;
        }
    }
    panic!("host batch did not settle");
}

#[test]
fn particle_deadlines_do_not_accumulate_late_host_wakes_or_replay_missed_samples() {
    let mut story = runtime(
        r#"fn main() {
            let p=particles("spark.png"); stage.replace(p);
            emit(p,burst:1,rate:0,lifetime:10,duration:10);
            narrator("waiting");
        }"#,
    );
    settle(&mut story, 0);
    let mut now = 0;
    for frame in 1u64..=120 {
        let expected = (frame * 1_000_000_000).div_ceil(60);
        let deadline = story
            .wake_deadline(reel_core::clock::MonoTime(now))
            .unwrap()
            .0;
        assert!(
            deadline.abs_diff(expected) <= 1,
            "frame={frame}, deadline={deadline}, expected={expected}"
        );
        now = deadline + 400_000;
        settle(&mut story, now);
        assert_eq!(story.logical_ns, now);
    }
    now += 125_000_000;
    settle(&mut story, now);
    assert_eq!(
        story.logical_ns, now,
        "a late host samples actual time once"
    );
    let expected = ((now * 60 / 1_000_000_000 + 1) * 1_000_000_000).div_ceil(60);
    let deadline = story
        .wake_deadline(reel_core::clock::MonoTime(now))
        .unwrap()
        .0;
    assert!(deadline.abs_diff(expected) <= 1);
}

#[test]
fn animation_cadence_follows_high_refresh_displays_and_explicit_game_rates() {
    for (configured, display, rate) in [
        (0., 120_000, 120_000),
        (0., 144_000, 144_000),
        (0., 59_940, 59_940),
        (144., 60_000, 144_000),
    ] {
        for producer in [
            "let p=particles(\"spark.png\"); stage.replace(p); emit(p,burst:1,rate:0,lifetime:10,duration:10);",
            "let p=text(\"moving\"); stage.replace(p); run(tween(p,x:100,duration:10));",
        ] {
            let mut story = runtime(&format!(
                "fn main(){{ game.configure(frame_rate:{configured}); {producer} narrator(\"waiting\"); }}"
            ));
            story
                .driver_mut()
                .unwrap()
                .adapter_mut()
                .set_window_info(std::sync::Arc::new(reel_core::window::WindowInfo {
                    refresh_rate_millihertz: Some(display),
                    ..Default::default()
                }));
            settle(&mut story, 0);
            let mut now = 0;
            for frame in 1u64..=300 {
                let expected = (frame * 1_000_000_000_000).div_ceil(rate);
                let deadline = story
                    .wake_deadline(reel_core::clock::MonoTime(now))
                    .unwrap()
                    .0;
                assert!(
                    deadline.abs_diff(expected) <= 1,
                    "rate={rate} frame={frame}: {deadline} != {expected}"
                );
                now = deadline + 400_000;
                settle(&mut story, now);
                assert_eq!(story.logical_ns, now);
            }
        }
    }
}

#[test]
fn continuation_and_collection_wakes_do_not_oversample_a_running_clock() {
    let mut story = runtime(
        r#"fn main() {
            let timer=clock(interval:1.0/60);
            stage.replace(text("clock",x:derive(||timer.elapsed)));
            narrator("waiting");
        }"#,
    );
    settle(&mut story, 0);
    settle(&mut story, 16_666_667);
    let before = story.driver().adapter().vm().stats();
    assert_eq!(story.logical_ns, 16_666_667);
    for time in 17..33 {
        story.driver_mut().unwrap().begin_collection().unwrap();
        story.collecting = true;
        settle(&mut story, time * 1_000_000);
        assert_eq!(story.logical_ns, 16_666_667);
    }
    assert_eq!(
        story.driver().adapter().vm().stats().native_entries,
        before.native_entries
    );
    settle(&mut story, 33_333_334);
    assert_eq!(story.logical_ns, 33_333_334);
    let stage = story.driver().adapter().stage();
    let x = stage.node(stage.root().unwrap()).unwrap().props.x;
    assert_eq!(x, 2. / 60.);
    // Compact derived reads need no native entry; the projected elapsed time
    // proves that the next producer deadline was actually sampled.
}

#[test]
fn input_wakes_start_short_delays_at_current_time_with_or_without_a_running_clock() {
    for timed in [false, true] {
        let code = format!(
            "fn main() {{ {} narrator(\"waiting\"); await run(delay(0.003)); narrator(\"done\"); }}",
            if timed {
                "let timer=clock(interval:1);"
            } else {
                ""
            }
        );
        let mut story = runtime(&code);
        settle(&mut story, 0);
        let session = story.driver().ui().session();
        let owner = session.root_owner();
        let id = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            story
                .driver_mut()
                .unwrap()
                .ui_mut()
                .session_mut()
                .handle_local_input(owner, InputAction::Advance(id))
                .unwrap();
        }
        let event_time = if timed { 1_000_000 } else { 10_000_000_000 };
        story.wake_input();
        settle(&mut story, event_time);
        assert_eq!(story.logical_ns, event_time);
        assert_eq!(story.driver().ui().session().view().history.len(), 1);
        settle(&mut story, event_time + 2_999_999);
        assert_eq!(story.driver().ui().session().view().history.len(), 1);
        settle(&mut story, event_time + 3_000_000);
        assert_eq!(
            story.driver().ui().session().view().history.len(),
            2,
            "timed={timed}, logical={}, wake={:?}",
            story.logical_ns,
            story.wake_deadline(reel_core::clock::MonoTime(event_time + 3_000_000))
        );
    }
}

#[test]
fn maintenance_progress_keeps_the_clock_turn_pending_for_ready_script() {
    let mut story = runtime(
        r#"
        fn main() {
            let timer=clock();
            stage.replace(text("pending",x:bind(timer.elapsed)));
            let gate=completion();
            for i in 0..130 { spawn(|| { await gate; }); }
            await window.configure(fullscreen:true);
            narrator("done");
        }
    "#,
    );
    story.set_window_control_enabled(true);
    story
        .driver_mut()
        .unwrap()
        .adapter_mut()
        .set_window_info(std::sync::Arc::new(reel_core::window::WindowInfo {
            capabilities: reel_core::window::WindowCapabilities {
                fullscreen: true,
                ..Default::default()
            },
            ..Default::default()
        }));
    for _ in 0..20_000 {
        if !story.needs_work() && !story.driver().has_runnable_tasks() {
            break;
        }
        story.wake_ready();
        story.tick(SliceBudget::default(), || 0).unwrap();
    }
    assert!(!story.driver().has_runnable_tasks());
    let request = story
        .driver_mut()
        .unwrap()
        .adapter_mut()
        .take_window_request(true)
        .unwrap()
        .unwrap();
    story
        .driver_mut()
        .unwrap()
        .adapter_mut()
        .complete_window_request(
            request.id,
            Ok(reel_engine::aside::window::WindowReply::Configured(
                std::sync::Arc::new(Default::default()),
            )),
        )
        .unwrap();
    story.wake_input();
    story.advance_clock(16_000_000).unwrap();
    assert!(story.driver().adapter().stage_ready());
    let before = story.driver().adapter().vm().stats().slices;
    story.tick(SliceBudget::default(), || 16_000_000).unwrap();
    assert_eq!(
        story.driver().adapter().vm().stats().slices,
        before,
        "bounded wait maintenance precedes VM execution"
    );
    assert!(story.driver().has_runnable_tasks());
    assert!(
        story.clock_turn_pending,
        "maintenance without a VM slice cannot release the next clock batch"
    );
    settle(&mut story, 16_000_000);
    assert!(matches!(story.driver().ui().session().view().interaction,
        Some(reel_engine::InteractionView::Dialogue(dialogue)) if dialogue.text()=="done"));
}

#[test]
fn budgeted_reactive_continuations_keep_one_sample_and_publish_the_whole_batch() {
    use reel_core::clock::MonoTime;
    use std::cell::Cell;
    let mut story = runtime(
        r#"
        use std::math::curve;
        fn main() {
            let timer = clock(interval: 1.0 / 60);
            let motion = curve([{time: 0.0, value: 0.0}, {time: 10.0, value: 10.0}]);
            let root = stack();
            for i in 0..128 { root.add(stack(x: derive(|| motion.sample(timer.elapsed)))); }
            stage.replace(root);
            narrator("waiting");
        }
    "#,
    );
    settle(&mut story, 0);
    let before = story.driver().adapter().vm().stats();
    // Charge deterministic CPU time to each budget check. The host still regains
    // control after each 2 ms slice and publishes only a complete reactive batch.
    let now = Cell::new(16_666_667u64);
    let mut sample = None;
    let mut delayed = 0;
    let mut wait_ns = 0;
    let mut turns = 0;
    loop {
        assert!(turns < 1000, "reactive clock batch failed to settle");
        turns += 1;
        story
            .tick(
                SliceBudget {
                    deadline_ns: Some(now.get() + 2_000_000),
                    ..Default::default()
                },
                || {
                    let value = now.get();
                    now.set(value + 10_000);
                    value
                },
            )
            .unwrap();
        assert_eq!(*sample.get_or_insert(story.logical_ns), story.logical_ns);
        if !story.needs_work() {
            break;
        }
        let deadline = story.wake_deadline(MonoTime(now.get())).unwrap();
        let delay = deadline.0.saturating_sub(now.get());
        delayed += usize::from(delay != 0);
        wait_ns += delay;
        now.set(now.get() + delay);
        if story.driver().adapter().stage_ready() {
            let stage = story.driver().adapter().stage();
            for &child in &stage.node(stage.root().unwrap()).unwrap().children {
                assert!((stage.node(child).unwrap().props.x - 1.0 / 60.0).abs() < 1e-9);
            }
        }
    }
    assert!(turns > 1, "exercise multiple bounded host turns");
    assert_eq!(
        wait_ns, 0,
        "ready continuations do not sleep between host turns"
    );
    assert!(story.driver().adapter().stage_ready());
    let stage = story.driver().adapter().stage();
    let root = stage.node(stage.root().unwrap()).unwrap();
    assert_eq!(root.children.len(), 128);
    for &child in &root.children {
        assert!((stage.node(child).unwrap().props.x - 1.0 / 60.0).abs() < 1e-9);
    }
    let stats = story.driver().adapter().vm().stats();
    println!(
        "reactive_schedule nodes=128 host_turns={turns} delayed_turns={delayed} wait_ns={wait_ns} vm_slices={} native_entries={} sample_ns={}",
        stats.slices - before.slices,
        stats.native_entries - before.native_entries,
        sample.unwrap()
    );
}

#[test]
fn ready_collection_wakes_immediately_but_external_waits_and_future_clocks_sleep() {
    use reel_core::clock::MonoTime;
    for code in [
        "fn main() { await completion(); }",
        "fn main() { let timer=clock(interval:1); await completion(); }",
    ] {
        let mut story = runtime(code);
        settle(&mut story, 0);
        let future = story.wake_deadline(MonoTime(100));
        assert!(future.is_none_or(|deadline| deadline.0 > 100));
        story.driver_mut().unwrap().begin_collection().unwrap();
        story.collecting = true;
        assert_eq!(story.wake_deadline(MonoTime(100)), Some(MonoTime(100)));
        settle(&mut story, 100);
        assert_eq!(story.wake_deadline(MonoTime(100)), future);
        story.driver_mut().unwrap().begin_collection().unwrap();
        story.collecting = true;
        story.suspend(100).unwrap();
        assert_eq!(story.wake_deadline(MonoTime(100)), None);
    }
}
