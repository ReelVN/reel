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
    Epoch, InteractionView, Session, SessionId,
    aside::{SliceBudget, VmLimits},
};
use reel_player::{
    aside_session::{AsideSessionUi, StoryState},
    assets::{AssetOptions, Assets},
    player::PlayerOptions,
    saves::{DirectorySaves, RuntimeSaves, SaveService, SaveStorage},
    story_runtime::StoryRuntime,
};
use std::{
    collections::VecDeque,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

fn scenario_project() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join("src")).unwrap();
    std::fs::create_dir(project.path().join("resources")).unwrap();
    project
}

#[test]
fn project_scenario_accepts_explicit_exit_after_persistent_publication() {
    let project = scenario_project();
    std::fs::write(project.path().join("src/main.aside"), r#"
fn main() {
    await persistent.write("exit-published", true);
    if await persistent.read("exit-published") != true { throw "persistent publication was not visible"; }
    game.exit();
}
"#).unwrap();
    execute_project(project.path(), Duration::from_secs(5));
}

fn execute_project(path: &Path, timeout: Duration) -> Vec<audio::AudioStats> {
    let clicks = scenario_clicks();
    execute_project_with_capture(
        path,
        timeout,
        std::env::var("REEL_SCENARIO_GPU_CAPTURE").is_ok_and(|value| value == "1"),
        &clicks,
        false,
    )
    .0
}

// JSON file: [[capture_ordinal, [canvas_x, canvas_y]], ...]. Coordinates use the
// logical canvas; the optional physical viewport uses the production Canvas mapping.
fn scenario_clicks() -> Vec<(u64, [f64; 2])> {
    let Some(path) = std::env::var_os("REEL_SCENARIO_CLICKS") else {
        return Vec::new();
    };
    let bytes = std::fs::read(&path).expect("read REEL_SCENARIO_CLICKS JSON file");
    let clicks: Vec<(u64, [f64; 2])> =
        serde_json::from_slice(&bytes).expect("REEL_SCENARIO_CLICKS expects [[capture,[x,y]],...]");
    let mut previous = 0;
    for &(capture, point) in &clicks {
        assert!(
            capture > previous,
            "click capture ordinals must be positive and strictly increasing"
        );
        assert!(
            point
                .into_iter()
                .all(|value| value.is_finite() && value >= 0.),
            "click coordinates must be finite and nonnegative"
        );
        previous = capture;
    }
    clicks
}

// CPU-only interaction requests from self-checking author projects. A marker
// consumes one accepted manual Advance; unrelated titles never inject input.
fn scenario_advances() -> Vec<String> {
    let Some(value) = std::env::var_os("REEL_SCENARIO_ADVANCES") else {
        return Vec::new();
    };
    let titles: Vec<String> =
        serde_json::from_str(value.to_str().expect("advance titles must be UTF-8"))
            .expect("REEL_SCENARIO_ADVANCES expects a JSON array of title strings");
    assert!(
        titles.iter().all(|title| !title.is_empty()),
        "advance title markers must be nonempty"
    );
    titles
}

fn execute_project_with_capture(
    path: &Path,
    timeout: Duration,
    gpu_capture: bool,
    clicks: &[(u64, [f64; 2])],
    first_capture_pressure: bool,
) -> (Vec<audio::AudioStats>, Vec<scenario_capture::CaptureStats>) {
    // Scale game time and offline PCM together; timeouts and CPU slices retain wall time.
    let time_scale: f64 = std::env::var("REEL_SCENARIO_TIME_SCALE")
        .map(|value| value.parse().expect("scenario time scale must be a number"))
        .unwrap_or(1.);
    assert!(
        time_scale.is_finite() && time_scale > 0.,
        "scenario time scale must be finite and positive"
    );
    assert!(
        gpu_capture || clicks.is_empty(),
        "scenario clicks require GPU captures"
    );
    let advance_titles = scenario_advances();
    let gpu = gpu_capture || std::env::var("REEL_SCENARIO_GPU").is_ok_and(|value| value == "1");
    assert!(
        !gpu_capture || advance_titles.is_empty(),
        "manual Advance markers use CPU presentation"
    );
    let loaded =
        reel_cli::project::load(path).unwrap_or_else(|error| panic!("{:?}", error.failure));
    let checked = loaded.check().unwrap();
    let entry = reel_cli::project::ENTRY;
    let package_directory = tempfile::tempdir().unwrap();
    let (source_options, packaged_story, packaged_options) = if let Some(path) =
        std::env::var_os("REEL_SCENARIO_PACKAGE")
    {
        let distribution = reel_player::distribution::load(Path::new(&path), None).unwrap();
        let story = reel_player::distribution::story_from_package(distribution.package).unwrap();
        let options = |effects: &reel_engine::aside::source::EffectProducts| {
            if let Some(resources) = &distribution.loose {
                let target = std::env::consts::OS;
                let (root, files) = reel_cli::playback::prepare_story_assets(
                    &loaded.project_root,
                    Some(resources.clone()),
                    target,
                    effects,
                )
                .unwrap();
                return PlayerOptions {
                    assets: AssetOptions {
                        root: root.clone(),
                        ..Default::default()
                    },
                    asset_packages: Some(distribution.packages.clone()),
                    asset_overlay: Some(root),
                    resource_target: target.into(),
                    resource_files: files,
                    catalogs: distribution.catalogs.clone().into(),
                    ..Default::default()
                };
            }
            let shaders = reel_cli::prepare_effects::prepare(
                &loaded.project_root,
                effects,
                reel_cli::prepare_effects::Target::Desktop,
            )
            .unwrap();
            PlayerOptions {
                assets: AssetOptions {
                    root: distribution.root.clone(),
                    ..Default::default()
                },
                asset_packages: Some(distribution.packages.clone()),
                asset_overlay: distribution.loose.clone(),
                resource_target: "vita".into(),
                resource_files: Some(shaders.files),
                catalogs: distribution.catalogs.clone().into(),
                ..Default::default()
            }
        };
        eprintln!("scenario release assets: {:?}", distribution.packages);
        let source_options = options(&checked.effects);
        let packaged_options = options(&story.effects);
        (source_options, story, packaged_options)
    } else {
        let artifact =
            reel_cli::build_export(&checked, entry, reel_cli::BuildProfile::Release).unwrap();
        let output = package_directory.path().join("distribution");
        reel_cli::distribution::export(
            &loaded,
            &artifact,
            &reel_cli::distribution::ExportOptions {
                output: output.clone(),
                overwrite: false,
                resources: if loaded.assets().is_dir() {
                    vec![reel_cli::distribution::Mount {
                        prefix: ".".into(),
                        directory: loaded.assets(),
                    }]
                } else {
                    Vec::new()
                },
                text: Vec::new(),
                resource_target: std::env::consts::OS.into(),
                pack_bytes: (64 << 20).try_into().unwrap(),
                runtime: None,
            },
        )
        .unwrap();
        let mut source_options = PlayerOptions::default();
        let (root, files) = reel_cli::playback::prepare_story_assets(
            &loaded.project_root,
            None,
            &source_options.resource_target,
            &checked.effects,
        )
        .unwrap();
        source_options.assets.root = root;
        source_options.resource_files = files;
        let (story, options) =
            reel_cli::playback::load(&output.join("game.asb"), None, std::env::consts::OS.into())
                .unwrap();
        (source_options, story, options)
    };
    let mut audio_stats = Vec::new();
    let mut capture_stats = Vec::new();
    for (kind, story, mut options) in [
        (
            "source",
            reel_cli::into_story(checked, entry).unwrap(),
            source_options,
        ),
        ("ASB", packaged_story, packaged_options),
    ] {
        let saves = tempfile::tempdir().unwrap();
        let assets = options.open_assets().unwrap();
        let mut audio = audio::AudioClock::default();
        let media = audio.factory(assets.clone());
        let driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap()
        .with_asset_context(assets, options.catalogs)
        .with_media(media())
        .unwrap();
        let directory = Arc::new(DirectorySaves::new(saves.path(), 256 << 20));
        let storage = SaveService::new(
            directory.clone(),
            QueueLimits {
                max_requests: 16,
                max_bytes: 256 << 20,
            },
            WakeSignal::new(|| {}),
        );
        let mut runtime = StoryRuntime::new(driver)
            .with_saves(RuntimeSaves::new(storage, 256 << 20))
            .with_media_factory(media);
        let mut captures = if gpu {
            scenario_capture::ScenarioCapture::new(kind, gpu_capture, first_capture_pressure)
        } else {
            scenario_capture::ScenarioCapture::cpu()
        };
        runtime.set_frame_capture_enabled(gpu_capture);
        let (mut bridge, mut root) = captures.bridge(runtime.driver());
        let mut identity = runtime.driver().ui().session().identity();
        let mut replacements = 0;
        let mut clicks = clicks.iter().peekable();
        let mut advances = advance_titles.iter().peekable();
        let mut advance_phase = 0_u8;
        let mut pointer_events = VecDeque::new();
        let started = Instant::now();
        let logical_elapsed = || started.elapsed().mul_f64(time_scale);
        let slice_ns = Duration::from_millis(2).mul_f64(time_scale).as_nanos() as u64;
        let mut progress_at = Duration::from_secs(30);
        let mut frame = 0;
        loop {
            frame += 1;
            audio.advance(logical_elapsed());
            // Match the window host: native continuations share one frame's CPU
            // deadline. A budget slice must not advance game time by one frame.
            let deadline = logical_elapsed().as_nanos() as u64 + slice_ns;
            runtime
                .tick(
                    SliceBudget {
                        deadline_ns: Some(deadline),
                        ..Default::default()
                    },
                    || logical_elapsed().as_nanos() as u64,
                )
                .unwrap_or_else(|error| {
                    if let Some(fault) = runtime.driver().fault().unwrap() {
                        let vm = runtime.driver().adapter().vm();
                        let package = vm.program().package();
                        let mut stack = vm.heap().fault_stack(fault.id).unwrap();
                        while let Some(frame) = stack.next(vm.heap()).unwrap() {
                            let function = &package.functions[frame.location.code.function.index()];
                            let module = &package.modules[function.module.index()];
                            eprintln!(
                                "{kind}: fault frame {}::{} pc={} registers={} kind={:?}",
                                package.symbols[module.path.index()],
                                package.symbols[function.name.index()],
                                frame.location.pc.0,
                                function.register_count,
                                frame.kind,
                            );
                        }
                    }
                    panic!(
                        "{kind}: {error}; title={:?}; vm={:?}",
                        runtime.driver().adapter().game_settings().title,
                        runtime.driver().adapter().vm().stats(),
                    )
                });
            if started.elapsed() >= progress_at {
                eprintln!(
                    "{kind}: elapsed={:.1}s, logical={:.1}s, frame={frame}, title={:?}, state={:?}, activities={}, presentation={:?}",
                    started.elapsed().as_secs_f64(),
                    logical_elapsed().as_secs_f64(),
                    runtime.driver().adapter().game_settings().title,
                    runtime.driver().state(),
                    runtime.driver().activities_pending(),
                    runtime.driver().projection_stamp()
                );
                progress_at = started.elapsed() + Duration::from_secs(30);
            }
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
            let requested_exit = runtime.driver().state() == StoryState::Cancelled
                && runtime.driver().adapter().exit_requested()
                && runtime.driver().fault().unwrap().is_none()
                && runtime.driver().host_failure().is_none();
            assert!(
                matches!(
                    runtime.driver().state(),
                    StoryState::Running | StoryState::Finished
                ) || requested_exit,
                "{kind}: {:?}; fault={:?}; host={:?}",
                runtime.driver().state(),
                runtime.driver().fault(),
                runtime.driver().host_failure()
            );
            if requested_exit {
                captures.cancel_pending();
            }
            if (runtime.driver().state() == StoryState::Finished || requested_exit)
                && !runtime.needs_tick()
            {
                captures.cancel_pending();
                break;
            }
            // Mutable access would interrupt capture. Let the actual save worker and
            // restore protocol finish before accepting another frame.
            if !runtime.capturing() && !requested_exit {
                let next = runtime.driver().ui().session().identity();
                if next != identity {
                    identity = next;
                    replacements += 1;
                    captures.new_session();
                    (bridge, root) = captures.bridge(runtime.driver());
                }
                if captures.drive(runtime.driver_mut().unwrap()) {
                    runtime.wake_input();
                }
                captures.present(
                    &mut runtime,
                    &mut bridge,
                    root,
                    frame,
                    logical_elapsed().as_nanos() as u64,
                );
                let driver = runtime.driver_mut().unwrap();
                let mut input = false;
                // Enter press/release each use a newly accepted CPU frame, just
                // like pointer input. Consume a marker only after the Session
                // reports the actual manual advance on its current dialogue.
                if advance_phase == 2 {
                    if matches!(driver.ui().session().view().interaction,
                        Some(InteractionView::Dialogue(dialogue)) if dialogue.advance_requested())
                    {
                        eprintln!(
                            "{kind}: manual Enter accepted {:?}",
                            advances.next().unwrap()
                        );
                        advance_phase = 0;
                    }
                } else if advance_phase == 1
                    || advances.peek().is_some_and(|title| {
                        title.as_str() == driver.adapter().game_settings().title
                    })
                {
                    use reel_ui::core::{TextEvent, keyboard};
                    let released = advance_phase == 1;
                    let outcome = driver
                        .handle_text(
                            &mut bridge,
                            TextEvent::Keyboard(keyboard::KeyboardEvent {
                                state: if released {
                                    keyboard::KeyState::Up
                                } else {
                                    keyboard::KeyState::Down
                                },
                                key: keyboard::Key::Named(keyboard::NamedKey::Enter),
                                code: keyboard::Code::Enter,
                                location: keyboard::Location::Standard,
                                modifiers: Default::default(),
                                repeat: false,
                                is_composing: false,
                            }),
                        )
                        .unwrap();
                    eprintln!("{kind}: manual Enter released={released}: {outcome:?}");
                    advance_phase = if released { 2 } else { 1 };
                    input = true;
                }

                if pointer_events.is_empty()
                    && let Some(&&(capture, [x, y])) = clicks.peek()
                    && captures.stats().count >= capture
                {
                    let size = driver
                        .adapter()
                        .game_settings()
                        .canvas_size
                        .unwrap_or([640, 420]);
                    assert!(
                        x < f64::from(size[0]) && y < f64::from(size[1]),
                        "scenario click is outside the accepted canvas"
                    );
                    let [window_x, window_y] = captures.window_point(&bridge, root, [x, y]);
                    pointer_events.extend(support::click_events(
                        window_x,
                        window_y,
                        reel_ui::core::PointerType::Mouse,
                    ));
                    clicks.next();
                    eprintln!("{kind}: pointer click after capture {capture} at ({x}, {y})");
                }
                // Move, press and release each pass through a separate runtime
                // tick and the actual accepted frame's input routing.
                if let Some(event) = pointer_events.pop_front() {
                    driver.handle_pointer(&mut bridge, event).unwrap();
                    input = true;
                }
                if input {
                    runtime.wake_input();
                }
            }
            // Give filesystem/resource workers CPU time between presentation attempts.
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!runtime.driver().cleanup_pending());
        if let Some(key) = std::env::var_os("REEL_SCENARIO_COMPLETED_KEY") {
            let key = key.to_str().expect("scenario completion key must be UTF-8");
            assert!(
                directory.read_data(key).unwrap().is_some(),
                "{kind}: missing persistent completion {key:?}"
            );
            eprintln!("{kind}: persistent completion {key:?} published");
        }
        assert!(
            clicks.next().is_none() && pointer_events.is_empty(),
            "scenario ended before its configured clicks"
        );
        assert!(
            advances.next().is_none() && advance_phase == 0,
            "scenario ended before all manual Advance markers were consumed"
        );
        audio_stats.push(audio.stats());
        if gpu_capture {
            eprintln!("{kind} GPU captures: {:?}", captures.stats());
            capture_stats.push(captures.stats());
        } else {
            assert_eq!(
                captures.stats().count,
                0,
                "presentation-only scenario captured pixels"
            );
        }
        eprintln!(
            "{kind}: {} completed; time_scale={time_scale}, frames={frame}, session_replacements={replacements}, audio={:?}",
            path.display(),
            audio.stats()
        );
        captures.finish();
    }
    (audio_stats, capture_stats)
}

#[test]
#[ignore = "requires a GPU adapter for capture-gated pointer input"]
fn project_scenario_pointer_clicks_reach_modal_buttons_after_accepted_frames() {
    let project = scenario_project();
    let source = project.path().join("src/main.aside");
    std::fs::write(
        &source,
        r##"
        fn main() {
            game.configure(width:64, height:64);
            let clicked=completion();
            let root=stack(width:64,height:64,background:"#ff0000") {
                button(width:64,height:64,on_click:|| clicked.reject("background click"));
            };
            stage.replace(root);
            scope(body:|s| {
                s.add(root,input(width:64,height:64,modal:true) {
                    button(x:16,y:16,width:32,height:32,decorated:false,background:"#0000ff",
                        on_click:|| clicked.resolve(true));
                });
                await stage.presented(); await stage.capture();
                if !(await clicked) {throw "modal click missing";}
                if clicked.resolve(true) {throw "modal click was not settled";}
            });
            await stage.presented();
            stage.replace(stack());
        }
    "##,
    )
    .unwrap();
    let (_, stats) = execute_project_with_capture(
        project.path(),
        Duration::from_secs(30),
        true,
        &[(1, [32., 32.])],
        false,
    );
    assert_eq!(stats.len(), 2);
    for capture in stats {
        assert_eq!(capture.count, 1);
        assert_eq!(capture.center, [0, 0, 255, 255]);
    }
}

#[test]
#[ignore = "requires a GPU adapter for real frame capture"]
fn project_scenario_gpu_capture_restores_png_images_and_particles_at_canvas_size() {
    let project = scenario_project();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 255, 255]))
        .save(project.path().join("resources/blue.png"))
        .unwrap();
    let source = project.path().join("src/main.aside");
    std::fs::write(
        &source,
        r##"
        use std::resume::{state,root,flow,point};
        use reel::types::Completion;
        class World {let mut png:Bytes|null=null;}
        state(World,"world");root(main,"game");
        fn main(w:World=model(World())) {
            game.configure(width: 960, height: 540);
            let ink = particles("blue.png", x: 480, y: 270, seed: 7);
            stage.replace(stack(width: 960, height: 540, background: "#ff0000") {
                image("blue.png", x: 800, width: 160, height: 540);
                ink;
            });
            let gate=completion();
            if await persistent.read("captured") == true {
                spawn(||{await stage.presented();gate.resolve(null);});
            }
            run_flow(story,w,context:{gate:gate,ink:ink})
        }
        fn story(w:World,ctx) {
            flow("capture");
            emit(ctx.ink, burst:1, lifetime:60, size:32, velocity_y:0,
                spread_x:0, spread_y:0, opacity_end:1);
            spawn(||{
                await stage.presented();
                w.png=await stage.capture();
                await saves.save("captured",thumbnail:w.png);
                await persistent.write("captured",true);
                await saves.load("captured");
                throw "capture restore returned to old session";
            });
            let gate:Completion=ctx.gate;
            point("saved");await gate;
            let thumbnail = await saves.thumbnail("captured");
            if thumbnail != w.png { throw "thumbnail bytes differ after restore"; }
            stage.replace(image(w.png, width: 960, height: 540));
            await stage.presented();
            await stage.capture();
        }
    "##,
    )
    .unwrap();
    let (_, stats) =
        execute_project_with_capture(project.path(), Duration::from_secs(90), true, &[], true);
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
fn project_scenario_copies_saved_state_without_capturing_current_state() {
    let project = scenario_project();
    std::fs::write(
        project.path().join("src/main.aside"),
        r#"
        use std::resume::{state,root,flow,point};
        use reel::types::Completion;
        class World {let mut value:int=1;}
        state(World,"world");root(main,"game");
        fn main(w:World=model(World())) {
            stage.replace(stack(width: 320, height: 180));
            let gate=completion();
            if await persistent.read("copied") == true {
                spawn(||{await stage.presented();gate.resolve(null);});
            }
            run_flow(story,w,context:gate)
        }
        fn copy_and_load(w:World) {
            await stage.presented();
            let saved = saves.save("source", title: "Original checkpoint");
            if await saved != null || saved.status != "succeeded" {
                throw "save operation result";
            }
            w.value = 9;
            let copied = saves.copy(source: "source", destination: "backup");
            if await copied != null || copied.status != "succeeded" {
                throw "copy operation result";
            }
            if w.value != 9 { throw "copy changed the current session"; }
            let slots = await saves.list();
            if slots.len() != 2 || slots[0].id != "backup" || slots[1].id != "source"
                || slots[0].title != "Original checkpoint"
                || slots[0].title != slots[1].title || slots[0].saved_at != slots[1].saved_at
                || slots[0].has_thumbnail != slots[1].has_thumbnail {
                throw "copy metadata changed";
            }
            await saves.copy("backup", "backup");
            let failed = saves.copy("missing", "backup");
            let mut rejected = false;
            try { await failed; } catch error { rejected = true; }
            if !rejected || failed.status != "failed" || failed.error == null {
                throw "missing source was accepted";
            }
            let mut invalid = 0;
            try { saves.copy("", "backup"); } catch error { invalid += 1; }
            try { saves.copy("source", ""); } catch error { invalid += 1; }
            if invalid != 2 { throw "empty slot identifier was accepted"; }
            await persistent.write("copied", true);
            await saves.load("backup");
            throw "load returned to the old session";
        }
        fn story(w:World,gate:Completion) {
            flow("copy");
            spawn(||copy_and_load(w));
            point("saved");await gate;
            if w.value != 1 || await persistent.read("copied") != true {
                throw "copy did not retain the original saved continuation";
            }
        }
    "#,
    )
    .unwrap();
    execute_project(project.path(), Duration::from_secs(90));
}

