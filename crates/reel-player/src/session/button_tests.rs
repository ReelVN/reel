mod focus;

use super::tests::{mount, mouse, session};
use super::*;
use crate::{
    aside_session::{AsideSessionUi, StoryState},
    story_runtime::StoryRuntime,
};
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_engine::aside::{SliceBudget, VmLimits};
use reel_ui::{
    bridge::{NoticeResult, PresentationNotice, PresentationOutcome},
    stage::NodeKind,
};

#[test]
fn save_slot_survives_player_restart_rebuilds_callbacks_and_preserves_slot_on_write_failure() {
    use crate::saves::{DirectorySaves, RuntimeSaves, SaveService, SaveStorage};
    use reel_core::{budget::QueueLimits, wake::WakeSignal};
    use std::sync::Arc;
    const SOURCE: &str = r#"
        use std::resume::{state,root,flow,point};
        class ButtonState {let mut count:int=1;}
        state(ButtonState,"buttons");root(main,"button-story");
        fn story(data:ButtonState){flow("buttons");point("waiting");narrator("saved input");}
        fn main(data:ButtonState=model(ButtonState())) {
            game.configure(text_speed:0);
            stage.replace(stack(width: 600, height: 400) {
                button("Save", x: 0, width: 90, height: 40, on_click: |e| {
                    data.count += 1;await saves.save("slot/一", title: "Chapter One");
                });
                button("Load", x: 100, width: 90, height: 40, on_click: |e| { await saves.load("slot/一"); throw "old load callback resumed"; });
                button("Change", x: 200, width: 90, height: 40, on_click: |e| { data.count += 10; });
                text(value: derive(|| f"count={data.count}"), y: 100);
            });
            run_flow(story,data);
        }
    "#;
    let folder = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/reel-artifacts/save-tests")
        .join(format!(
            "restart-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    let storage = Arc::new(DirectorySaves::new(&folder, 16 << 20));
    let create = || {
        let wake = WakeSignal::new(|| {});
        let service = SaveService::new(
            storage.clone(),
            QueueLimits {
                max_requests: 1,
                max_bytes: 16 << 20,
            },
            wake.clone(),
        );
        runtime(SOURCE)
            .with_saves(RuntimeSaves::new(service, 16 << 20))
            .with_media_factory(move || {
                Box::new(crate::media::MediaPlayer::new(
                    crate::assets::Assets::directory(Default::default()),
                    Default::default(),
                    wake.clone(),
                    |_| Err("test has no sound output".into()),
                ))
            })
    };
    let wait = |runtime: &mut StoryRuntime| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while runtime.needs_tick() {
            assert!(
                std::time::Instant::now() < deadline,
                "save/load did not settle"
            );
            runtime
                .tick(SliceBudget::default(), || 5_000_000_000)
                .unwrap();
            std::thread::yield_now();
        }
    };
    let activate = |runtime: &mut StoryRuntime, bridge: &mut PresentationBridge, x: f64| {
        for kind in ["move", "down", "up"] {
            runtime
                .driver_mut()
                .unwrap()
                .handle_pointer(bridge, mouse(kind, (x, 20.)))
                .unwrap();
        }
        runtime.wake_input();
        wait(runtime);
    };
    let has_count = |runtime: &StoryRuntime, expected: &str| {
        assert!(
            runtime.driver().adapter().stage().nodes().any(
                |node| matches!(&node.kind, NodeKind::Text(text) if text.as_ref() == expected)
            ),
            "expected {expected}"
        );
    };
    let mut first = create();
    wait(&mut first);
    let (mut bridge, root) = mount(first.driver().ui());
    present(&mut first, &mut bridge, root, 1);
    activate(&mut first, &mut bridge, 40.);
    has_count(&first, "count=2");
    let slots = storage.list().unwrap();
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].title, "Chapter One");
    first.close().unwrap();
    wait(&mut first);
    drop(first);
    drop(bridge);

    let mut second = create();
    wait(&mut second);
    let (mut bridge, root) = mount(second.driver().ui());
    present(&mut second, &mut bridge, root, 1);
    activate(&mut second, &mut bridge, 240.);
    has_count(&second, "count=11");
    present(&mut second, &mut bridge, root, 2);
    activate(&mut second, &mut bridge, 140.);
    has_count(&second, "count=2");
    present(&mut second, &mut bridge, root, 3);
    activate(&mut second, &mut bridge, 240.);
    has_count(&second, "count=12");

    let checkpoint = storage.read("slot/一").unwrap();
    let mut original = Vec::new();
    checkpoint.write_to(&mut original).unwrap();
    let too_small = DirectorySaves::new(&folder, 1);
    assert!(
        too_small
            .write(slots[0].clone(), &checkpoint, None)
            .is_err()
    );
    let mut after = Vec::new();
    storage
        .read("slot/一")
        .unwrap()
        .write_to(&mut after)
        .unwrap();
    assert_eq!(after, original);
    second.close().unwrap();
    wait(&mut second);
    storage.delete("slot/一").unwrap();
    assert!(storage.list().unwrap().is_empty());
    std::fs::remove_dir(&folder).unwrap();
}

fn runtime(source: &str) -> StoryRuntime {
    StoryRuntime::new(
        AsideSessionUi::from_source(
            session(),
            source,
            "buttons",
            1,
            "main",
            "en",
            VmLimits::default(),
        )
        .unwrap(),
    )
}

#[test]
fn initial_interaction_state_tracks_bindings_and_ancestor_disable_from_source_and_asb() {
    for encoded in [false, true] {
        for parent_enabled in [false, true] {
            let source = r##"fn main() {
                let state = model({enabled: false, parent_enabled: false, done: false});
                let panel = stack(enabled: bind(state.parent_enabled));
                let control = panel.add(button(width: 100, height: 40,
                    enabled: bind(state.enabled),
                    on_interaction: |event| {
                        if event.disabled != (!state.enabled || !state.parent_enabled) {
                            throw "interaction published before inherited state settled";
                        }
                    }));
                let status = text(derive(|| if state.done {"complete"}
                    else if control.disabled {"disabled"} else {"enabled"}));
                stage.replace(stack { panel; status; });
                state.enabled = true;
                state.parent_enabled = PARENT_ENABLED;
                await stage.presented();
                if !control.enabled || control.disabled != !PARENT_ENABLED
                    || panel.disabled != !PARENT_ENABLED {
                    throw "initial interaction differs from the mounted state";
                }
                state.parent_enabled = true;
                await stage.presented();
                if control.disabled { throw "enabled ancestor did not update interaction"; }
                state.parent_enabled = false;
                await stage.presented();
                if !control.disabled { throw "disabled ancestor did not update interaction"; }
                state.parent_enabled = true;
                state.enabled = false;
                await stage.presented();
                if !control.disabled { throw "disabled control did not update interaction"; }
                state.enabled = true;
                await stage.presented();
                if control.disabled { throw "enabled control did not update interaction"; }
                state.done = true;
                await completion();
            }"##
            .replace(
                "PARENT_ENABLED",
                if parent_enabled { "true" } else { "false" },
            );
            let mut compiled = reel_engine::aside::source::compile_story(
                &source,
                "initial_interaction",
                1,
                "main",
            )
            .unwrap();
            if encoded {
                use aside_vm::bytecode as bc;
                let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
                compiled.program = bc::verify(
                    bc::decode(&bytes, bc::Limits::default()).unwrap(),
                    bc::Limits::default(),
                )
                .unwrap();
            }
            let mut story = StoryRuntime::new(
                AsideSessionUi::from_compiled(session(), compiled, "en", VmLimits::default())
                    .unwrap(),
            );
            drain(&mut story);
            let (mut bridge, root) = mount(story.driver().ui());
            for frame in 1..=5 {
                for _ in 0..16 {
                    story.driver_mut().unwrap().sync(&mut bridge, root).unwrap();
                    story.wake_input();
                    drain(&mut story);
                    if story.driver().adapter().stage_ready() {
                        break;
                    }
                }
                present(&mut story, &mut bridge, root, frame);
                story.wake_input();
                drain(&mut story);
            }
            assert!(
                story.driver().adapter().stage().nodes().any(
                    |node| matches!(&node.kind, NodeKind::Text(text) if text.as_ref() == "complete")
                ),
                "asb={encoded} parent_enabled={parent_enabled}"
            );
        }
    }
}

