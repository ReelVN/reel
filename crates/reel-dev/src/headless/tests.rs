use super::*;

fn execute(source: &str, options: &RunOptions, prefix: &[ActionSelection]) -> RunReport {
    let story = reel_engine::aside::source::compile_story(source, "scenario", 1, "main")
        .expect("compile the actual scenario");
    let resources = tempfile::tempdir().unwrap();
    run(story, resources.path(), options, prefix).unwrap()
}

#[test]
fn dialogue_and_actions_follow_the_real_session_and_strict_trace() {
    let source = r#"
        fn main() {
            stage.replace(stack());
            narrator("Read this first.");
            let result = choose([{key:"left",label:"Left"},{key:"right",label:"Right"}]);
            if result != "right" { throw "wrong route"; }
            narrator("Reached the selected route.");
        }
    "#;
    let trace = vec![ActionSelection {
        kind: ActionKind::Choice,
        option_keys: vec!["left".into(), "right".into()],
        selected: 1,
    }];
    let options = RunOptions {
        strict_replay: true,
        ..Default::default()
    };
    let report = execute(source, &options, &trace);
    assert_eq!(
        report.outcome,
        RunOutcome::Finished,
        "{:?}",
        report.diagnostic
    );
    assert_eq!(report.actions, trace);
    assert!(report.accepted_frames >= 3);
    let changed = execute(
        source,
        &options,
        &[ActionSelection {
            kind: ActionKind::Choice,
            option_keys: vec!["wrong".into()],
            selected: 0,
        }],
    );
    assert_eq!(changed.outcome, RunOutcome::ReplayMismatch);
    assert!(changed.actions.is_empty());
}

#[test]
fn uncaught_callback_or_story_fault_retains_source_stack() {
    let report = execute(
        "fn fail() { throw \"broken flow\"; } fn main() { fail(); }",
        &RunOptions::default(),
        &[],
    );
    assert_eq!(report.outcome, RunOutcome::RuntimeFailure);
    let diagnostic = report.diagnostic.unwrap();
    assert!(diagnostic.message.contains("broken flow"));
    assert!(diagnostic.stack.iter().any(|frame| frame.function == "fail"
        && frame.path.as_deref() == Some("scenario")
        && frame.line == Some(1)));
    assert!(
        diagnostic
            .stack
            .iter()
            .any(|frame| frame.function == "main")
    );
}

#[test]
fn unused_replay_suffix_never_passes() {
    let report = execute(
        "fn main() {}",
        &RunOptions::default(),
        &[ActionSelection {
            kind: ActionKind::Choice,
            option_keys: vec!["unused".into()],
            selected: 0,
        }],
    );
    assert_eq!(report.outcome, RunOutcome::ReplayMismatch);
}

#[test]
fn a_missing_final_stage_resource_is_a_failure() {
    let report = execute(
        "fn main() { stage.replace(image(\"missing.png\")); }",
        &RunOptions::default(),
        &[],
    );
    assert_eq!(
        report.outcome,
        RunOutcome::ResourceFailure,
        "{:?}",
        report.diagnostic
    );
    assert!(report.diagnostic.unwrap().message.contains("missing.png"));
}

#[test]
fn frame_capture_marks_the_unavailable_host_boundary_incomplete() {
    let report = execute(
        "fn main() { stage.replace(stack()); await stage.capture(width:16, height:16); }",
        &RunOptions::default(),
        &[],
    );
    assert_eq!(report.outcome, RunOutcome::Blocked, "{report:?}");
    assert!(report.diagnostic.unwrap().message.contains("stage.capture"));
}

#[test]
fn every_run_has_fresh_persistent_storage() {
    let source = r#"
        fn main() {
            let old = await persistent.read("test", default:null);
            if old != null { throw "storage leaked across runs"; }
            await persistent.write("test", 42);
            if await persistent.read("test") != 42 { throw "persistent round trip failed"; }
        }
    "#;
    for _ in 0..2 {
        let report = execute(source, &RunOptions::default(), &[]);
        assert_eq!(
            report.outcome,
            RunOutcome::Finished,
            "{:?}",
            report.diagnostic
        );
    }
}

#[test]
fn a_long_delay_advances_the_virtual_clock_without_waiting() {
    let report = execute(
        "fn main() { await run(delay(3600)); }",
        &RunOptions::default(),
        &[],
    );
    assert_eq!(
        report.outcome,
        RunOutcome::Finished,
        "{:?}",
        report.diagnostic
    );
    assert!(
        report.elapsed < Duration::from_secs(2),
        "{:?}",
        report.elapsed
    );
    assert!(report.steps < 100, "{}", report.steps);
}

