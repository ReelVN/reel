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
    runtime.driver.sync(bridge, root).unwrap();
    let recording = bridge.record(FrameId(frame)).unwrap();
    let stamp = runtime.driver.projection_stamp();
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            recording.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(frame))
        )),
        NoticeResult::Accepted
    );
    runtime.driver.presented(stamp).unwrap();
}
fn click(runtime: &mut StoryRuntime, bridge: &mut PresentationBridge) {
    for kind in ["move", "down", "up"] {
        runtime
            .driver
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
fn caption(runtime: &StoryRuntime) -> String {
    runtime
        .driver
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
    let (mut bridge, root) = super::tests::mount_with_font(runtime.driver.ui(), Some(font.into()));
    let mut frame = 1;
    present(&mut runtime, &mut bridge, root, frame);
    let interaction = runtime
        .driver
        .ui()
        .session()
        .view()
        .interaction
        .unwrap()
        .id();
    let state = |runtime: &StoryRuntime| {
        runtime
            .driver
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
        runtime.driver.handle_text(bridge, event).unwrap();
        runtime.wake_input();
        // One clock value; no real-time sleeps or native window/GPU dependency.
        drain(runtime);
        assert_eq!(runtime.driver.state(), StoryState::Running);
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
            .driver
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
    assert_eq!(runtime.driver.state(), StoryState::Running);
    let (mut bridge, root) = mount(runtime.driver.ui());
    click(&mut runtime, &mut bridge);
    assert_eq!(caption(&runtime), "Ready", "unpresented input");
    present(&mut runtime, &mut bridge, root, 1);
    runtime
        .driver
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
        .driver
        .handle_pointer(&mut bridge, mouse("up", (50., 30.)))
        .unwrap();
    drain(&mut runtime);
    assert_eq!(caption(&runtime), "Ready");
    click(&mut runtime, &mut bridge);
    assert_eq!(caption(&runtime), "Pointer");
    assert_eq!(bridge.root().focused_widget(), Some(focus));
    runtime.driver.sync(&mut bridge, root).unwrap();
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
            .driver
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
            .driver
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
        .driver
        .ui()
        .session()
        .view()
        .interaction
        .unwrap()
        .id();
    let (mut bridge, root) = mount(runtime.driver.ui());
    present(&mut runtime, &mut bridge, root, 1);
    for kind in ["move", "down", "up"] {
        runtime
            .driver
            .handle_pointer(&mut bridge, mouse(kind, (50., 30.)))
            .unwrap();
    }
    assert_eq!(
        runtime
            .driver
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
                await run(tween(target, x: 100, duration: 1));
                throw "closed callback must not resume";
            });
        });
    }"#,
    );
    drain(&mut runtime);
    let (mut bridge, root) = mount(runtime.driver.ui());
    present(&mut runtime, &mut bridge, root, 1);
    for kind in ["move", "down", "up"] {
        runtime
            .driver
            .handle_pointer(&mut bridge, mouse(kind, (50., 30.)))
            .unwrap();
    }
    runtime.wake_input();
    for step in 0..10_000 {
        runtime.tick(SliceBudget::default(), || 0).unwrap();
        if runtime.driver.activities_need_time()
            && !runtime.driver.has_runnable_tasks()
            && runtime.driver.adapter().vm().heap().phase() == aside_vm::GcPhase::Idle
        {
            break;
        }
        assert!(step < 9999, "waiting callback did not finish collection");
    }
    let before = runtime.driver.adapter().vm().stats().slices;
    for ns in [100_000_000, 200_000_000, 300_000_000] {
        runtime.tick(SliceBudget::default(), || ns).unwrap();
    }
    assert_eq!(runtime.driver.adapter().vm().stats().slices, before);
    runtime.driver.sync(&mut bridge, root).unwrap();
    let target = runtime
        .driver
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
    let (mut bridge, root) = mount(runtime.driver.ui());
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
        runtime.driver.handle_pointer(&mut bridge, e).unwrap();
    }
    runtime.wake_input();
    drain(&mut runtime);
    assert!(
        runtime
            .driver
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
        runtime.driver.handle_pointer(&mut bridge, e).unwrap();
    }
    runtime.wake_input();
    drain(&mut runtime);
    assert_eq!(runtime.driver.adapter().stage().nodes().filter(|n| matches!(&n.kind, NodeKind::Button { label: Some(label), .. } if label == "Pressed")).count(), 2);
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
    super::tests::accept(&mut ui, &mut bridge, 1);
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
                super::tests::accept(&mut ui, &mut bridge, frame);
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
        super::tests::accept(&mut ui, &mut bridge, frame);
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
        super::tests::accept(&mut ui, &mut bridge, frame);
        assert_eq!(
            clicks(send(&mut ui, &mut bridge, false)),
            0,
            "newly presented content cancels the old press"
        );
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
    let (mut bridge, root) = mount(runtime.driver.ui());
    present(&mut runtime, &mut bridge, root, 1);
    let state = |runtime: &StoryRuntime| {
        runtime
            .driver
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
        .driver
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
        .driver
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
        .driver
        .handle_text(&mut bridge, TextEvent::WindowFocusChange(false))
        .unwrap();
    runtime.wake_input();
    drain(&mut runtime);
    assert_eq!(state(&runtime), "false:false:false:false");
}