#[test]
fn interaction_appearance_preserves_pointer_press_and_tracks_ancestor_disable() {
    let publish = |story: &mut StoryRuntime, bridge: &mut PresentationBridge, root, frame| {
        for _ in 0..16 {
            story.driver_mut().unwrap().sync(bridge, root).unwrap();
            if story.driver().adapter().stage_ready() {
                present(story, bridge, root, frame);
                return;
            }
            story.wake_input();
            drain(story);
        }
        panic!("appearance layout feedback did not settle");
    };
    let mut story = runtime(
        r##"fn main() {
        let data = model({enabled: true, clicks: 0});
        let look = appearance(base: {background: "#102030"}, hover: {background: "#ffffff"},
            pressed: {scale_x: 0.98, color: "#000000"}, disabled: {background: "#303030"});
        stage.replace(stack {
            stack(enabled: bind(data.enabled)) {
                button("Action", width: 100, height: 40, decorated: false, appearance: look,
                    on_click: || {data.clicks += 1;});
            };
            button("Disable", x: 140, width: 100, height: 40, on_click: || {data.enabled = false;});
            text(derive(|| f"clicks:{data.clicks}"), y: 80);
        });
    }"##,
    );
    drain(&mut story);
    let (mut bridge, root) = mount(story.driver().ui());
    publish(&mut story, &mut bridge, root, 1);
    let id = story.driver().adapter().stage().nodes().find(|node|
        matches!(&node.kind, NodeKind::Button { label: Some(label), .. } if label == "Action")).unwrap().id;
    for (index, kind) in ["move", "down", "up"].into_iter().enumerate() {
        story
            .driver_mut()
            .unwrap()
            .handle_pointer(&mut bridge, mouse(kind, (50., 20.)))
            .unwrap();
        story.wake_input();
        drain(&mut story);
        if kind == "down" {
            let props = &story.driver().adapter().stage().node(id).unwrap().props;
            assert_eq!(props.scale_x, 0.98);
            assert_eq!(props.color, [0, 0, 0, 255]);
        }
        publish(&mut story, &mut bridge, root, index as u64 + 2);
    }
    assert!(
        story
            .driver()
            .adapter()
            .stage()
            .nodes()
            .any(|node| matches!(&node.kind, NodeKind::Text(text) if text.as_ref() == "clicks:1"))
    );
    for kind in ["move", "down", "up"] {
        story
            .driver_mut()
            .unwrap()
            .handle_pointer(&mut bridge, mouse(kind, (180., 20.)))
            .unwrap();
    }
    story.wake_input();
    drain(&mut story);
    publish(&mut story, &mut bridge, root, 5);
    story.wake_input();
    drain(&mut story);
    assert_eq!(
        story
            .driver()
            .adapter()
            .stage()
            .node(id)
            .unwrap()
            .props
            .background,
        Some([48, 48, 48, 255])
    );
    for kind in ["move", "down", "up"] {
        story
            .driver_mut()
            .unwrap()
            .handle_pointer(&mut bridge, mouse(kind, (50., 20.)))
            .unwrap();
    }
    story.wake_input();
    drain(&mut story);
    assert!(
        story
            .driver()
            .adapter()
            .stage()
            .nodes()
            .any(|node| matches!(&node.kind, NodeKind::Text(text) if text.as_ref() == "clicks:1"))
    );
}

