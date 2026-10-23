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
        option_keys: vec!["left".into(), "right".into()].into(),
        input_state: None,
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
    assert_eq!(report.actions.iter().cloned().collect::<Vec<_>>(), trace);
    assert!(report.accepted_frames >= 3);
    let changed = execute(
        source,
        &options,
        &[ActionSelection {
            kind: ActionKind::Choice,
            option_keys: vec!["wrong".into()].into(),
            input_state: None,
            selected: 0,
        }],
    );
    assert_eq!(changed.outcome, RunOutcome::ReplayMismatch);
    assert!(changed.actions.is_empty());
}

#[test]
fn open_dialogue_keeps_timed_script_continuations_running() {
    let report = execute(
        r#"
        fn main() {
            stage.replace(stack());
            let speech = narrator("An open paragraph.", wait: false);
            await run(delay(0.25));
            speech.wait();
            narrator("The timed continuation completed.");
        }
        "#,
        &RunOptions::default(),
        &[],
    );
    assert_eq!(report.outcome, RunOutcome::Finished, "{report:?}");
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
            option_keys: vec!["unused".into()].into(),
            input_state: None,
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
fn invalid_capture_dimensions_fail_before_requesting_a_backend() {
    let report = execute(
        "fn main() { stage.replace(stack()); await stage.capture(width:0, height:16); }",
        &RunOptions::default(),
        &[],
    );
    assert_eq!(report.outcome, RunOutcome::RuntimeFailure, "{report:?}");
    assert!(
        report
            .diagnostic
            .unwrap()
            .message
            .contains("capture dimensions")
    );
}

fn prepared_runner(source: &str) -> (Runner, tempfile::TempDir) {
    let story = reel_engine::aside::source::compile_story(source, "scenario", 1, "main")
        .expect("compile input scenario");
    let resources = tempfile::tempdir().unwrap();
    let runner =
        Runner::new_with_environment(story, resources.path(), &RunEnvironment::default()).unwrap();
    (runner, resources)
}

#[test]
fn an_authored_pointer_input_scope_advances_a_completion_gate() {
    let report = execute(
        r#"
        fn main() {
            let advanced = completion();
            stage.replace(input(width:256,height:96,actions:{
                advance:{bindings:["PointerPrimary"],on_press:|| advanced.resolve()}
            }));
            await advanced;
            game.exit();
        }
        "#,
        &RunOptions::default(),
        &[],
    );
    assert_eq!(report.outcome, RunOutcome::Finished, "{report:?}");
    assert_eq!(report.actions.len(), 1);
    assert!(report.actions[0].option_keys[0].contains(":input:advance:"));
}

#[test]
fn authored_input_bindings_use_keyboard_gamepad_axis_and_pointer_events() {
    for binding in [
        "Enter",
        "NumpadEnter",
        "Space",
        "ControlLeft",
        "KeyZ",
        "South",
        "LeftX",
        "PointerSecondary",
    ] {
        let source = format!(
            r#"
            fn main() {{
                let advanced = completion();
                stage.replace(input(width:256,height:96,actions:{{
                    advance:{{bindings:["{binding}"],on_release:|| advanced.resolve()}}
                }}));
                await advanced;
                game.exit();
            }}
        "#
        );
        let report = execute(&source, &RunOptions::default(), &[]);
        assert_eq!(
            report.outcome,
            RunOutcome::Finished,
            "{binding}: {report:?}"
        );
        assert_eq!(report.actions.len(), 1);
    }
}

#[test]
fn input_candidates_follow_the_existing_keyboard_owner() {
    let (mut runner, _resources) = prepared_runner(
        r#"
        fn main() {
            let advanced = completion();
            stage.replace(input(width:256,height:96,actions:{
                outer:{bindings:["Enter"],on_release:|| {throw "wrong input owner";}}
            }) {
                input(width:256,height:96,actions:{
                    inner:{bindings:["Enter"],on_release:|| advanced.resolve()}
                });
            });
            await advanced;
            game.exit();
        }
    "#,
    );
    let decision = runner.drive().unwrap().unwrap();
    assert_eq!(decision.option_keys.len(), 1);
    assert!(decision.option_keys[0].starts_with("0/0:input:inner:"));
    runner.apply(&decision, 0).unwrap();
    assert!(runner.drive().unwrap().is_none());
    runner.close().unwrap();
}

#[test]
fn focused_buttons_keep_enter_while_unrelated_bindings_reach_the_parent() {
    let (mut runner, _resources) = prepared_runner(
        r#"
        fn main() {
            let advanced = completion();
            stage.replace(input(width:256,height:96,actions:{
                enter:{bindings:["Enter"],on_release:|| {throw "focused button bypassed";}},
                escape:{bindings:["Escape"],on_release:|| advanced.resolve()}
            }) {
                button("Focus",width:256,height:96,on_click:||{});
            });
            await advanced;
            game.exit();
        }
    "#,
    );
    let first = runner.drive().unwrap().unwrap();
    let focus = first
        .option_keys
        .iter()
        .position(|key| key.ends_with(":button:Focus"))
        .unwrap();
    runner.apply(&first, focus).unwrap();
    let focused = runner.drive().unwrap().unwrap();
    assert!(
        focused
            .option_keys
            .iter()
            .all(|key| !key.contains(":input:enter:"))
    );
    let escape = focused
        .option_keys
        .iter()
        .position(|key| key.contains(":input:escape:"))
        .unwrap();
    runner.apply(&focused, escape).unwrap();
    assert!(runner.drive().unwrap().is_none());
    runner.close().unwrap();
}

#[test]
fn an_open_completed_paragraph_keeps_its_authored_input_gate_actionable() {
    let source = r#"
        fn main() {
            let advanced = completion();
            stage.replace(input(width:256,height:96,actions:{
                advance:{bindings:["Enter"],on_release:|| advanced.resolve()}
            }));
            narrator("The paragraph remains open while its illustrated card waits.",wait:false,reveal_skippable:SKIPPABLE);
            await advanced;
            game.exit();
        }
    "#;
    for skippable in [true, false] {
        let report = execute(
            &source.replace("SKIPPABLE", if skippable { "true" } else { "false" }),
            &RunOptions::default(),
            &[],
        );
        assert_eq!(report.outcome, RunOutcome::Finished, "{report:?}");
        assert_eq!(report.actions.len(), 1);
        assert!(report.actions[0].option_keys[0].starts_with("0:input:advance:"));
    }
}

#[test]
fn a_filled_single_line_editor_has_an_explicit_submit_action() {
    let (mut runner, _resources) = prepared_runner(
        r#"
        fn main() {
            game.configure(width:256,height:96);
            stage.replace(text_input(width:256,height:64,on_submit:|event| {
                if event.value != "Player" { throw "wrong submitted value"; }
                game.exit();
            }));
        }
        "#,
    );
    let empty = runner.drive().unwrap().unwrap();
    assert_eq!(empty.option_keys.as_ref(), ["0:text:empty"]);
    runner.apply(&empty, 0).unwrap();
    let filled = runner.drive().unwrap().unwrap();
    assert_eq!(filled.option_keys.as_ref(), ["0:text:submit"]);
    let checkpoint = runner.runtime.checkpoint().unwrap();
    runner
        .runtime
        .restore_checkpoint(checkpoint, runner.logical_ns, || {})
        .unwrap();
    runner.reset_presentation().unwrap();
    let restored = runner.restored_decision().unwrap().unwrap();
    assert_eq!(filled.option_keys, restored.option_keys);
    runner.apply(&restored, 0).unwrap();
    assert!(runner.drive().unwrap().is_none());
    runner.close().unwrap();
}

#[test]
fn filling_an_editor_can_replace_the_page_before_submission() {
    let (mut runner, _resources) = prepared_runner(
        r#"
        fn main() {
            game.configure(width:256,height:96);
            stage.replace(text_input(width:256,height:64,
                on_change:|| stage.replace(button("Next",width:256,height:64,on_click:||game.exit())),
                on_submit:|| { throw "retired editor submitted"; }));
        }
        "#,
    );
    let empty = runner.drive().unwrap().unwrap();
    runner.apply(&empty, 0).unwrap();
    let next = runner.drive().unwrap().unwrap();
    assert_eq!(next.option_keys.as_ref(), ["0:button:Next"]);
    runner.apply(&next, 0).unwrap();
    assert!(runner.drive().unwrap().is_none());
    runner.close().unwrap();
}

#[test]
fn geometry_feedback_settles_before_an_input_checkpoint_is_exposed() {
    let (mut runner, _resources) = prepared_runner(
        r#"
        fn main() {
            game.configure(width:256,height:96);
            let control=button(width:256,height:96,on_click:||game.exit());
            stage.replace(control);
            spawn(|| {
                await stage.presented(owner:control);
                let geometry=await stage.geometry(control);
                control.width=geometry.width/2;
            });
        }
    "#,
    );
    let decision = runner.drive().unwrap().unwrap();
    assert!(runner.runtime.checkpoint_ready());
    let _checkpoint = runner.runtime.checkpoint().unwrap();
    let DecisionAction::Control { position, .. } = &decision.actions[0] else {
        panic!("expected the resized control")
    };
    assert_eq!(*position, [64., 48.]);
    runner.apply(&decision, 0).unwrap();
    assert!(runner.drive().unwrap().is_none());
    runner.close().unwrap();
}

#[test]
fn a_click_reacquires_the_control_after_hover_moves_it() {
    let (mut runner, _resources) = prepared_runner(
        r#"
        fn main() {
            game.configure(width:256,height:96);
            let state = model({shifted:false});
            let mut control = null;
            control = button(width:64,height:64,
                on_interaction: |event| {
                    if event.hover && !state.shifted {
                        state.shifted = true;
                        control.x = 128;
                    }
                },
                on_click: || game.exit());
            stage.replace(control);
        }
    "#,
    );
    let decision = runner.drive().unwrap().unwrap();
    runner.apply(&decision, 0).unwrap();
    assert!(
        runner.drive().unwrap().is_none(),
        "the moved control must receive release"
    );
    runner.close().unwrap();
}

#[test]
fn a_disappearing_pointer_target_does_not_activate_its_replacement() {
    let (mut runner, _resources) = prepared_runner(
        r#"
        fn main() {
            game.configure(width:64,height:64);
            let replacement = button("Next",width:64,height:64,visible:false,on_click:||game.exit());
            let mut first = null;
            first = button("First",width:64,height:64,
                on_interaction: |event| {
                    if event.hover {
                        first.visible = false;
                        replacement.visible = true;
                    }
                },
                on_click:||{throw "retired target clicked";});
            stage.replace(stack(children:[first,replacement]));
        }
    "#,
    );
    let first = runner.drive().unwrap().unwrap();
    runner.apply(&first, 0).unwrap();
    let next = runner.drive().unwrap().unwrap();
    assert!(next.option_keys[0].ends_with(":button:Next"));
    runner.apply(&next, 0).unwrap();
    assert!(runner.drive().unwrap().is_none());
    runner.close().unwrap();
}

#[test]
fn a_transient_control_and_waiting_for_its_entrance_both_reach_the_menu() {
    let source = r#"
        fn main() {
            game.configure(width:256,height:96);
            let transient = button("Light",width:64,height:64,on_click:||{});
            let start = button("Start",x:128,width:64,height:64,enabled:false,on_click:||game.exit());
            stage.replace(stack(children:[transient,start]));
            spawn(|| {
                await run(delay(1));
                transient.remove();
                start.enabled = true;
            });
        }
    "#;
    for wait_first in [false, true] {
        let (mut runner, _resources) = prepared_runner(source);
        let transient = runner.drive().unwrap().unwrap();
        assert_eq!(transient.option_keys[0], WAIT_FINITE);
        assert!(transient.option_keys[1].ends_with(":button:Light"));
        let checkpoint = runner.runtime.checkpoint().unwrap();
        runner
            .runtime
            .restore_checkpoint(checkpoint, runner.logical_ns, || {})
            .unwrap();
        runner.reset_presentation().unwrap();
        let restored = runner.restored_decision().unwrap().unwrap();
        assert_eq!(restored.option_keys, transient.option_keys);
        let selected = restored
            .option_keys
            .iter()
            .position(|key| (key == WAIT_FINITE) == wait_first)
            .unwrap();
        runner.apply(&restored, selected).unwrap();
        let menu = runner.drive().unwrap().unwrap();
        assert!(
            menu.option_keys[0].ends_with(":button:Start"),
            "{:?}",
            menu.option_keys
        );
        runner.apply(&menu, 0).unwrap();
        assert!(runner.drive().unwrap().is_none());
        runner.close().unwrap();
    }
}

#[test]
fn repeated_background_waits_do_not_keep_postponing_the_next_input() {
    let (mut runner, _resources) = prepared_runner(
        r#"
        fn main() {
            game.configure(width:64,height:64);
            stage.replace(button(width:64,height:64,on_click:||{}));
            spawn(|| { while true { await run(delay(1)); } });
        }
    "#,
    );
    let first = runner.drive().unwrap().unwrap();
    let clicked = first
        .option_keys
        .iter()
        .position(|key| key != WAIT_FINITE)
        .unwrap();
    runner.apply(&first, clicked).unwrap();
    let repeated = runner.drive().unwrap().unwrap();
    assert_eq!(first.option_keys, repeated.option_keys);
    assert_eq!(runner.logical_ns, 1_000_000_000);
    let wait = repeated
        .option_keys
        .iter()
        .position(|key| key == WAIT_FINITE)
        .unwrap();
    runner.apply(&repeated, wait).unwrap();
    let next = runner.drive().unwrap().unwrap();
    assert_eq!(next.option_keys, first.option_keys);
    assert_eq!(runner.logical_ns, 2_000_000_000);
    runner.close().unwrap();
}

#[test]
fn finite_wait_inspection_excludes_paused_repeats_and_clock_ticks() {
    let (mut runner, _resources) = prepared_runner(
        r#"
        fn main() {
            stage.replace(button(width:64,height:64,on_click:||{}));
            let paused = run(delay(10),paused:true);
            spawn(|| await paused);
            let forever = run(repeat(delay(1)));
            spawn(|| await forever);
            let ticker = clock(interval:1);
            spawn(|| ticker.next());
            spawn(|| await run(delay(2)));
        }
    "#,
    );
    runner.drive().unwrap().unwrap();
    let waits = runner
        .runtime
        .driver()
        .adapter()
        .finite_activity_waits()
        .unwrap();
    assert_eq!(waits.len(), 1);
    runner.close().unwrap();
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
fn dialogue_ready_indicator_and_paused_clock_do_not_starve_reveal_time() {
    let report = execute(
        r#"
        use reel::types::Scope;
        fn main() {
            let panel = stack();
            let dialogue = game.dialogue();
            let icon = stack(visible:derive(|| dialogue.active && dialogue.ready));
            panel.add(icon);
            stage.replace(panel);
            let timer = clock(interval:0.5,paused:true,owner:icon);
            spawn(|| {
                loop {
                    await when(|| icon.visible);
                    await stage.presented(owner:icon);
                    if icon.visible {
                        timer.resume();
                        await when(|| !icon.visible);
                        timer.pause();
                    }
                }
            },owner:icon);
            for index in 0 .. 56 {
                let speech = narrator("Contract dialogue.",wait:false);
                let row = game.dialogue().lines.top();
                scope(owner:panel,body:|line_scope: Scope| {
                    line_scope.spawn(|| { await when(|| row.advance_requested); });
                    speech.wait();
                });
            }
            game.exit();
        }
    "#,
        &RunOptions::default(),
        &[],
    );
    assert_eq!(report.outcome, RunOutcome::Finished, "{report:?}");
    assert!(report.accepted_frames >= 56);
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
        &recorded.actions.iter().cloned().collect::<Vec<_>>(),
    );
    assert_eq!(
        replayed.outcome,
        RunOutcome::Finished,
        "{:?}",
        replayed.diagnostic
    );
    assert_eq!(replayed.actions, recorded.actions);
}
