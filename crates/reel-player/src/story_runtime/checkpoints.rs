//! Real storage and authored controls exercise replacement with independently compiled code.
mod media;
use super::save_cost::{Screen, has_text};
use super::*;
use crate::saves::{DirectorySaves, RuntimeSaves, SaveService, SaveStorage};
use reel_core::{budget::QueueLimits, wake::WakeSignal};
use reel_engine::{Epoch, InputAction, InteractionView, Session, SessionId};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const OLD_LINE: &str =
    "The original page stays half revealed while its saved world crosses versions.";

fn source(current: bool) -> String {
    let (class, field, inner, outer, prefix, suffix, label, default, extra, entry) = if current {
        (
            "CurrentStats",
            "points",
            "current_chapter",
            "current_story",
            "world.primary.points+=1000;",
            "world.primary.points+=100;",
            "B current custom view",
            "throw \"current default constructor executed\";",
            "let extra:int=99;",
            "stage.replace(menu(\"B title\"));",
        )
    } else {
        (
            "Stats",
            "score",
            "chapter",
            "story",
            "world.primary.score+=5;",
            "world.primary.score+=old_reward;",
            "A custom view",
            "let value=model(Stats());self.primary=value;self.alias=value;for i in 0..512{self.items.append(i);}",
            "",
            "start_story();",
        )
    };
    let fields = if current {
        ",fields:{points:\"score\"}"
    } else {
        ""
    };
    let extra_view = if current {
        "page.add(text(value:derive(||f\"extra={world.primary.extra}\"),y:140));"
    } else {
        ""
    };
    let old_local = if current {
        ""
    } else {
        "let old_reward=world.primary.score-2;"
    };
    let line = if current {
        "B changed the text at the saved point"
    } else {
        OLD_LINE
    };
    let next = if current { "B new suffix" } else { "A suffix" };
    format!(
        r#"
        use std::resume::{{state,root,flow,point}};
        {unrelated}
        class {class} {{ {extra} let mut {field}:int=7; }}
        state({class},"stats"{fields});
        class World {{
            let primary:{class};let alias:{class};
            let mut bootstraps:int=0;let mut items:array=[];
            fn init(self){{{default}}}
        }}
        state(World,"world");root(start_story,"game");
        fn menu(label){{
            let status=model({{value:""}});
            stack(width:800,height:600){{
                button("Save",width:90,height:40,on_click:||{{
                    try {{await saves.save("slot",title:"Version checkpoint");status.value="saved";}}
                    catch error {{status.value="save failed";}}
                }});
                button("Load",x:100,width:90,height:40,on_click:||{{
                    try {{await saves.load("slot");status.value="old load returned";}}
                    catch error {{status.value="load failed";}}
                }});
                text(label,y:50);text(value:derive(||status.value),y:170);
            }}
        }}
        fn {inner}(world:World){{
            flow("chapter");{prefix}{old_local}
            point("line");narrator("{line}");
            {suffix}
        }}
        fn {outer}(world:World){{
            flow("story");point("chapter");{inner}(world);
            if world.primary!=world.alias||world.items.len()!=512||world.items[511]!=511{{throw "shared State changed";}}
            point("suffix");narrator("{next}");
        }}
        fn start_story(world:World=model(World())){{
            game.configure(width:800,height:600,text_speed:30);
            world.bootstraps+=1;
            let page=menu("{label}");let dialogue=game.dialogue();
            page.add(text(value:derive(||f"score={{world.primary.{field}}} same={{world.primary==world.alias}} boots={{world.bootstraps}}"),y:80));
            page.add(text(value:derive(||dialogue.text),y:110));
            {extra_view}
            stage.replace(page);run_flow({outer},world);
        }}
        fn main(){{{entry}}}
    "#,
        unrelated = if current { "fn moved_before(){17}" } else { "" }
    )
}

