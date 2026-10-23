//! Input-boundary reuse across finite presentation phases and repeated waits.

use super::{RunEnvironment, RunOutcome, explore_with_environment};
use crate::testing::{ExplorationReport, TestStatus};
use anyhow::Result;
use reel_engine::aside::source::CompiledStory;

fn check(source: &str) -> Result<ExplorationReport> {
    let source = reel_engine::aside::source::compile_story(source, "boundary_test", 1, "main")?;
    let limits = aside_vm::bytecode::Limits::default();
    let bytes = aside_vm::bytecode::encode(source.program.package(), limits)?;
    let asb = CompiledStory {
        program: aside_vm::bytecode::VerifiedProgram::decode(&bytes, limits)?,
        initializer: source.initializer,
        entry: source.entry,
        catalogs: Default::default(),
    };
    let resources = tempfile::tempdir()?;
    for (source, destination) in [
        ("panels.png", "first.png"),
        ("mask-waves.png", "second.png"),
    ] {
        std::fs::copy(
            crate::root().join("examples/assets").join(source),
            resources.path().join(destination),
        )?;
    }
    explore_with_environment(
        source,
        asb,
        resources.path(),
        &RunEnvironment {
            seed: 17,
            unix_seconds: 42,
        },
    )
}

fn assert_reaches_completion(report: &ExplorationReport) {
    assert!(
        report.runs.iter().any(|run| {
            run.source.outcome == RunOutcome::Finished && run.asb.outcome == RunOutcome::Finished
        }),
        "{report:?}"
    );
    assert!(report.runs.iter().all(|run| {
        run.replay_mismatch.is_none()
            && run.source.outcome == run.asb.outcome
            && matches!(
                run.source.outcome,
                RunOutcome::Finished | RunOutcome::UiCycle
            )
    }));
}

#[test]
fn a_persistent_input_survives_finite_image_and_animation_phases() -> Result<()> {
    let report = check(
        r#"
        use reel::types::{Node,Scope};
        fn hold(panel:Node, state) {
            scope(owner:panel,body:|phase:Scope| {
                state.advance = || phase.finish();
                await run(delay(0.04));
            });
            state.advance = null;
        }
        fn main() {
            game.configure(width:64,height:64);
            let state = {advance:null};
            let panel = input(width:64,height:64,modal:true,actions:{
                next:{bindings:["Enter"],on_release:|| {
                    let advance = state.advance;
                    state.advance = null;
                    if advance != null { advance(); }
                }}
            });
            let picture = panel.add(image("first.png",width:64,height:64,opacity:0));
            stage.replace(panel);
            await stage.presented(owner:panel);
            await run(tween(picture,opacity:1,duration:0.03));
            hold(panel,state);
            await run(tween(picture,opacity:0,duration:0.03));
            picture.source = "second.png";
            await stage.presented(owner:panel);
            await run(tween(picture,opacity:1,duration:0.03));
            hold(panel,state);
            game.exit();
        }
        "#,
    )?;
    assert_reaches_completion(&report);
    Ok(())
}

#[test]
fn the_same_image_and_wait_helper_keep_distinct_caller_phases() -> Result<()> {
    let report = check(
        r#"
        fn hold() { await run(delay(0.04)); }
        fn main() {
            game.configure(width:64,height:64);
            stage.replace(input(width:64,height:64,actions:{
                next:{bindings:["Enter"],on_release:|| {}}
            }) {
                image("first.png",width:64,height:64);
            });
            await stage.presented();
            hold();
            hold();
            game.exit();
        }
        "#,
    )?;
    assert_reaches_completion(&report);
    Ok(())
}

#[test]
fn a_loop_at_the_same_wait_reuses_its_input_observation() -> Result<()> {
    let report = check(
        r#"
        fn main() {
            game.configure(width:64,height:64);
            stage.replace(input(width:64,height:64,actions:{
                next:{bindings:["Enter"],on_release:|| {}}
            }) {
                image("first.png",width:64,height:64);
            });
            await stage.presented();
            loop { await run(delay(0.04)); }
        }
        "#,
    )?;
    assert_eq!(report.status, TestStatus::Incomplete, "{report:?}");
    assert!(!report.runs.is_empty());
    assert!(report.runs.iter().all(|run| {
        run.source.outcome == RunOutcome::UiCycle
            && run.asb.outcome == RunOutcome::UiCycle
            && run.replay_mismatch.is_none()
            && run.source.actions.len() <= 2
    }));
    Ok(())
}

#[test]
fn restored_button_focus_keeps_ownership_and_existing_focus_waits() -> Result<()> {
    let report = check(
        r#"
        fn main() {
            game.configure(width:64,height:64);
            let state = model({phase:0,entries:0,focused:false,lost:false,hover_lost:false});
            let first = button("First",width:64,height:32);
            first.on_interaction = |event| {
                if event.focused && !state.focused { state.entries += 1; }
                state.focused = event.focused;
            };
            fn finish(check_hover:bool) {
                if !first.focused || state.entries != 1 || state.lost {
                    throw "checkpoint replayed focus feedback or completed the focus-loss wait";
                }
                if check_hover && (!first.hover || state.hover_lost) {
                    throw "checkpoint completed the existing hover-loss wait";
                }
                game.exit();
            }
            first.on_click = || {
                if state.phase == 0 {
                    state.phase = 1;
                    spawn(|| {
                        await when(|| !first.focused);
                        state.lost = true;
                    },owner:first);
                    spawn(|| {
                        await when(|| !first.hover);
                        state.hover_lost = true;
                    },owner:first);
                } else { finish(true); }
            };
            stage.replace(input(width:64,height:64,actions:{
                back:{bindings:["Space"],on_release:|| {}}
            }) {
                column(width:64,height:64) {
                    button("Sibling",width:64,height:32,focus_on_pointer:false,
                        enabled:derive(|| state.phase == 1),on_click:|| finish(false));
                    first;
                }
            });
        }
        "#,
    )?;
    assert_reaches_completion(&report);
    assert!(
        report.runs.iter().any(|run| {
            run.source.outcome == RunOutcome::Finished
                && run.source.actions.len() == 2
                && run.source.actions.iter().last().unwrap().selected == 1
        }),
        "the focused checkpoint sibling must execute: {report:?}"
    );
    Ok(())
}