#[test]
fn radio_group_snapshot_rebuilds_binding_identity_and_restores_selection_without_focus() {
    use reel_engine::aside::{SessionVm, snapshot::SnapshotBudget};
    use reel_engine::{Epoch, SessionId, SessionIdentity};
    let mut story = runtime(
        r#"
        fn main(){
            let state=model({value:0,changes:0});
            let changed=||{state.changes+=1;};
            stage.replace(column{
                radio(0,"zero",value:bind(state.value),height:40,width:120,on_change:changed);
                radio(1,"one",value:bind(state.value),height:40,width:120,on_change:changed);
                text(derive(||f"selection:{state.value}:{state.changes}"));
            });
        }
    "#,
    );
    drain(&mut story);
    let (mut bridge, root) = mount(story.driver().ui());
    present(&mut story, &mut bridge, root, 1);
    let press = |story: &mut StoryRuntime, bridge: &mut PresentationBridge, named| {
        for state in [keyboard::KeyState::Down, keyboard::KeyState::Up] {
            story
                .driver_mut()
                .unwrap()
                .handle_text(bridge, key(state, keyboard::Key::Named(named)))
                .unwrap();
        }
        story.wake_input();
        drain(story);
    };
    let state = |story: &StoryRuntime| {
        story
            .driver()
            .adapter()
            .stage()
            .nodes()
            .find_map(|node| match &node.kind {
                NodeKind::Text(text) if text.as_ref().starts_with("selection:") => {
                    Some(text.to_string())
                }
                _ => None,
            })
            .unwrap()
    };
    press(&mut story, &mut bridge, keyboard::NamedKey::Tab);
    press(&mut story, &mut bridge, keyboard::NamedKey::ArrowDown);
    assert_eq!(state(&story), "selection:1:1");
    let live = story.driver();
    let mut capture = live.snapshot_begin().unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let bytes = capture.finish().unwrap().encode().unwrap();
    let mut image = reel_engine::aside::snapshot::SessionSnapshot::decode(&bytes).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        live.adapter().vm().program().clone(),
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(802),
            epoch: Epoch(2),
        },
        Default::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (session, vm) = restore.finish().unwrap();
    let mut restored = AsideSessionUi::restored(
        session,
        vm,
        "en".into(),
        live.ui().assets(),
        live.initializer(),
        live.entry_function(),
        live.catalogs(),
    )
    .unwrap();
    while !restored
        .prepare_restored(SliceBudget::default(), &mut || 0)
        .unwrap()
    {}
    let mut story = StoryRuntime::new(restored);
    drain(&mut story);
    let (mut bridge, root) = mount(story.driver().ui());
    assert!(bridge.root().focused_widget().is_none());
    present(&mut story, &mut bridge, root, 1);
    press(&mut story, &mut bridge, keyboard::NamedKey::Tab);
    press(&mut story, &mut bridge, keyboard::NamedKey::ArrowDown);
    assert_eq!(
        state(&story),
        "selection:0:2",
        "the restored group selects the next item once"
    );
}
fn drain(runtime: &mut StoryRuntime) {
    for _ in 0..10_000 {
        if !runtime.needs_tick() {
            return;
        }
        runtime
            .tick(
                SliceBudget {
                    instructions: 5,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap();
    }
    panic!("button runtime did not sleep");
}
fn present(
    runtime: &mut StoryRuntime,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    frame: u64,
) {
    runtime.driver_mut().unwrap().sync(bridge, root).unwrap();
    let (recording, stamp) = runtime
        .driver()
        .record(bridge, root, FrameId(frame))
        .unwrap()
        .expect("settled frame");
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            recording.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(frame))
        )),
        NoticeResult::Accepted
    );
    runtime
        .driver_mut()
        .unwrap()
        .presented(bridge, &stamp)
        .unwrap();
}
fn click(runtime: &mut StoryRuntime, bridge: &mut PresentationBridge) {
    for kind in ["move", "down", "up"] {
        runtime
            .driver_mut()
            .unwrap()
            .handle_pointer(bridge, mouse(kind, (50., 30.)))
            .unwrap();
    }
    runtime.wake_input();
    drain(runtime);
}
fn key(state: keyboard::KeyState, key: keyboard::Key) -> TextEvent {
    TextEvent::Keyboard(keyboard::KeyboardEvent {
        state,
        key,
        code: keyboard::Code::Unidentified,
        location: keyboard::Location::Standard,
        modifiers: Default::default(),
        repeat: false,
        is_composing: false,
    })
}
#[test]
fn nested_view_centers_content_and_routes_unfocused_control_keys() {
    let mut runtime = runtime(
        r#"
        let data = model({held: false, clicks: 0});
        fn main() {
            stage.replace(stack(width: 600, height: 400) {
                input(stretch: "both", actions: {skip: {
                    bindings: [binding("ControlLeft")],
                    on_action: |e| { data.held = e.phase == "pressed"; }
                }}) {
                    view(|| button(derive(|| f"{data.held}:{data.clicks}"),
                        width: 100, height: 40, align_x: 0.5, align_y: 0.5,
                        on_click: |e| { data.clicks += 1; }), stretch: "both");
                }
                input(modal: true, visible: false, width: 600, height: 400);
            });
        }
    "#,
    );
    drain(&mut runtime);
    let (mut bridge, root) = mount(runtime.driver().ui());
    present(&mut runtime, &mut bridge, root, 1);
    let mut event = key(
        keyboard::KeyState::Down,
        keyboard::Key::Named(keyboard::NamedKey::Control),
    );
    let TextEvent::Keyboard(ref mut physical) = event else {
        unreachable!()
    };
    physical.code = keyboard::Code::ControlLeft;
    runtime
        .driver_mut()
        .unwrap()
        .handle_text(&mut bridge, event)
        .unwrap();
    runtime.wake_input();
    drain(&mut runtime);
    assert_eq!(
        caption(&runtime),
        "true:0",
        "a stack wrapper must not hide its input scope"
    );
    present(&mut runtime, &mut bridge, root, 2);
    runtime
        .driver_mut()
        .unwrap()
        .handle_text(&mut bridge, TextEvent::WindowFocusChange(false))
        .unwrap();
    runtime.wake_input();
    drain(&mut runtime);
    assert_eq!(
        caption(&runtime),
        "false:0",
        "focus loss releases held actions"
    );
    runtime
        .driver_mut()
        .unwrap()
        .handle_text(&mut bridge, TextEvent::WindowFocusChange(true))
        .unwrap();
    present(&mut runtime, &mut bridge, root, 3);
    for kind in ["move", "down", "up"] {
        runtime
            .driver_mut()
            .unwrap()
            .handle_pointer(&mut bridge, mouse(kind, (300., 200.)))
            .unwrap();
    }
    runtime.wake_input();
    drain(&mut runtime);
    assert_eq!(
        caption(&runtime),
        "false:1",
        "view must preserve vertical centering"
    );
    runtime.close().unwrap();
    drain(&mut runtime);
}

fn caption(runtime: &StoryRuntime) -> String {
    runtime
        .driver()
        .adapter()
        .stage()
        .nodes()
        .find_map(|node| match &node.kind {
            NodeKind::Button { label, .. } => label.clone(),
            _ => None,
        })
        .unwrap()
}