fn folder() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/reel-artifacts/logical-player-checkpoints")
        .join(format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
}
fn runtime(source: &str, version: u64, storage: Arc<DirectorySaves>) -> StoryRuntime {
    let driver = AsideSessionUi::from_source(
        Session::new(SessionId(510), Epoch(1)).unwrap(),
        source,
        "player_checkpoint",
        version,
        "main",
        "en",
        Default::default(),
    )
    .unwrap();
    let service = SaveService::new(
        storage,
        QueueLimits {
            max_requests: 1,
            max_bytes: 64 << 20,
        },
        WakeSignal::new(|| {}),
    );
    StoryRuntime::new(driver).with_saves(RuntimeSaves::new(service, 64 << 20))
}
fn drain(runtime: &mut StoryRuntime) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while runtime.short_poll().is_some() || runtime.saves.as_deref().is_some_and(RuntimeSaves::busy)
    {
        assert!(
            Instant::now() < deadline,
            "checkpoint runtime did not settle"
        );
        runtime.tick(SliceBudget::default(), || 0).unwrap();
        std::thread::yield_now();
    }
}
fn close(runtime: &mut StoryRuntime) {
    runtime.close().unwrap();
    drain(runtime);
    assert!(runtime.close_complete());
}
fn dialogue(runtime: &StoryRuntime) -> (String, usize, String) {
    let Some(InteractionView::Dialogue(dialogue)) =
        runtime.driver().ui().session().view().interaction
    else {
        panic!("dialogue")
    };
    (
        dialogue.text().to_owned(),
        dialogue.reveal().revealed_count,
        dialogue.lines()[0].content_id().to_owned(),
    )
}
fn reveal_some(runtime: &mut StoryRuntime, count: usize) {
    let session = runtime.driver_mut().unwrap().ui_mut().session_mut();
    let id = session.view().interaction.unwrap().id();
    session
        .reveal_more(session.root_owner(), id, count)
        .unwrap();
    runtime.wake_input();
    drain(runtime);
}
fn advance(runtime: &mut StoryRuntime) {
    let session = runtime.driver_mut().unwrap().ui_mut().session_mut();
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
        if session
            .view()
            .interaction
            .is_none_or(|interaction| interaction.id() != id)
        {
            break;
        }
    }
    runtime.wake_input();
    drain(runtime);
}
fn saved(storage: Arc<DirectorySaves>) -> (String, usize, String) {
    saved_with(&source(false), storage)
}

fn saved_with(code: &str, storage: Arc<DirectorySaves>) -> (String, usize, String) {
    let mut old = runtime(code, 1, storage.clone());
    drain(&mut old);
    let mut screen = Screen::new(&old);
    assert!(screen.present(&mut old));
    drain(&mut old);
    reveal_some(&mut old, 8);
    let before = dialogue(&old);
    assert_eq!(before.0, OLD_LINE);
    assert_eq!(before.1, 8);
    assert!(screen.present(&mut old));
    screen.click(&mut old, 40.);
    drain(&mut old);
    has_text(&old, "saved");
    assert_eq!(storage.list().unwrap().len(), 1);
    assert_eq!(old.driver().ui().session().view().history.len(), 1);
    close(&mut old);
    before
}

#[test]
fn disk_checkpoint_rebuilds_current_root_and_resumes_saved_page_into_new_suffix() {
    let folder = folder();
    let storage = Arc::new(DirectorySaves::new(&folder, 64 << 20));
    let automatic = |code: String| {
        code.replace("state(World,\"world\");", "")
            .replace("point(\"line\");", "")
            .replace("point(\"chapter\");", "")
            .replace("point(\"suffix\");", "")
    };
    let before = saved_with(&automatic(source(false)), storage.clone());
    let code = automatic(source(true))
        .replace("World", "CurrentWorld")
        .replace(
            "stage.replace(page);run_flow",
            r#"
        let preference = await persistent.read("candidate-write", default: "current preference");
        page.add(text(preference, y: 200));
        try { await persistent.write("candidate-write", true); }
        catch error { page.add(text("storage write rejected", y: 230)); }
        try { await window.configure(width: 800, height: 600); }
        catch error { page.add(text("window change rejected", y: 260)); }
        stage.replace(page);run_flow
        "#,
        );
    let mut current = runtime(&code, 27, storage.clone());
    drain(&mut current);
    current
        .driver_mut()
        .unwrap()
        .adapter_mut()
        .set_window_info(Arc::new(reel_core::window::WindowInfo {
            capabilities: reel_core::window::WindowCapabilities {
                resize: true,
                ..Default::default()
            },
            ..Default::default()
        }));
    let identity = current.driver().ui().session().identity();
    has_text(&current, "B title");
    let mut screen = Screen::new(&current);
    assert!(screen.present(&mut current));
    screen.click(&mut current, 140.);
    drain(&mut current);
    assert_ne!(current.driver().ui().session().identity(), identity);
    has_text(&current, "B current custom view");
    has_text(&current, "score=12 same=true boots=2");
    has_text(&current, "extra=99");
    has_text(&current, "current preference");
    has_text(&current, "storage write rejected");
    has_text(&current, "window change rejected");
    assert!(storage.read_data("candidate-write").unwrap().is_none());
    assert_eq!(dialogue(&current), before);
    let history = current.driver().ui().session().view().history;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text(), OLD_LINE);
    assert!(screen.present(&mut current));
    drain(&mut current);
    advance(&mut current);
    assert_eq!(dialogue(&current).0, "B new suffix");
    has_text(&current, "score=112 same=true boots=2");
    let history = current.driver().ui().session().view().history;
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].text(), OLD_LINE);
    assert_eq!(history[1].text(), "B new suffix");
    assert_eq!(
        current
            .driver()
            .ui()
            .session()
            .read_progress()
            .ids()
            .count(),
        1
    );
    close(&mut current);
    storage.delete("slot").unwrap();
    std::fs::remove_dir(folder).unwrap();
}