#[test]
fn project_scenario_executes_assertions_across_save_restore_and_new_session() {
    let project = scenario_project();
    let source = project.path().join("src/main.aside");
    std::fs::write(
        &source,
        r#"
        use std::resume::{state,root,flow,point};
        use reel::types::Completion;
        class World {let marker:int=17;}
        state(World,"world");root(main,"game");
        fn main(w:World=model(World())) {
            stage.replace(stack(width: 320, height: 180));
            let gate=completion();
            if await persistent.read("progress") == 42 {
                spawn(||{await stage.presented();gate.resolve(null);});
            }
            run_flow(story,w,context:gate)
        }
        fn story(w:World,gate:Completion) {
            flow("progress");
            game.record("Chapter one\nAdditional detail");
            spawn(||{
                await stage.presented();
                await saves.save("checkpoint");
                let slots = await saves.list();
                if slots[0].title != "Chapter one" { throw "default save title lost the transcript"; }
                await persistent.write("progress", 42);
                await saves.load("checkpoint");
                throw "load returned to the old session";
            });
            point("saved");await gate;
            if w.marker != 17 || await persistent.read("progress") != 42 {
                throw "persistent rewind";
            }
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
    execute_project(project.path(), Duration::from_secs(90));
}

#[test]
fn project_scenario_rebuilds_transition_target_from_a_checkpoint_saved_during_playback() {
    let project = scenario_project();
    for (name, color) in [
        ("old.png", [255, 0, 0, 255]),
        ("new.png", [0, 0, 255, 255]),
        ("mask.png", [128, 128, 128, 255]),
    ] {
        image::RgbaImage::from_pixel(16, 16, image::Rgba(color))
            .save(project.path().join("resources").join(name))
            .unwrap();
    }
    let source = project.path().join("src/main.aside");
    std::fs::write(
        &source,
        r##"
        use std::resume::{state,root,flow,point};
        use reel::types::Completion;
        class World {let mut calls:int=0;let result:table={value:42};let mut changed:bool=false;}
        state(World,"world");root(main,"game");
        fn main(w:World=model(World())) {
            game.configure(width: 320, height: 180);
            let panel = stack(x: 30, width: 200, height: 180) {
                image(if w.changed {"new.png"} else {"old.png"}, width: 200, height: 180);
            };
            stage.replace(stack(width: 320, height: 180, background: "#000000") { panel; });
            let gate=completion();
            if await persistent.read("transition") == true {
                spawn(||{await stage.presented();gate.resolve(null);});
            }
            run_flow(story,w,context:{gate:gate,panel:panel})
        }
        fn story(w:World,ctx) {
            flow("transition");
            let panel=ctx.panel;
            let changed = completion();
            let work = spawn(|| {
                stage.transition(target: panel, duration: 0.4, mask: "mask.png", change: || {
                    w.calls += 1;
                    w.changed = true;
                    panel.clear();
                    panel.add(image("new.png", width: 200, height: 180));
                    changed.resolve(null);
                    w.result
                })
            });
            await changed;
            await stage.presented();
            await delay(0.03).run();
            spawn(||{
                await stage.presented();
                await saves.save("transition");
                if await work != w.result { throw "transition callback result identity changed"; }
                if w.calls != 1 { throw "transition callback executed twice"; }
                await persistent.write("transition",true);
                await saves.load("transition");
                throw "load returned to the old transition";
            });
            let gate:Completion=ctx.gate;
            point("saved");await gate;
            if w.calls != 1 || !w.changed { throw "transition change replayed on restore"; }
            let zero = stage.transition(target: panel, duration: 0, mask: "unused.png", change: || {
                w.calls += 1;
                panel.x = 90;
                w.result
            });
            if zero != w.result || w.calls != 2 || panel.x != 90 {
                throw "zero-duration transition did not finish its new frame";
            }
            stage.transition(target: panel, duration: 0.02, change: || {
                panel.visible = false;
            });
            if panel.visible { throw "transition to an empty subtree failed"; }
            stage.transition(duration: 0.02, change: || {
                stage.replace(stack(width: 320, height: 180, background: "#00ff00"));
            });
        }
    "##,
    )
    .unwrap();
    execute_project_with_capture(project.path(), Duration::from_secs(30), false, &[], false);
}

#[test]
#[ignore = "requires a GPU adapter for transition pixels and current Root reconstruction"]
fn project_scenario_gpu_transition_keeps_ancestor_opacity_after_root_reconstruction() {
    let project = scenario_project();
    for (name, color) in [("old.png", [255, 0, 0, 255]), ("new.png", [0, 0, 255, 255])] {
        image::RgbaImage::from_pixel(16, 16, image::Rgba(color))
            .save(project.path().join("resources").join(name))
            .unwrap();
    }
    let source = project.path().join("src/main.aside");
    std::fs::write(
        &source,
        r##"
        use std::resume::{state,root,flow,point};
        use reel::types::Completion;
        class World {let mut calls:int=0;let mut changed:bool=false;}
        state(World,"world");root(main,"game");
        fn reveal_new(panel,changed,update) {
            stage.transition(target:panel,duration:2,change:||{
                update();
                panel.clear();
                panel.add(image("new.png",width:640,height:360));
                changed.resolve(null);
            })
        }
        fn main(w:World=model(World())) {
            game.configure(width: 960, height: 540);
            let panel = stack(width: 640, height: 360) {
                image("old.png", width: 640, height: 360);
            };
            stage.replace(stack(width: 960, height: 540, background: "#00ff00") {
                stack(x: 160, y: 90, width: 640, height: 360, opacity: 0.5, clip: true) {
                    panel;
                }
            });
            let gate=completion();
            if await persistent.read("middle") == true {
                if !w.changed || w.calls!=1 {throw "lost the saved transition stage";}
                spawn(||{
                    await stage.presented();
                    let changed=completion();
                    let animation=spawn(||reveal_new(panel,changed,||{}));
                    await changed;
                    await stage.presented();
                    await delay(0.25).run();
                    await stage.capture();
                    await animation;
                    gate.resolve(null);
                });
            }
            run_flow(story,w,context:{gate:gate,panel:panel})
        }
        fn story(w:World,ctx) {
            flow("transition");
            await stage.presented();
            await stage.capture();
            let panel=ctx.panel;
            let changed = completion();
            spawn(||reveal_new(panel,changed,||{w.calls+=1;w.changed=true;}));
            await changed;
            await stage.presented();
            await delay(0.25).run();
            await stage.capture();
            spawn(||{
                await stage.presented();
                await saves.save("middle");
                await persistent.write("middle",true);
                await saves.load("middle");
                throw "load returned to old transition";
            });
            let gate:Completion=ctx.gate;
            point("saved");await gate;
            if w.calls != 1 { throw "saved transition replayed the business change"; }
            await stage.presented();
            await stage.capture();
        }
    "##,
    )
    .unwrap();
    let (_, stats) =
        execute_project_with_capture(project.path(), Duration::from_secs(90), true, &[], false);
    assert_eq!(stats.len(), 2);
    for stats in stats {
        assert_eq!(
            stats.count, 4,
            "source and ASB both capture four actual accepted frames"
        );
        let old = stats.centers[0];
        let middle = stats.centers[1];
        let restored = stats.centers[2];
        let new = stats.centers[3];
        assert!(old[0] >= 180 && old[1] >= 180 && old[2] <= 3, "{old:?}");
        assert!(new[0] <= 3 && new[1] >= 180 && new[2] >= 180, "{new:?}");
        for pixel in [middle, restored] {
            assert!(
                pixel[0] > 5 && pixel[2] > 5,
                "transition lost an input: {pixel:?}"
            );
            assert!(
                pixel[1].abs_diff(old[1]) <= 3,
                "ancestor opacity was applied twice: {pixel:?}"
            );
            assert_eq!(pixel[3], 255);
        }
        assert_eq!(
            stats.right,
            [0, 255, 0, 255],
            "transition changed pixels outside its ancestor clip"
        );
    }
}

#[test]
fn project_scenario_mixes_audio_queue_across_save_restore_and_new_session() {
    let project = scenario_project();
    std::fs::write(
        project.path().join("resources/chime.wav"),
        include_bytes!("../../../examples/assets/chime.wav"),
    )
    .unwrap();
    let source = project.path().join("src/main.aside");
    std::fs::write(
        &source,
        r#"
        use std::resume::{state,root,flow,point};
        use reel::types::{Completion,Playback};
        class World {let mut sound:Playback|null=null;let mut position:float=0.0;}
        state(World,"world");root(main,"game");
        fn main(w:World=model(World())) {
            stage.replace(stack(width: 320, height: 180));
            let gate=completion();
            if await persistent.read("queued") == true {
                spawn(||{await stage.presented();gate.resolve(null);});
            }
            run_flow(story,w,context:gate)
        }
        fn story(w:World,gate:Completion) {
            flow("audio");
            await stage.presented();
            let sound = audio.play("chime.wav", group: "music", volume: 0.5,
                looping: true, loop_start: 0.02, loop_end: 0.2);
            w.sound=sound;
            if !(await sound.at(0.03)) {throw "queue position not reached";}
            sound.pause();
            await delay(0.02).run();
            audio.queue(sound, "chime.wav", start: 0.1);
            w.position=sound.position();
            if w.position <= 0 { throw "audio did not consume PCM"; }
            spawn(||{
                await stage.presented();
                await saves.save("audio");
                audio.stop(sound);
                if await sound != "Stopped" { throw "stop did not settle"; }
                await persistent.write("queued",true);
                await saves.load("audio");
                throw "load returned to old audio";
            });
            point("saved");await gate;
            await delay(0.02).run();
            if sound!=w.sound || sound.position() < w.position - 0.001
                || sound.position() > w.position + 0.001 {
                throw "restored paused position changed";
            }
            sound.resume();
            if await sound != "Completed" || await sound != "Completed" {
                throw "restored queue did not complete";
            }
            if sound.position() < 0.19 { throw "queue tail did not play"; }
            game.start(finish);
        }
        fn finish() {
            let sound = audio.play("chime.wav");
            if await sound != "Completed" || sound.position() < 0.19 {
                throw "new session lost media backend";
            }
        }
    "#,
    )
    .unwrap();
    for mixed in execute_project(project.path(), Duration::from_secs(90)) {
        assert!(mixed.frames > 0, "offline device did not consume frames");
        assert!(
            mixed.peak > 0.01,
            "decoded fixture produced no audible PCM: {mixed:?}"
        );
    }
}

#[test]
fn project_scenario_loads_effects_catalogs_images_and_audio_from_release_archives() {
    let project = scenario_project();
    image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 255, 255]))
        .save(project.path().join("resources/tile.png"))
        .unwrap();
    std::fs::write(
        project.path().join("resources/chime.wav"),
        include_bytes!("../../../examples/assets/chime.wav"),
    )
    .unwrap();
    std::fs::write(
        project.path().join("src/main.aside"),
        r#"
        use std::resume::{state,root,flow,point};
        use reel::types::Completion;
        class World {let marker:int=17;}
        state(World,"world");root(main,"game");
        fn tint(pixel, amount: float = 0.5) -> (float, float, float, float) {
            let (r, g, b, a) = pixel.sample(pixel.uv);
            (r, g, b * amount, a)
        }
        fn main(w:World=model(World())) {
            localization.set_locale("en");
            let caption = tr("原文", {en: "Prepared resources"});
            if caption != "Prepared resources" { throw "packaged translation missing"; }
            let page = stack(width: 64, height: 64, effect: effect(tint)) {
                image("tile.png", width: 64, height: 64);
            };
            stage.replace(page);
            let gate=completion();
            if await persistent.read("resources") == true {
                spawn(||{await stage.presented(owner:page);gate.resolve(null);});
            }
            run_flow(story,w,context:{gate:gate,page:page})
        }
        fn story(w:World,ctx) {
            flow("resources");
            let page=ctx.page;
            spawn(||{
                await stage.presented(owner:page);
                await saves.save("resources");
                await persistent.write("resources",true);
                await saves.load("resources");
                throw "resource restore returned to old session";
            });
            let gate:Completion=ctx.gate;
            point("saved");await gate;
            await stage.presented(owner:page);
            if w.marker!=17 || tr("再见", {en: "Restored resources"}) != "Restored resources" {
                throw "restored catalog missing";
            }
            if await audio.play("chime.wav", owner:page) != "Completed" {
                throw "packaged audio failed";
            }
            page.remove();
        }
    "#,
    )
    .unwrap();
    for mixed in execute_project(project.path(), Duration::from_secs(30)) {
        assert!(
            mixed.peak > 0.01,
            "packaged audio produced no PCM: {mixed:?}"
        );
    }
}