#[test]
#[ignore = "requires REEL_SESSION_FONT, an explicitly supplied TTF/OTF font"]
fn text_input_ime_selection_cancel_submit_and_focus_use_the_real_stage() {
    use keyboard::{Key, KeyState, Modifiers, NamedKey};
    use reel_ui::core::Ime;
    let font =
        std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("REEL_SESSION_FONT")).unwrap();
    let mut runtime = runtime(
        r#"fn main() {
        game.configure(text_speed: 0);
        let data = model({name: "start", changes: 0, submits: 0});
        stage.replace(column() {
            text_input(value: bind(data.name), width: 240, height: 50,
                on_change: |event| { data.changes += 1; },
                on_submit: |event| { data.submits += 1; });
            text(value: derive(|| f"{data.name}:{data.changes}:{data.submits}"));
            button("other", width: 240, height: 50);
        });
        narrator("do not advance");
    }"#,
    );
    // Reveal scheduling is part of the pending narrative; settle it once.
    drain(&mut runtime);
    let (mut bridge, root) =
        super::tests::mount_with_font(runtime.driver().ui(), Some(font.into()));
    let mut frame = 1;
    present(&mut runtime, &mut bridge, root, frame);
    let interaction = runtime
        .driver()
        .ui()
        .session()
        .view()
        .interaction
        .unwrap()
        .id();
    let state = |runtime: &StoryRuntime| {
        runtime
            .driver()
            .adapter()
            .stage()
            .nodes()
            .find_map(|node| match &node.kind {
                NodeKind::Text(value) => Some(value.to_string()),
                _ => None,
            })
            .unwrap()
    };
    let mut send = |runtime: &mut StoryRuntime, bridge: &mut PresentationBridge, event| {
        runtime
            .driver_mut()
            .unwrap()
            .handle_text(bridge, event)
            .unwrap();
        runtime.wake_input();
        // One clock value; no real-time sleeps or native window/GPU dependency.
        drain(runtime);
        assert_eq!(runtime.driver().state(), StoryState::Running);
        frame += 1;
        present(runtime, bridge, root, frame);
    };
    send(
        &mut runtime,
        &mut bridge,
        key(KeyState::Down, Key::Named(NamedKey::Tab)),
    );
    let focus = bridge.root().focused_widget().unwrap();
    let mut all = key(KeyState::Down, Key::Character("a".into()));
    if let TextEvent::Keyboard(event) = &mut all {
        event.modifiers = if cfg!(target_os = "macos") {
            Modifiers::META
        } else {
            Modifiers::CONTROL
        };
    }
    send(&mut runtime, &mut bridge, all.clone());
    send(
        &mut runtime,
        &mut bridge,
        TextEvent::Ime(Ime::Preedit("zhong".into(), Some((5, 5)))),
    );
    assert_eq!(state(&runtime), "start:0:0");
    send(
        &mut runtime,
        &mut bridge,
        key(KeyState::Down, Key::Named(NamedKey::Enter)),
    );
    assert_eq!(
        state(&runtime),
        "start:0:0",
        "IME confirmation must not submit the form"
    );
    send(
        &mut runtime,
        &mut bridge,
        TextEvent::Ime(Ime::Preedit("".into(), None)),
    );
    send(
        &mut runtime,
        &mut bridge,
        TextEvent::Ime(Ime::Commit("中文".into())),
    );
    assert_eq!(state(&runtime), "中文:1:0");
    assert_eq!(
        bridge.root().focused_widget(),
        Some(focus),
        "value echo preserves widget identity"
    );
    send(&mut runtime, &mut bridge, all.clone());
    send(
        &mut runtime,
        &mut bridge,
        TextEvent::Ime(Ime::Preedit("cancel".into(), None)),
    );
    send(
        &mut runtime,
        &mut bridge,
        TextEvent::Ime(Ime::Preedit("".into(), None)),
    );
    send(
        &mut runtime,
        &mut bridge,
        TextEvent::ClipboardPaste("first\r\nsecond".into()),
    );
    assert_eq!(
        state(&runtime),
        "first second:2:0",
        "cancel restores the original selection"
    );
    send(
        &mut runtime,
        &mut bridge,
        key(KeyState::Down, Key::Named(NamedKey::Enter)),
    );
    assert_eq!(state(&runtime), "first second:2:1");
    send(&mut runtime, &mut bridge, all);
    send(
        &mut runtime,
        &mut bridge,
        TextEvent::Ime(Ime::Preedit("discard".into(), Some((7, 7)))),
    );
    send(
        &mut runtime,
        &mut bridge,
        TextEvent::WindowFocusChange(false),
    );
    send(
        &mut runtime,
        &mut bridge,
        TextEvent::WindowFocusChange(true),
    );
    send(
        &mut runtime,
        &mut bridge,
        TextEvent::Ime(Ime::Commit("戻る".into())),
    );
    assert_eq!(state(&runtime), "戻る:3:1");
    let mut alt_graph = key(KeyState::Down, Key::Character("€".into()));
    if let TextEvent::Keyboard(event) = &mut alt_graph {
        event.modifiers = Modifiers::ALT_GRAPH;
    }
    send(&mut runtime, &mut bridge, alt_graph);
    assert_eq!(state(&runtime), "戻る€:4:1");
    send(
        &mut runtime,
        &mut bridge,
        key(KeyState::Down, Key::Named(NamedKey::Backspace)),
    );
    assert_eq!(state(&runtime), "戻る:5:1");
    assert_eq!(
        runtime
            .driver()
            .ui()
            .session()
            .view()
            .interaction
            .unwrap()
            .id(),
        interaction
    );
}

#[test]
fn author_buttons_route_accepted_pointer_and_keyboard_without_desktop_backend() {
    let mut runtime = runtime(
        r#"fn main() {
        let mut n = 0;
        let box = column();
        box.add(button("Ready", width: 180, height: 60, on_click: |event| {
            n += 1;
            if n == 1 { event.target.label = "Pointer"; }
            if n == 2 { event.target.label = "Keyboard"; }
            if n == 3 { event.target.label = "Space"; box.enabled = false; }
            if n > 3 { throw "disabled ancestor accepted a click"; }
        }));
        stage.replace(box);
    }"#,
    );
    drain(&mut runtime);
    assert_eq!(runtime.driver().state(), StoryState::Running);
    let (mut bridge, root) = mount(runtime.driver().ui());
    click(&mut runtime, &mut bridge);
    assert_eq!(caption(&runtime), "Ready", "unpresented input");
    present(&mut runtime, &mut bridge, root, 1);
    runtime
        .driver_mut()
        .unwrap()
        .handle_text(
            &mut bridge,
            key(
                keyboard::KeyState::Down,
                keyboard::Key::Named(keyboard::NamedKey::Tab),
            ),
        )
        .unwrap();
    let focus = bridge
        .root()
        .focused_widget()
        .expect("Tab enters the button");
    // Release without a matching Down must not activate.
    runtime
        .driver_mut()
        .unwrap()
        .handle_pointer(&mut bridge, mouse("up", (50., 30.)))
        .unwrap();
    drain(&mut runtime);
    assert_eq!(caption(&runtime), "Ready");
    click(&mut runtime, &mut bridge);
    assert_eq!(caption(&runtime), "Pointer");
    assert_eq!(bridge.root().focused_widget(), Some(focus));
    runtime
        .driver_mut()
        .unwrap()
        .sync(&mut bridge, root)
        .unwrap();
    click(&mut runtime, &mut bridge);
    assert_eq!(
        caption(&runtime),
        "Pointer",
        "changed projection must be presented"
    );
    present(&mut runtime, &mut bridge, root, 2);
    assert_eq!(
        bridge.root().focused_widget(),
        Some(focus),
        "stable button identity"
    );
    for state in [keyboard::KeyState::Down, keyboard::KeyState::Up] {
        runtime
            .driver_mut()
            .unwrap()
            .handle_text(
                &mut bridge,
                key(state, keyboard::Key::Named(keyboard::NamedKey::Enter)),
            )
            .unwrap();
    }
    runtime.wake_input();
    drain(&mut runtime);
    assert_eq!(caption(&runtime), "Keyboard");
    present(&mut runtime, &mut bridge, root, 3);
    for state in [keyboard::KeyState::Down, keyboard::KeyState::Up] {
        runtime
            .driver_mut()
            .unwrap()
            .handle_text(
                &mut bridge,
                key(state, keyboard::Key::Character(" ".into())),
            )
            .unwrap();
    }
    runtime.wake_input();
    drain(&mut runtime);
    assert_eq!(caption(&runtime), "Space");
    present(&mut runtime, &mut bridge, root, 4);
    click(&mut runtime, &mut bridge);
    assert_eq!(caption(&runtime), "Space");
    assert!(!runtime.needs_tick());
    runtime
        .tick(SliceBudget::default(), || {
            panic!("idle host must not sample VM clock")
        })
        .unwrap();
    runtime.close().unwrap();
    drain(&mut runtime);
    assert!(runtime.close_complete());
}

#[test]
fn control_click_does_not_advance_dialogue_and_callback_fault_keeps_source() {
    let mut runtime = runtime(
        "fn main() {\n    stage.replace(button(\"Fail\", width: 180, height: 60, on_click: |event| {\n        throw \"callback failure\";\n    }));\n    narrator(\"still reading\");\n}",
    );
    drain(&mut runtime);
    let interaction = runtime
        .driver()
        .ui()
        .session()
        .view()
        .interaction
        .unwrap()
        .id();
    let (mut bridge, root) = mount(runtime.driver().ui());
    present(&mut runtime, &mut bridge, root, 1);
    for kind in ["move", "down", "up"] {
        runtime
            .driver_mut()
            .unwrap()
            .handle_pointer(&mut bridge, mouse(kind, (50., 30.)))
            .unwrap();
    }
    assert_eq!(
        runtime
            .driver()
            .ui()
            .session()
            .view()
            .interaction
            .unwrap()
            .id(),
        interaction
    );
    runtime.wake_input();
    let error = loop {
        if let Err(error) = runtime.tick(SliceBudget::default(), || 0) {
            break error.to_string();
        }
        assert!(runtime.needs_tick(), "callback never ran");
    };
    assert!(
        error.contains("buttons:3:9") && error.contains("callback failure"),
        "{error}"
    );
    runtime.close().unwrap();
    drain(&mut runtime);
    assert!(runtime.close_complete());
}