#[test]
fn scene_presentation_wait_includes_image_io_and_rejects_old_or_unaccepted_frames() {
    let mut runtime = runtime(
        r#"fn main() {
        let data = model({message: "waiting"});
        stage.replace(stack {
            image("assets/panels.png");
            text(bind(data.message));
            button("keep scene interactive", on_click: |event| {});
        });
        await stage.presented();
        data.message = "first";
        await stage.presented();
        data.message = "done";
    }"#,
    );
    runtime.driver = runtime
        .driver
        .with_asset_options(crate::assets::AssetOptions {
            root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
            ..Default::default()
        });
    let (send, receive) = std::sync::mpsc::channel();
    runtime
        .driver
        .set_resource_waker(std::sync::Arc::new(move || {
            let _ = send.send(());
        }));
    drain(&mut runtime);
    let state = |runtime: &StoryRuntime| {
        runtime
            .driver
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
    let (mut bridge, root) = mount(runtime.driver.ui());
    runtime.driver.sync(&mut bridge, root).unwrap();
    assert!(runtime.driver.ui().images_pending());
    let old = runtime.driver.projection_stamp();
    super::tests::accept(runtime.driver.ui_mut(), &mut bridge, 1);
    runtime.driver.presented(old).unwrap();
    runtime.wake_input();
    drain(&mut runtime);
    assert_eq!(
        state(&runtime),
        "waiting",
        "the retained frame has no loaded image"
    );
    while runtime.driver.ui().images_pending() {
        receive
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("image completion wakes the host");
        runtime.driver.sync(&mut bridge, root).unwrap();
    }
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
    let first = runtime.driver.projection_stamp();
    assert_eq!(first.presentation_request, 1);
    runtime.wake_input();
    drain(&mut runtime);
    runtime.driver.sync(&mut bridge, root).unwrap();
    assert_eq!(state(&runtime), "first");
    runtime.driver.presented(first).unwrap();
    runtime.wake_input();
    drain(&mut runtime);
    assert_eq!(
        state(&runtime),
        "first",
        "repainting the accepted old frame cannot settle the next wait"
    );
    present(&mut runtime, &mut bridge, root, 4);
    runtime.wake_input();
    drain(&mut runtime);
    runtime.driver.sync(&mut bridge, root).unwrap();
    assert_eq!(state(&runtime), "done");
    runtime.close().unwrap();
    drain(&mut runtime);
    assert!(runtime.close_complete());
}