#[test]
fn project_scenario_media_position_cue_restores_with_real_audio() {
    let project = scenario_project();
    std::fs::write(
        project.path().join("resources/chime.wav"),
        include_bytes!("../../../examples/assets/chime.wav"),
    )
    .unwrap();
    let source = project.path().join("src/main.aside");
    std::fs::write(
        &source,
        r#"
        use std::resume::{state,root,flow,point};
        use reel::types::{Completion,Playback};
        class World {let mut sound:Playback|null=null;}
        state(World,"world");root(main,"game");
        fn main(w:World=model(World())) {
            let cursor=derive(||if w.sound==null{0.0}else{w.sound.position()});
            let marker=stack(width:20,height:20,x:derive(||cursor.value*100));
            stage.replace(stack(width:320,height:180){marker;});
            let gate=completion();
            if await persistent.read("position") == true {
                spawn(||{await stage.presented();gate.resolve(null);});
            }
            run_flow(story,w,context:{gate:gate,marker:marker})
        }
        fn story(w:World,ctx) {
            flow("position");
            let sound=audio.play("chime.wav",paused:true);
            w.sound=sound;
            let marker=ctx.marker;
            await stage.presented();
            if sound.position()!=0 || marker.x!=0 {throw "paused loading advanced";}
            let cue=sound.at(0.125);
            sound.seek(0.05);
            if sound.position()!=0.05 {throw "seek observation";}
            await stage.presented();
            if marker.x!=5 {throw "seek binding";}
            spawn(||{
                await stage.presented();
                await saves.save("position");
                await persistent.write("position",true);
                await saves.load("position");
                throw "old session returned";
            });
            let gate:Completion=ctx.gate;
            point("saved");await gate;
            if sound.position()!=0.05 || marker.x!=5 {throw "restored position";}
            sound.resume();
            if !(await cue) || !(await cue) {throw "restored position wait";}
            if sound.position()<0.125 {throw "cue preceded observed position";}
            if await sound!="Completed" {throw "audio terminal";}
            await stage.presented();
            if marker.x<24 {throw "final reactive position";}
            if !(await sound.at(sound.position())) {throw "final threshold";}
            if await sound.at(1) {throw "early ending reported arrival";}
        }
    "#,
    )
    .unwrap();
    for mixed in execute_project(project.path(), Duration::from_secs(30)) {
        assert!(mixed.frames > 0);
        assert!(mixed.peak > 0.01, "real decoded PCM was not consumed");
    }
}