#[test]
fn suspended_button_animation_advances_without_polling_vm_and_closes_its_wait() {
    let mut runtime = runtime(
        r#"fn main() {
        let target = text("moving");
        stage.replace(stack() {
            target;
            button("Animate", width: 180, height: 60, on_click: |event| {
                await tween(target, x: 100, duration: 1).run();
                throw "closed callback must not resume";
            });
        });
    }"#,
    );
    drain(&mut runtime);
    let (mut bridge, root) = mount(runtime.driver().ui());
    present(&mut runtime, &mut bridge, root, 1);
    for kind in ["move", "down", "up"] {
        runtime
            .driver_mut()
            .unwrap()
            .handle_pointer(&mut bridge, mouse(kind, (50., 30.)))
            .unwrap();
    }
    runtime.wake_input();
    for step in 0..10_000 {
        runtime.tick(SliceBudget::default(), || 0).unwrap();
        if runtime.driver().activities_need_time()
            && !runtime.driver().has_runnable_tasks()
            && runtime.driver().adapter().vm().heap().phase() == aside_vm::GcPhase::Idle
        {
            break;
        }
        assert!(step < 9999, "waiting callback did not finish collection");
    }
    let before = runtime.driver().adapter().vm().stats().slices;
    for ns in [100_000_000, 200_000_000, 300_000_000] {
        runtime.tick(SliceBudget::default(), || ns).unwrap();
    }
    assert_eq!(runtime.driver().adapter().vm().stats().slices, before);
    runtime
        .driver_mut()
        .unwrap()
        .sync(&mut bridge, root)
        .unwrap();
    let target = runtime
        .driver()
        .adapter()
        .stage()
        .nodes()
        .find(|node| matches!(&node.kind, NodeKind::Text(value) if value.as_ref() == "moving"))
        .unwrap();
    assert_eq!(target.props.x, 30.);
    runtime.close().unwrap();
    drain(&mut runtime);
    assert!(runtime.close_complete());
}

#[test]
fn concurrent_touch_callbacks_keep_their_own_presented_down_stamp() {
    let mut runtime = runtime(
        r#"fn main() {
        let caption = text("none");
        let left = button("Left", width: 80, height: 50, on_click: |event| { caption.value = "left"; event.target.label = "Pressed"; });
        let right = button("Right", width: 80, height: 50, on_click: |event| { caption.value = "right"; event.target.label = "Pressed"; });
        stage.replace(column() { row() { left; right; }; caption; });
    }"#,
    );
    drain(&mut runtime);
    let (mut bridge, root) = mount(runtime.driver().ui());
    present(&mut runtime, &mut bridge, root, 1);
    let touch = |kind, id, x| {
        let mut e = mouse(kind, (x, 25.));
        let info = PointerInfo {
            pointer_id: PointerId::new(id),
            persistent_device_id: None,
            pointer_type: PointerType::Touch,
        };
        match &mut e {
            PointerEvent::Down(e) | PointerEvent::Up(e) => {
                e.pointer = info;
                e.button = None;
            }
            PointerEvent::Cancel(p) => *p = info,
            _ => unreachable!(),
        }
        e
    };
    // Cancelling the second touch must leave the first touch's stamp intact.
    for e in [
        touch("down", 2, 25.),
        touch("down", 3, 125.),
        touch("cancel", 3, 125.),
        touch("up", 2, 25.),
    ] {
        runtime
            .driver_mut()
            .unwrap()
            .handle_pointer(&mut bridge, e)
            .unwrap();
    }
    runtime.wake_input();
    drain(&mut runtime);
    assert!(
        runtime
            .driver()
            .adapter()
            .stage()
            .nodes()
            .any(|n| n.kind == NodeKind::Text("left".into()))
    );
    present(&mut runtime, &mut bridge, root, 2);
    for e in [
        touch("down", 2, 25.),
        touch("down", 3, 125.),
        touch("up", 2, 25.),
        touch("up", 3, 125.),
    ] {
        runtime
            .driver_mut()
            .unwrap()
            .handle_pointer(&mut bridge, e)
            .unwrap();
    }
    runtime.wake_input();
    drain(&mut runtime);
    assert_eq!(runtime.driver().adapter().stage().nodes().filter(|n| matches!(&n.kind, NodeKind::Button { label: Some(label), .. } if label == "Pressed")).count(), 2);
}