#[test]
fn title_button_uses_pointer_callbacks_and_starts_a_new_session() {
    let source = r#"
        fn story() {
            stage.replace(stack());
            narrator("The actual story started.");
            let result = choose([{key:"done",label:"Done"}]);
            if result != "done" { throw "choice callback missing"; }
        }
        fn main() {
            game.configure(width:64,height:64);
            stage.replace(button(width:64,height:64,on_click:|| game.start(story)));
        }
    "#;
    let report = execute(source, &RunOptions::default(), &[]);
    assert_eq!(
        report.outcome,
        RunOutcome::Finished,
        "{:?}",
        report.diagnostic
    );
    assert_eq!(report.actions.len(), 2);
    assert_eq!(report.actions[0].kind, ActionKind::Controls);
    assert_eq!(report.actions[1].kind, ActionKind::Choice);
}

#[test]
fn a_looping_title_animation_keeps_its_start_button_actionable() {
    let source = r#"
        fn story() { stage.replace(stack()); narrator("Started."); }
        fn main() {
            game.configure(width:64,height:64);
            let animated = stack(width:64,height:64) {
                button(width:64,height:64,on_click:|| game.start(story));
            };
            stage.replace(animated);
            run(repeat(sequence(
                tween(animated, opacity:0.5, duration:1),
                tween(animated, opacity:1, duration:1)
            )));
        }
    "#;
    let report = execute(source, &RunOptions::default(), &[]);
    assert_eq!(
        report.outcome,
        RunOutcome::Finished,
        "{:?}",
        report.diagnostic
    );
    assert_eq!(report.actions[0].kind, ActionKind::Controls);
}

#[test]
fn configured_fonts_and_active_audio_use_the_production_resource_path() {
    let resources = tempfile::tempdir().unwrap();
    std::fs::copy(
        crate::root().join("examples/assets/chime.wav"),
        resources.path().join("chime.wav"),
    )
    .unwrap();
    std::fs::copy(
        crate::root()
            .join("examples/author_arcade/resources/fonts/AtkinsonHyperlegible-Regular.ttf"),
        resources.path().join("body.ttf"),
    )
    .unwrap();
    let story = reel_engine::aside::source::compile_story(
        r#"
        fn main() {
            game.configure(fonts:["body.ttf"]);
            stage.replace(text("A real font and sound."));
            await audio.play("chime.wav");
        }
    "#,
        "scenario",
        1,
        "main",
    )
    .unwrap();
    let report = run(story, resources.path(), &RunOptions::default(), &[]).unwrap();
    assert_eq!(
        report.outcome,
        RunOutcome::Finished,
        "{:?}",
        report.diagnostic
    );
    assert!(report.accepted_frames > 0);
    assert!(report.audio_frames > 0);
    assert!(report.audio_peak > 0.01, "{}", report.audio_peak);
}

#[test]
fn unseeded_streams_and_civil_time_replay_from_the_host_environment() {
    let source = r#"
        use std::time::now;
        fn next() {
            stage.replace(stack());
            if now() != 1234567890 { throw "new session lost civil time"; }
            let stream = random();
            choose([{key:f"next-{stream.int(0,1000000)}",label:"Next"}]);
        }
        fn main() {
            if now() != 1234567890 { throw "wrong civil time"; }
            let first = random();
            let second = random();
            let a = first.int(0,1000000);
            let b = second.int(0,1000000);
            if a == b { throw "independent streams reused a seed"; }
            stage.replace(stack());
            choose([{key:f"{a}-{b}",label:"Start"}]);
            game.start(next);
        }
    "#;
    let environment = RunEnvironment {
        seed: 42,
        unix_seconds: 1234567890,
    };
    let recorded = execute(
        source,
        &RunOptions {
            strict_replay: false,
            environment: environment.clone(),
        },
        &[],
    );
    assert_eq!(
        recorded.outcome,
        RunOutcome::Finished,
        "{:?}",
        recorded.diagnostic
    );
    assert_eq!(recorded.actions.len(), 2);
    let replayed = execute(
        source,
        &RunOptions {
            strict_replay: true,
            environment,
        },
        &recorded.actions,
    );
    assert_eq!(
        replayed.outcome,
        RunOutcome::Finished,
        "{:?}",
        replayed.diagnostic
    );
    assert_eq!(replayed.actions, recorded.actions);
}
