//! Execute self-checking project entries with real CPU presentation and isolated storage.
#[path = "support/author.rs"]
#[allow(dead_code)]
mod support;

use reel_core::{budget::QueueLimits, wake::WakeSignal};
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{SliceBudget, VmLimits},
};
use reel_player::{
    aside_session::{AsideSessionUi, StoryState},
    assets::AssetOptions,
    saves::{DirectorySaves, RuntimeSaves, SaveService},
    story_runtime::StoryRuntime,
};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

fn execute_project(path: &Path) {
    let loaded =
        reel_cli::project::load(path).unwrap_or_else(|error| panic!("{:?}", error.failure));
    let checked = loaded.check().unwrap();
    let entry = loaded.entry(None);
    let artifact = reel_cli::build(&checked, Some(&entry)).unwrap();
    for (kind, story) in [
        ("source", reel_cli::into_story(checked, &entry).unwrap()),
        ("ASB", reel_cli::decode_story(&artifact.bytes).unwrap()),
    ] {
        let saves = tempfile::tempdir().unwrap();
        let driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap()
        .with_asset_options(AssetOptions {
            root: loaded.root.clone(),
            ..Default::default()
        });
        let storage = SaveService::new(
            Arc::new(DirectorySaves::new(saves.path(), 256 << 20)),
            QueueLimits {
                max_requests: 16,
                max_bytes: 256 << 20,
            },
            WakeSignal::new(|| {}),
        );
        let mut runtime =
            StoryRuntime::new(driver).with_saves(RuntimeSaves::new(storage, 256 << 20));
        let (mut bridge, mut root) = support::bridge(runtime.driver());
        let mut identity = runtime.driver().ui().session().identity();
        let mut replacements = 0;
        let started = Instant::now();
        let mut frame = 0;
        loop {
            frame += 1;
            // Match the window host: native continuations share one frame's CPU
            // deadline. A budget slice must not advance game time by one frame.
            let deadline = started.elapsed().as_nanos() as u64 + 2_000_000;
            let progress = runtime
                .tick(
                    SliceBudget {
                        deadline_ns: Some(deadline),
                        ..Default::default()
                    },
                    || started.elapsed().as_nanos() as u64,
                )
                .unwrap();
            assert!(
                started.elapsed() < Duration::from_secs(90),
                "{kind} scenario did not finish: {}; title={:?}, frame={frame}, progress={progress:?}, state={:?}, capture={}, tasks={}, activities={}, presentation={:?}",
                path.display(),
                runtime.driver().adapter().game_settings().title,
                runtime.driver().state(),
                runtime.capturing(),
                runtime.driver().has_runnable_tasks(),
                runtime.driver().activities_pending(),
                runtime.driver().projection_stamp()
            );
            assert!(
                matches!(
                    runtime.driver().state(),
                    StoryState::Running | StoryState::Finished
                ),
                "{kind}: {:?}; fault={:?}; host={:?}",
                runtime.driver().state(),
                runtime.driver().fault(),
                runtime.driver().host_failure()
            );
            if runtime.driver().state() == StoryState::Finished && !runtime.needs_tick() {
                break;
            }
            // Mutable access would interrupt capture. Let the actual save worker and
            // restore protocol finish before accepting another CPU frame.
            if !runtime.capturing() {
                let next = runtime.driver().ui().session().identity();
                if next != identity {
                    (bridge, root) = support::bridge(runtime.driver());
                    identity = next;
                    replacements += 1;
                }
                support::present(runtime.driver_mut().unwrap(), &mut bridge, root, frame);
                runtime.wake_input();
            }
            // Give filesystem/resource workers CPU time between presentation attempts.
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!runtime.driver().cleanup_pending());
        eprintln!(
            "{kind}: {} completed; frames={frame}, session_replacements={replacements}",
            path.display()
        );
    }
}

#[test]
fn project_scenario_executes_assertions_across_save_restore_and_new_session() {
    let project = tempfile::tempdir().unwrap();
    let source = project.path().join("main.aside");
    std::fs::write(
        &source,
        r#"
        fn main() {
            stage.replace(stack(width: 320, height: 180));
            await stage.presented();
            let save = saves.save("checkpoint");
            await save;
            if !save.restored {
                await persistent.write("progress", 42);
                await saves.load("checkpoint");
                throw "load returned to the old session";
            }
            if await persistent.read("progress") != 42 { throw "persistent rewind"; }
            game.start(finish);
            throw "start returned to the old session";
        }
        fn finish() {
            if await persistent.read("progress") != 42 { throw "new session lost storage"; }
            await saves.delete("checkpoint");
            if (await saves.list()).len() != 0 { throw "slot not deleted"; }
            stage.replace(stack());
            await stage.presented();
        }
    "#,
    )
    .unwrap();
    execute_project(&source);
}

#[test]
#[ignore = "requires REEL_SCENARIO pointing to a self-checking project entry"]
fn external_project_scenario() {
    let path = std::env::var_os("REEL_SCENARIO").expect("set REEL_SCENARIO to an Aside entry file");
    execute_project(Path::new(&path));
}