#[test]
fn control_acceptance_tracks_its_content_and_cancels_replaced_press_pairs() {
    use reel_ui::stage::{NodeEvent, NodeProps, Stage};
    let mut stage = Stage::default();
    let label = stage
        .create(NodeKind::Text("first".into()), NodeProps::default(), vec![])
        .unwrap();
    let control = stage
        .create(
            NodeKind::Button {
                label: None,
                listening: true,
                focus_on_pointer: true,
                focusable: true,
            },
            NodeProps {
                width: Some(180.),
                height: Some(60.),
                ..Default::default()
            },
            vec![label],
        )
        .unwrap();
    let other = stage
        .create(
            NodeKind::Text("background".into()),
            NodeProps {
                x: 250.,
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let root = stage
        .create(NodeKind::Stack, NodeProps::default(), vec![control, other])
        .unwrap();
    stage.replace(root).unwrap();
    let mut ui = SessionUi::new(session()).unwrap();
    ui.set_stage(&stage);
    let (mut bridge, root) = mount(&ui);
    ui.sync(&mut bridge, root).unwrap();
    super::tests::accept(&mut ui, &mut bridge, root, 1);
    let clicks = |dispatch: UiDispatch| {
        dispatch
            .events
            .into_iter()
            .filter(|event| matches!(event, NodeEvent::Click(_)))
            .count()
    };
    let edit = |stage: &mut Stage, id, text: &str| {
        let props = stage.node(id).unwrap().props.clone();
        stage
            .update(id, NodeKind::Text(text.into()), props)
            .unwrap();
    };
    let mut frame = 1;
    for keyboard in [false, true] {
        let send = |ui: &mut SessionUi, bridge: &mut PresentationBridge, down| {
            if keyboard {
                ui.handle_text(
                    bridge,
                    key(
                        if down {
                            keyboard::KeyState::Down
                        } else {
                            keyboard::KeyState::Up
                        },
                        keyboard::Key::Named(keyboard::NamedKey::Enter),
                    ),
                )
                .unwrap()
            } else {
                ui.handle_pointer(bridge, mouse(if down { "down" } else { "up" }, (50., 30.)))
                    .unwrap()
            }
        };
        for published in [false, true] {
            send(&mut ui, &mut bridge, true);
            edit(
                &mut stage,
                other,
                &format!("animation {keyboard} {published}"),
            );
            ui.set_stage(&stage);
            ui.sync(&mut bridge, root).unwrap();
            if published {
                frame += 1;
                super::tests::accept(&mut ui, &mut bridge, root, frame);
            }
            assert_eq!(
                clicks(send(&mut ui, &mut bridge, false)),
                1,
                "unrelated animation must retain the press"
            );
        }
        send(&mut ui, &mut bridge, true);
        edit(&mut stage, label, &format!("new choice {keyboard}"));
        ui.set_stage(&stage);
        // Feedback can arrive before the queued semantic update is applied.
        let mut props = stage.node(control).unwrap().props.clone();
        props.color = [255, 0, 0, if keyboard { 255 } else { 128 }];
        stage
            .update(control, stage.node(control).unwrap().kind.clone(), props)
            .unwrap();
        ui.set_feedback_stage(&stage);
        ui.sync(&mut bridge, root).unwrap();
        assert_eq!(clicks(send(&mut ui, &mut bridge, false)), 0);
        // Pressing an unpresented replacement cannot turn into a valid release
        // just because a frame arrives while the button is held.
        send(&mut ui, &mut bridge, true);
        frame += 1;
        super::tests::accept(&mut ui, &mut bridge, root, frame);
        assert_eq!(
            clicks(send(&mut ui, &mut bridge, false)),
            0,
            "press began before replacement was presented"
        );
        send(&mut ui, &mut bridge, true);
        assert_eq!(clicks(send(&mut ui, &mut bridge, false)), 1);
        send(&mut ui, &mut bridge, true);
        edit(&mut stage, label, &format!("replacement {keyboard}"));
        ui.set_stage(&stage);
        ui.sync(&mut bridge, root).unwrap();
        frame += 1;
        super::tests::accept(&mut ui, &mut bridge, root, frame);
        assert_eq!(
            clicks(send(&mut ui, &mut bridge, false)),
            0,
            "newly presented content cancels the old press"
        );
    }
}

#[test]
fn state_images_keep_press_origin_across_callbacks_and_delayed_resources() {
    use reel_core::{
        budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
        identity::{AssetKey, ContentVersion, ResourceVersion},
        test_support::{MemoryAsset, MemoryAssetIo},
    };
    use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};
    const PNG: &[u8] = include_bytes!("../../../../examples/assets/panels.png");
    for encoded in [false, true] {
        for replace_content in [false, true] {
            let mut host = None;
            let assets = crate::assets::Assets::with_adapters(Default::default(), |wake| {
                let limits = QueueLimits {
                    max_requests: 8,
                    max_bytes: 768 << 20,
                };
                let storage = BudgetPool::new(BudgetCategory::CpuInput, limits);
                let mut io = MemoryAssetIo::new(limits, limits, wake.clone());
                for (index, name) in ["off.png", "over.png", "down.png"].into_iter().enumerate() {
                    io.insert_asset(
                        ResourceVersion {
                            key: AssetKey(name.into()),
                            version: ContentVersion([index as u8; 32]),
                        },
                        MemoryAsset {
                            bytes: OwnedBytes::new(
                                PNG.to_vec(),
                                storage.reserve(PNG.len() as u64).unwrap(),
                            )
                            .unwrap(),
                            media_type: None,
                        },
                    )
                    .unwrap();
                }
                let io = Rc::new(RefCell::new(io));
                host = Some(io.clone());
                (
                    Box::new(crate::images::tests::ControlledIo(io)),
                    Box::new(crate::assets::ImageExecutor::new(limits, wake)),
                )
            });
            let source = r#"fn main() {
                let state=model({hover:false,calls:0});
                let caption=text("first",y:40);
                let control=button(width:180,height:60,decorated:false,padding:0,
                    on_click:||{state.calls+=1;});
                control.on_interaction=||{
                    state.hover=control.hover;
                    if REPLACE && control.pressed {caption.value="replacement";}
                };
                control.add(image(source:derive(||if control.pressed {"down.png"}
                    else if control.hover {"over.png"} else {"off.png"}),
                    width:180,height:40));
                control.add(caption);
                stage.replace(stack{control;text(derive(||f"calls:{state.calls}"),y:100);});
                await completion();
            }"#
            .replace("REPLACE", if replace_content { "true" } else { "false" });
            let mut compiled =
                reel_engine::aside::source::compile_story(&source, "state_images", 1, "main")
                    .unwrap();
            if encoded {
                use aside_vm::bytecode as bc;
                let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
                compiled.program = bc::verify(
                    bc::decode(&bytes, bc::Limits::default()).unwrap(),
                    bc::Limits::default(),
                )
                .unwrap();
            }
            let driver =
                AsideSessionUi::from_compiled(session(), compiled, "en", VmLimits::default())
                    .unwrap()
                    .with_assets(assets);
            let mut runtime = StoryRuntime::new(driver);
            let host = host.unwrap();
            let (send, receive) = std::sync::mpsc::channel();
            runtime
                .driver_mut()
                .unwrap()
                .set_resource_waker(Arc::new(move || {
                    let _ = send.send(());
                }));
            drain(&mut runtime);
            let (mut bridge, root) = mount(runtime.driver().ui());
            let finish_images = |runtime: &mut StoryRuntime, bridge: &mut PresentationBridge| {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                loop {
                    while host.borrow_mut().complete_next_read() {}
                    runtime.driver_mut().unwrap().sync(bridge, root).unwrap();
                    runtime.wake_input();
                    drain(runtime);
                    runtime.driver_mut().unwrap().sync(bridge, root).unwrap();
                    if !runtime.driver().ui().images_pending()
                        && runtime.driver().adapter().stage_ready()
                    {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "state image did not load: queued={:?}, state={:?}, ready={}, source={:?}",
                        host.borrow().read_usage(),
                        runtime.driver().state(),
                        runtime.driver().adapter().stage_ready(),
                        runtime.driver().adapter().stage().nodes().find_map(|node| {
                            if let NodeKind::Image { source, .. } = &node.kind {
                                Some(source)
                            } else {
                                None
                            }
                        })
                    );
                    let _ = receive.recv_timeout(Duration::from_millis(1));
                }
            };
            finish_images(&mut runtime, &mut bridge);
            present(&mut runtime, &mut bridge, root, 1);
            for (index, kind) in ["move", "down"].into_iter().enumerate() {
                runtime
                    .driver_mut()
                    .unwrap()
                    .handle_pointer(&mut bridge, mouse(kind, (50., 20.)))
                    .unwrap();
                runtime.wake_input();
                // Finish both derivations and ordinary callbacks before publishing.
                drain(&mut runtime);
                runtime
                    .driver_mut()
                    .unwrap()
                    .sync(&mut bridge, root)
                    .unwrap();
                assert!(
                    runtime.driver().ui().images_pending(),
                    "withhold the new state's IO"
                );
                assert!(
                    runtime
                        .driver()
                        .record(&mut bridge, root, FrameId(9))
                        .unwrap()
                        .is_none()
                );
                runtime
                    .driver_mut()
                    .unwrap()
                    .sync(&mut bridge, root)
                    .unwrap();
                finish_images(&mut runtime, &mut bridge);
                present(&mut runtime, &mut bridge, root, index as u64 + 2);
            }
            runtime
                .driver_mut()
                .unwrap()
                .handle_pointer(&mut bridge, mouse("up", (50., 20.)))
                .unwrap();
            runtime.wake_input();
            drain(&mut runtime);
            let expected = if replace_content {
                "calls:0"
            } else {
                "calls:1"
            };
            assert!(
                runtime.driver().adapter().stage().nodes().any(
                    |node| matches!(&node.kind,NodeKind::Text(text) if text.as_ref()==expected)
                ),
                "asb={encoded} replacement={replace_content}: release must follow its accepted content"
            );
        }
    }
}

