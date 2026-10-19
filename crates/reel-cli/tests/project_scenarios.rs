//! Execute self-checking projects with CPU presentation, offline audio and isolated storage.
#[path = "support/audio.rs"]
mod audio;
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

fn execute_project(path: &Path) -> Vec<audio::AudioStats> {
    let loaded =
        reel_cli::project::load(path).unwrap_or_else(|error| panic!("{:?}", error.failure));
    let checked = loaded.check().unwrap();
    let entry = loaded.entry(None);
    let artifact = reel_cli::build(&checked, Some(&entry)).unwrap();
    let mut audio_stats = Vec::new();
    for (kind, story) in [
        ("source", reel_cli::into_story(checked, &entry).unwrap()),
        ("ASB", reel_cli::decode_story(&artifact.bytes).unwrap()),
    ] {
        let saves = tempfile::tempdir().unwrap();
        let mut audio = audio::AudioClock::default();
        let media = audio.factory(loaded.root.clone());
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
        })
        .with_media(media())
        .unwrap();
        let storage = SaveService::new(
            Arc::new(DirectorySaves::new(saves.path(), 256 << 20)),
            QueueLimits {
                max_requests: 16,
                max_bytes: 256 << 20,
            },
            WakeSignal::new(|| {}),
        );
        let mut runtime = StoryRuntime::new(driver)
            .with_saves(RuntimeSaves::new(storage, 256 << 20))
            .with_media_factory(media);
        let (mut bridge, mut root) = support::bridge(runtime.driver());
        let mut identity = runtime.driver().ui().session().identity();
        let mut replacements = 0;
        let started = Instant::now();
        let mut frame = 0;
        loop {
            frame += 1;
            audio.advance(started.elapsed());
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
        audio_stats.push(audio.stats());
        eprintln!(
            "{kind}: {} completed; frames={frame}, session_replacements={replacements}, audio={:?}",
            path.display(),
            audio.stats()
        );
    }
    audio_stats
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
fn project_scenario_mixes_audio_queue_across_save_restore_and_new_session() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("chime.wav"),
        include_bytes!("../../../examples/assets/chime.wav"),
    )
    .unwrap();
    let source = project.path().join("main.aside");
    std::fs::write(&source, r#"
        fn main() {
            stage.replace(stack(width: 320, height: 180));
            await stage.presented();
            let sound = audio.play("chime.wav", group: "music", volume: 0.5,
                looping: true, loop_start: 0.02, loop_end: 0.2);
            while audio.position(sound) < 0.03 { await run(delay(0.005)); }
            pause(sound);
            await run(delay(0.02));
            audio.queue(sound, "chime.wav", start: 0.1);
            let position = audio.position(sound);
            if position <= 0 { throw "audio did not consume PCM"; }
            let save = saves.save("audio");
            await save;
            if !save.restored {
                audio.stop(sound);
                if await sound != "Stopped" { throw "stop did not settle"; }
                await saves.load("audio");
                throw "load returned to old audio";
            }
            await run(delay(0.02));
            if audio.position(sound) < position - 0.001 || audio.position(sound) > position + 0.001 {
                throw "restored paused position changed";
            }
            resume(sound);
            if await sound != "Completed" || await sound != "Completed" {
                throw "restored queue did not complete";
            }
            if audio.position(sound) < 0.19 { throw "queue tail did not play"; }
            game.start(finish);
        }
        fn finish() {
            let sound = audio.play("chime.wav");
            if await sound != "Completed" || audio.position(sound) < 0.19 {
                throw "new session lost media backend";
            }
        }
    "#).unwrap();
    for mixed in execute_project(&source) {
        assert!(mixed.frames > 0, "offline device did not consume frames");
        assert!(
            mixed.peak > 0.01,
            "decoded fixture produced no audible PCM: {mixed:?}"
        );
    }
}

#[test]
fn offline_audio_clock_preserves_fractional_time_and_releases_retired_outputs() {
    let mut audio = audio::AudioClock::default();
    let media = audio.factory(std::env::temp_dir());
    let player = media();
    audio.advance(Duration::from_nanos(10_417));
    assert_eq!(audio.stats().frames, 0);
    audio.advance(Duration::from_nanos(20_834));
    assert_eq!(audio.stats().frames, 1);
    for _ in 0..100 {
        audio.advance(Duration::from_nanos(20_834));
    }
    assert_eq!(audio.stats().frames, 1, "VM slices must not add audio time");
    audio.advance(Duration::from_secs(1));
    assert_eq!(audio.stats().frames, 48_000);
    drop(player);
    audio.advance(Duration::from_secs(2));
    assert_eq!(
        audio.stats().frames,
        48_000,
        "clock retained a retired output"
    );
}

#[test]
#[ignore = "requires REEL_SCENARIO pointing to a self-checking project entry"]
fn external_project_scenario() {
    let path = std::env::var_os("REEL_SCENARIO").expect("set REEL_SCENARIO to an Aside entry file");
    execute_project(Path::new(&path));
}