#[test]
fn segmented_dialogue_resumes_after_deferred_skip_and_auto_advance() {
    let project = scenario_project();
    let source = project.path().join("src/main.aside");
    std::fs::write(
        &source,
        r#"
        fn main() {
            game.configure(text_speed:0);
            let owner = stack(width:64, height:64);
            stage.replace(owner);
            for skipping in [true, false] {
                if skipping {game.reading().set_skip(true, unread:true);}
                else {game.reading().set_auto(true, delay:0.01);}
                let line = narrator("Deferred advance", wait:false);
                await stage.presented(owner:owner);
                await delay(0.02).run(owner:owner);
                if !game.dialogue().ready {throw "deferred line was not revealed";}
                line.wait();
                line.append(" and another segment", wait:false);
                await stage.presented(owner:owner);
                await delay(0.02).run(owner:owner);
                if !game.dialogue().ready {throw "appended line was not revealed";}
                line.wait();
                line.append(" and a waiting segment");
                if game.dialogue().text!="Deferred advance and another segment and a waiting segment" {
                    throw "waiting append did not retain its open speech";
                }
                if !game.dialogue().active {throw "open dialogue was retired";}
            }
            game.reading().set_auto(false);
            game.reading().set_skip(false);
            let timed=narrator(rich("Timed[next]"),wait:false,reveal_skippable:false);
            timed.wait();
            timed.append(rich(" continuation[pause=0.01][next]"));
            if game.dialogue().text.plain!="Timed continuation" {throw "waiting append did not honor next";}
        }
        "#,
    )
    .unwrap();
    execute_project(project.path(), Duration::from_secs(15));
}

#[test]
fn offline_audio_clock_preserves_fractional_time_and_releases_retired_outputs() {
    let mut audio = audio::AudioClock::default();
    let media = audio.factory(Assets::directory(AssetOptions {
        root: std::env::temp_dir(),
        ..Default::default()
    }));
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
#[ignore = "requires REEL_SCENARIO pointing to a self-checking project directory"]
fn external_project_scenario() {
    let path =
        std::env::var_os("REEL_SCENARIO").expect("set REEL_SCENARIO to a Reel project directory");
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