#[test]
fn pointer_use_hides_navigation_focus_without_losing_keyboard_activation() {
    let mut runtime = runtime(
        r#"fn main() {
        let first = button("first", width: 180, height: 60, on_click: |e| {});
        let second = button("second", width: 180, height: 60, on_click: |e| {});
        stage.replace(column {
            first; second;
            text(derive(|| f"{first.focused}:{first.focus_visible}:{first.hover}:{second.hover}"));
        });
    }"#,
    );
    drain(&mut runtime);
    let (mut bridge, root) = mount(runtime.driver().ui());
    present(&mut runtime, &mut bridge, root, 1);
    let state = |runtime: &StoryRuntime| {
        runtime
            .driver()
            .adapter()
            .stage()
            .nodes()
            .find_map(|node| {
                if let NodeKind::Text(text) = &node.kind {
                    Some(text.to_string())
                } else {
                    None
                }
            })
            .unwrap()
    };
    click(&mut runtime, &mut bridge);
    assert_eq!(state(&runtime), "true:false:true:false");
    runtime
        .driver_mut()
        .unwrap()
        .handle_text(
            &mut bridge,
            key(
                keyboard::KeyState::Down,
                keyboard::Key::Named(keyboard::NamedKey::Enter),
            ),
        )
        .unwrap();
    runtime.wake_input();
    drain(&mut runtime);
    assert_eq!(state(&runtime), "true:true:true:false");
    present(&mut runtime, &mut bridge, root, 2);
    assert_eq!(
        state(&runtime),
        "true:true:true:false",
        "geometry refresh does not change modality"
    );
    runtime
        .driver_mut()
        .unwrap()
        .handle_pointer(&mut bridge, mouse("move", (50., 90.)))
        .unwrap();
    runtime.wake_input();
    drain(&mut runtime);
    assert_eq!(
        state(&runtime),
        "true:false:false:true",
        "only the hovered second button is highlighted"
    );
    runtime
        .driver_mut()
        .unwrap()
        .handle_text(&mut bridge, TextEvent::WindowFocusChange(false))
        .unwrap();
    runtime.wake_input();
    drain(&mut runtime);
    assert_eq!(state(&runtime), "false:false:false:false");
}

#[test]
fn virtual_images_cannot_settle_new_geometry_or_presentation_waits() {
    for wait in ["stage.geometry(viewport)", "stage.presented()"] {
        let source = r#"fn main() {
            let state=model({pending:false,message:"waiting"});
            let values=[];for i in 0..100 {values.append(i);}
            let rows=list(||values,key:|i|i,virtual:true,build:|row|column(height:40){
                image("withheld.png",width:40,height:40,visible:derive(||state.pending && row.value==3));
            });
            let viewport=scroll(width:200,height:100){rows};
            stage.replace(stack{
                viewport;
                button(x:220,width:50,height:40,on_click:||{state.pending=true;await WAIT;state.message="done";});
                button(x:280,width:50,height:40,on_click:||{state.pending=false;});
                text(bind(state.message),y:200);
            });
        }"#.replace("WAIT",wait);
        let mut runtime = runtime(&source);
        runtime
            .driver_mut()
            .unwrap()
            .ui_mut()
            .set_assets(crate::assets::Assets::with_adapters(
                Default::default(),
                |wake| {
                    let limits = reel_core::budget::QueueLimits {
                        max_requests: 32,
                        max_bytes: 768 << 20,
                    };
                    (
                        Box::new(reel_core::test_support::MemoryAssetIo::new(
                            limits,
                            limits,
                            wake.clone(),
                        )),
                        Box::new(crate::assets::ImageExecutor::new(limits, wake)),
                    )
                },
            ));
        let (mut bridge, root) = mount(runtime.driver().ui());
        let mut serial = 0;
        let draw =
            |runtime: &mut StoryRuntime, bridge: &mut PresentationBridge, serial: &mut u64| {
                runtime.driver_mut().unwrap().sync(bridge, root).unwrap();
                *serial += 1;
                let (source, receipt) = runtime
                    .driver()
                    .record(bridge, root, FrameId(*serial))
                    .unwrap()?;
                assert_eq!(
                    bridge.resolve(PresentationNotice::new(
                        source.key(),
                        SurfaceEpoch(1),
                        PresentationOutcome::Accepted(FrameId(*serial))
                    )),
                    NoticeResult::Accepted
                );
                runtime
                    .driver_mut()
                    .unwrap()
                    .presented(bridge, &receipt)
                    .unwrap();
                Some(receipt)
            };
        for _ in 0..12 {
            runtime.wake_input();
            drain(&mut runtime);
            draw(&mut runtime, &mut bridge, &mut serial);
        }
        // The 25px overscan after this 100px viewport includes row 3 at y=120.
        // Its missing image must block fresh receipts while visible rows remain drawable.
        let buffered_row = runtime
            .driver()
            .adapter()
            .stage()
            .nodes()
            .find_map(|node| match &node.kind {
                NodeKind::VirtualList(list) => list.rows.iter().find(|row| row.index == 3).copied(),
                _ => None,
            })
            .expect("the image row is mounted in the virtual buffer");
        assert!(buffered_row.offset >= 100. && buffered_row.offset < 125.);
        let activate = |runtime: &mut StoryRuntime, bridge: &mut PresentationBridge, x| {
            for kind in ["move", "down", "up"] {
                runtime
                    .driver_mut()
                    .unwrap()
                    .handle_pointer(bridge, mouse(kind, (x, 20.)))
                    .unwrap();
            }
            runtime.wake_input();
            drain(runtime);
        };
        let state = |runtime: &StoryRuntime| {
            runtime
                .driver()
                .adapter()
                .stage()
                .nodes()
                .find_map(|n| {
                    if let NodeKind::Text(t) = &n.kind {
                        Some(t.to_string())
                    } else {
                        None
                    }
                })
                .unwrap()
        };
        activate(&mut runtime, &mut bridge, 240.);
        let mut retained = 0;
        for _ in 0..12 {
            if let Some(receipt) = draw(&mut runtime, &mut bridge, &mut serial) {
                assert!(!receipt.current, "{wait}");
                assert!(receipt.geometry.is_none());
                retained += 1;
            }
            runtime.wake_input();
            drain(&mut runtime);
            assert_eq!(state(&runtime), "waiting", "{wait}");
        }
        assert!(
            retained > 0,
            "a complete visible window must remain presentable"
        );
        assert!(runtime.driver().ui().images_pending());
        activate(&mut runtime, &mut bridge, 300.);
        for _ in 0..12 {
            draw(&mut runtime, &mut bridge, &mut serial);
            runtime.wake_input();
            drain(&mut runtime);
        }
        assert_eq!(state(&runtime), "done", "{wait}");
    }
}

