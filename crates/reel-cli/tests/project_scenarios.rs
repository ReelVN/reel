//! Self-checking projects with CPU or opt-in GPU presentation, offline audio and isolated storage.
#[path = "support/audio.rs"]
mod audio;
#[path = "support/scenario_capture.rs"]
mod scenario_capture;
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

fn execute_project(path: &Path, timeout: Duration) -> Vec<audio::AudioStats> {
    execute_project_with_capture(
        path,
        timeout,
        std::env::var("REEL_SCENARIO_GPU_CAPTURE").is_ok_and(|value| value == "1"),
    )
    .0
}

fn execute_project_with_capture(
    path: &Path,
    timeout: Duration,
    gpu_capture: bool,
) -> (Vec<audio::AudioStats>, Vec<scenario_capture::CaptureStats>) {
    let loaded =
        reel_cli::project::load(path).unwrap_or_else(|error| panic!("{:?}", error.failure));
    let checked = loaded.check().unwrap();
    let entry = loaded.entry(None);
    let artifact = reel_cli::build(&checked, Some(&entry)).unwrap();
    let mut audio_stats = Vec::new();
    let mut capture_stats = Vec::new();
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
        let mut captures = gpu_capture.then(scenario_capture::ScenarioCapture::new);
        runtime.set_frame_capture_enabled(gpu_capture);
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
            runtime
                .tick(
                    SliceBudget {
                        deadline_ns: Some(deadline),
                        ..Default::default()
                    },
                    || started.elapsed().as_nanos() as u64,
                )
                .unwrap();
            assert!(
                started.elapsed() < timeout,
                "{kind} scenario did not finish within {timeout:?}: {}; title={:?}, frame={frame}, state={:?}, capture={}, tasks={}, activities={}, presentation={:?}",
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
            // restore protocol finish before accepting another frame.
            if !runtime.capturing() {
                let next = runtime.driver().ui().session().identity();
                if next != identity {
                    (bridge, root) = support::bridge(runtime.driver());
                    identity = next;
                    replacements += 1;
                    if let Some(captures) = &mut captures {
                        captures.new_session();
                    }
                }
                if let Some(captures) = &mut captures {
                    let driver = runtime.driver_mut().unwrap();
                    captures.drive(driver);
                    captures.present(driver, &mut bridge, root, frame);
                } else {
                    support::present(runtime.driver_mut().unwrap(), &mut bridge, root, frame);
                }
                runtime.wake_input();
            }
            // Give filesystem/resource workers CPU time between presentation attempts.
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!runtime.driver().cleanup_pending());
        audio_stats.push(audio.stats());
        if let Some(captures) = &captures {
            eprintln!("{kind} GPU captures: {:?}", captures.stats());
            capture_stats.push(captures.stats());
        }
        eprintln!(
            "{kind}: {} completed; frames={frame}, session_replacements={replacements}, audio={:?}",
            path.display(),
            audio.stats()
        );
    }
    (audio_stats, capture_stats)
}

#[test]
#[ignore = "requires a GPU adapter for real frame capture"]
fn project_scenario_gpu_capture_restores_png_images_and_particles_at_canvas_size() {
    let project = tempfile::tempdir().unwrap();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 255, 255]))
        .save(project.path().join("blue.png"))
        .unwrap();
    let source = project.path().join("main.aside");
    std::fs::write(
        &source,
        r##"
        fn main() {
            game.configure(width: 960, height: 540);
            let ink = particles("blue.png", x: 480, y: 270, seed: 7);
            stage.replace(stack(width: 960, height: 540, background: "#ff0000") {
                image("blue.png", x: 800, width: 160, height: 540);
                ink;
            });
            emit(ink, burst: 1, lifetime: 60, size: 32);
            await stage.presented();
            let png = await stage.capture();
            let checkpoint = saves.save("captured", thumbnail: png);
            await checkpoint;
            if !checkpoint.restored {
                await saves.load("captured");
                throw "capture restore returned to old session";
            }
            let thumbnail = await saves.thumbnail("captured");
            if thumbnail != png { throw "thumbnail bytes differ after restore"; }
            stage.replace(image(png, width: 960, height: 540));
            await stage.presented();
            await stage.capture();
        }
    "##,
    )
    .unwrap();
    let (_, stats) = execute_project_with_capture(&source, Duration::from_secs(90), true);
    assert_eq!(stats.len(), 2, "source and actual ASB must both render");
    for captured in stats {
        assert_eq!(captured.count, 2);
        assert_eq!(
            captured.size,
            [960, 540],
            "capture used the helper's old viewport"
        );
        assert!(
            captured.center[0] <= 3
                && captured.center[1] <= 3
                && captured.center[2] >= 252
                && captured.center[3] == 255,
            "restored PNG lost its particle: {:?}",
            captured.center
        );
        assert_eq!(
            captured.right,
            [0, 0, 255, 255],
            "restored PNG lost its source image"
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
    execute_project(&source, Duration::from_secs(90));
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
    for mixed in execute_project(&source, Duration::from_secs(90)) {
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
    let timeout = std::env::var_os("REEL_SCENARIO_TIMEOUT_SECS")
        .map(|value| {
            let seconds = value
                .to_str()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|&seconds| seconds > 0)
                .unwrap_or_else(|| {
                    panic!(
                        "REEL_SCENARIO_TIMEOUT_SECS must be a positive u64 integer, got {value:?}"
                    )
                });
            Duration::from_secs(seconds)
        })
        .unwrap_or(Duration::from_secs(90));
    execute_project(Path::new(&path), timeout);
}