#[test]
fn history_jump_restores_data_at_an_earlier_dialogue_and_truncates_the_transcript() {
    let folder = folder();
    let storage = Arc::new(DirectorySaves::new(&folder, 64 << 20));
    let code = r#"
        use std::resume::{state, root, flow};
        class World { let mut score: int = 0; }
        state(World, "world"); root(story_root, "story-root");
        fn line(text: string) { flow("line"); narrator(text); }
        fn story(world: World) {
            flow("story");
            world.score += 1; line("First dialogue");
            world.score += 4; line("Second dialogue");
        }
        fn story_root(world: World = model(World())) {
            game.configure(width: 800, height: 600, text_speed: 0);
            saves.track_history(true);
            stage.replace(stack {
                button("Jump", width: 90, height: 40, on_click: || {
                    let group = game.history()[0].group_id;
                    if !saves.can_jump(group) { throw "missing retained history group"; }
                    await saves.jump(group);
                    throw "old session continued after jump";
                });
                text(value: derive(|| f"score={world.score}"), y: 60);
            });
            run_flow(story, world);
        }
        fn main() { story_root(); }
    "#;
    let mut runtime = runtime(code, 1, storage.clone());
    drain(&mut runtime);
    let mut screen = Screen::new(&runtime);
    assert!(screen.present(&mut runtime));
    drain(&mut runtime);
    reveal_some(&mut runtime, usize::MAX);
    assert!(screen.present(&mut runtime));
    drain(&mut runtime);
    advance(&mut runtime);
    assert_eq!(dialogue(&runtime).0, "Second dialogue");
    has_text(&runtime, "score=5");
    assert!(screen.present(&mut runtime));
    let old = runtime.driver().ui().session().identity();
    screen.click(&mut runtime, 40.);
    drain(&mut runtime);
    assert_ne!(old, runtime.driver().ui().session().identity());
    assert_eq!(dialogue(&runtime).0, "First dialogue");
    assert_eq!(runtime.driver().ui().session().view().history.len(), 1);
    has_text(&runtime, "score=1");
    assert!(screen.present(&mut runtime));
    drain(&mut runtime);
    advance(&mut runtime);
    assert_eq!(dialogue(&runtime).0, "Second dialogue");
    has_text(&runtime, "score=5");
    assert!(storage.list().unwrap().is_empty());
    close(&mut runtime);
}

#[test]
fn incompatible_saved_point_keeps_live_session_and_committed_slot() {
    let folder = folder();
    let storage = Arc::new(DirectorySaves::new(&folder, 64 << 20));
    saved(storage.clone());
    let mut stored = Vec::new();
    storage.read("slot").unwrap().write_to(&mut stored).unwrap();
    for code in [
        source(true).replace("point(\"line\")", "point(\"changed-line\")"),
        source(true)
            .replace("let mut points:int=7", "let mut points:string=\"changed\"")
            .replace("world.primary.points+=1000;", "")
            .replace("world.primary.points+=100;", ""),
        source(true)
            .replace(
                "point(\"line\");",
                "let new_reward=world.primary.points;point(\"line\");",
            )
            .replace(
                "world.primary.points+=100;",
                "world.primary.points+=new_reward;",
            ),
    ] {
        let mut current = runtime(&code, 27, storage.clone());
        drain(&mut current);
        let identity = current.driver().ui().session().identity();
        let mut screen = Screen::new(&current);
        assert!(screen.present(&mut current));
        screen.click(&mut current, 140.);
        drain(&mut current);
        assert_eq!(current.driver().ui().session().identity(), identity);
        has_text(&current, "B title");
        has_text(&current, "load failed");
        assert!(current.driver().ui().session().view().history.is_empty());
        let mut after = Vec::new();
        storage.read("slot").unwrap().write_to(&mut after).unwrap();
        assert_eq!(after, stored);
        close(&mut current);
    }
    storage.delete("slot").unwrap();
    std::fs::remove_dir(folder).unwrap();
}

#[test]
fn input_cancels_frozen_capture_and_keeps_the_live_reading() {
    let folder = folder();
    let storage = Arc::new(DirectorySaves::new(&folder, 64 << 20));
    let mut story = runtime(&source(false), 1, storage.clone());
    drain(&mut story);
    let identity = story.driver().ui().session().identity();
    let mut screen = Screen::new(&story);
    assert!(screen.present(&mut story));
    drain(&mut story);
    screen.click(&mut story, 40.);
    for _ in 0..1000 {
        if story.capturing() {
            break;
        }
        story.tick(SliceBudget::default(), || 0).unwrap();
    }
    assert!(story.capturing());
    reveal_some(&mut story, 8);
    assert!(!story.capturing());
    assert_eq!(story.driver().ui().session().identity(), identity);
    assert_eq!(dialogue(&story).0, OLD_LINE);
    assert_eq!(dialogue(&story).1, 8);
    assert_eq!(story.driver().ui().session().view().history.len(), 1);
    has_text(&story, "save failed");
    assert!(storage.list().unwrap().is_empty());
    close(&mut story);
    if folder.exists() {
        std::fs::remove_dir(folder).unwrap();
    }
}
