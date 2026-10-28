//! Host continuation wakes preserve producer cadence; actual input samples time.
use super::*;
use reel_engine::{Epoch, InputAction, Session, SessionId, aside::VmLimits};

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