#[test]
fn scene_presentation_wait_includes_image_io_and_rejects_old_or_unaccepted_frames() {
    for prepared in [false, true] {
        let generated = prepared.then(|| super::prepared_image_tests::fixture().6);
        let story = r#"fn main() {
        let data = model({message: "waiting"});
        let picture=image("assets/panels.png");
        stage.replace(stack {
            picture;
            image("ordinary-source.png", x: 600, y: 400, width: 1, height: 1);
            image("ordinary-source.png", x: 602, y: 400, width: 1, height: 1);
            text(bind(data.message));
            button("keep scene interactive", on_click: |event| {});
        });
        let geometry=await stage.geometry(picture);
        if geometry.width<=0 || geometry.height<=0 {throw "geometry before image preparation";}
        data.message = "first";
        await stage.presented();
        data.message = "done";
    }"#;
        let story = story.replace(
            "assets/panels.png",
            if prepared {
                "long.reel-image"
            } else {
                "assets/panels.png"
            },
        );
        let story = story.replace(
            "ordinary-source.png",
            if prepared {
                "long.reel-image.tiles/0/0-0.png"
            } else {
                "assets/panels.png"
            },
        );
        let mut runtime = runtime(&story);
        runtime
            .driver_mut()
            .unwrap()
            .set_asset_options(crate::assets::AssetOptions {
                root: generated.unwrap_or_else(|| {
                    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples")
                }),
                ..Default::default()
            });
        let (send, receive) = std::sync::mpsc::channel();
        runtime
            .driver_mut()
            .unwrap()
            .set_resource_waker(std::sync::Arc::new(move || {
                let _ = send.send(());
            }));
        let (mut bridge, root) = mount(runtime.driver().ui());
        runtime
            .driver_mut()
            .unwrap()
            .sync(&mut bridge, root)
            .unwrap();
        let (recording, old) = runtime
            .driver()
            .record(&mut bridge, root, FrameId(1))
            .unwrap()
            .expect("prepared initial frame");
        assert_eq!(
            bridge.resolve(PresentationNotice::new(
                recording.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(FrameId(1)),
            )),
            NoticeResult::Accepted
        );
        runtime
            .driver_mut()
            .unwrap()
            .presented(&bridge, &old)
            .unwrap();
        drain(&mut runtime);
        let state = |runtime: &StoryRuntime| {
            runtime
                .driver()
                .adapter()
                .stage()
                .nodes()
                .find_map(|node| {
                    if let NodeKind::Text(text) = &node.kind {
                        Some(text.to_string())
                    } else {
                        None
                    }
                })
                .unwrap()
        };
        runtime
            .driver_mut()
            .unwrap()
            .sync(&mut bridge, root)
            .unwrap();
        assert!(runtime.driver().ui().images_pending());
        assert!(
            runtime
                .driver()
                .record(&mut bridge, root, FrameId(2))
                .unwrap()
                .is_none(),
            "pending images retain the accepted frame without issuing a new receipt"
        );
        runtime
            .driver_mut()
            .unwrap()
            .presented(&bridge, &old)
            .unwrap();
        runtime.wake_input();
        drain(&mut runtime);
        assert_eq!(
            state(&runtime),
            "waiting",
            "the retained frame has no loaded image"
        );
        while runtime.driver().ui().images_pending() {
            receive
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("image completion wakes the host");
            runtime
                .driver_mut()
                .unwrap()
                .sync(&mut bridge, root)
                .unwrap();
        }
        let resources = runtime.driver().ui().image_resources();
        let handles: std::collections::BTreeSet<_> =
            resources.iter().map(|lease| lease.handle()).collect();
        assert_eq!(
            handles.len(),
            resources.len(),
            "mixed ordinary/prepared images freeze shared resources once"
        );
        let rejected = bridge.record(FrameId(2)).unwrap();
        bridge.resolve(PresentationNotice::new(
            rejected.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Error,
        ));
        runtime.wake_input();
        drain(&mut runtime);
        assert_eq!(
            state(&runtime),
            "waiting",
            "recording and rejecting do not acknowledge presentation"
        );
        present(&mut runtime, &mut bridge, root, 3);
        let (recording, first) = runtime
            .driver()
            .record(&mut bridge, root, FrameId(4))
            .unwrap()
            .expect("settled frame");
        bridge.resolve(PresentationNotice::new(
            recording.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(4)),
        ));
        assert_eq!(first.stamp.presentation_request, 1);
        runtime.wake_input();
        drain(&mut runtime);
        runtime
            .driver_mut()
            .unwrap()
            .sync(&mut bridge, root)
            .unwrap();
        assert_eq!(state(&runtime), "first");
        runtime
            .driver_mut()
            .unwrap()
            .presented(&bridge, &first)
            .unwrap();
        runtime.wake_input();
        drain(&mut runtime);
        assert_eq!(
            state(&runtime),
            "first",
            "repainting the accepted old frame cannot settle the next wait"
        );
        present(&mut runtime, &mut bridge, root, 5);
        runtime.wake_input();
        drain(&mut runtime);
        runtime
            .driver_mut()
            .unwrap()
            .sync(&mut bridge, root)
            .unwrap();
        assert_eq!(state(&runtime), "done");
        runtime.close().unwrap();
        drain(&mut runtime);
        assert!(runtime.close_complete());
    }
}

#[test]
fn idle_manual_advance_publishes_request_and_wakes_author_without_animation() {
    let mut runtime = runtime(
        r#"fn main(){stage.replace(text("before"));narrator("A",wait:false);await when(||game.dialogue().advance_requested);stage.replace(text("done"));await completion();}"#,
    );
    drain(&mut runtime);
    let session = runtime.driver_mut().unwrap().ui_mut().session_mut();
    let id = session.view().interaction.unwrap().id();
    session
        .advance_reveal(
            session.root_owner(),
            id,
            std::time::Duration::from_secs(1),
            1000.,
        )
        .unwrap();
    let (mut bridge, root) = mount(runtime.driver().ui());
    present(&mut runtime, &mut bridge, root, 1);
    assert!(
        !runtime.needs_tick(),
        "ready open speech must sleep without animations"
    );
    for state in [keyboard::KeyState::Down, keyboard::KeyState::Up] {
        let mut event = key(state, keyboard::Key::Named(keyboard::NamedKey::Enter));
        if let TextEvent::Keyboard(key) = &mut event {
            key.code = keyboard::Code::Enter;
        }
        runtime
            .driver_mut()
            .unwrap()
            .handle_text(&mut bridge, event)
            .unwrap();
    }
    assert!(
        matches!(runtime.driver().ui().session().view().interaction,Some(reel_engine::InteractionView::Dialogue(d)) if d.advance_requested()),
        "manual Enter not accepted"
    );
    runtime.wake_input();
    assert!(
        runtime.needs_tick(),
        "accepted manual request must wake observer before a new frame"
    );
    drain(&mut runtime);
    assert!(
        runtime
            .driver()
            .adapter()
            .stage()
            .nodes()
            .any(|n| matches!(&n.kind,NodeKind::Text(t) if t.as_ref()=="done"))
    );
}
