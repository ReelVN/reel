//! Source and ASB execute the same interactive scene without a window/GPU adapter.
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{SliceBudget, VmLimits, source::CompiledStory},
};
use reel_player::aside_session::{AsideSessionUi, StoryState};
use reel_ui::{bridge::PresentationBridge, core::*, dpi::PhysicalPosition, stage::NodeKind};

#[path = "support/author.rs"]
mod support;
use support::*;

const SOURCE: &str = include_str!("../../../examples/author_buttons/src/main.aside");

#[test]
fn conditional_table_tails_configure_input_from_source_and_asb() {
    let source = r#"fn main() {
        for flag in [true, false] {
            let actions=if flag { {accept: {bindings: ["Enter"]}} }
                else { {back: {bindings: ["Escape"]}} };
            if actions.has_own("accept") != flag || actions.has_own("back") == flag {
                throw "wrong conditional input table";
            }
            stage.replace(input(actions: actions) {
                column { text("Conditional input"); }
            });
        }
        stage.replace(stack());
    }"#;
    let checked = reel_cli::check(source, "conditional-input.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_cli::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        assert_eq!(driver.state(), StoryState::Finished);
    }
}

#[test]
fn source_and_asb_retire_a_batch_of_live_completion_owners() {
    let source = r#"fn main() {
        let owner=stack(); stage.replace(owner);
        let requests=[];
        for i in 0..1024 {requests.append(completion(owner:owner));}
        for request in requests {
            if !request.pending {throw "live result ended";}
        }
        owner.remove();
        for request in requests {
            if request.pending {throw "retired owner retained result";}
        }
    }"#;
    let checked = reel_cli::check(source, "completion-batch.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_engine::aside::source::compile_story(source, "main", 1, "main").unwrap(),
        reel_cli::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        assert_eq!(driver.state(), StoryState::Finished);
    }
}

#[test]
fn bound_class_method_keeps_its_receiver_after_entry_returns() {
    let source = r#"
    class Counter {
        let mut count: int = 0;
        let caption;
        fn init(self, caption) { self.caption = caption; }
        fn click(self, event) {
            self.count += 1;
            self.caption.value = f"{self.count}";
        }
    }
    fn main() {
        let caption = text("0", y: 150);
        let counter = Counter(caption);
        stage.replace(stack(width: 640, height: 420) {
            caption;
            button("Count", x: 20, y: 20, width: 180, height: 60,
                on_click: counter.click);
        });
    }"#;
    let checked = reel_cli::check(source, "class-callback.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(source, "main", 1, "main").unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        assert_eq!(driver.state(), StoryState::Running);
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        for count in 1..=2 {
            click_at(&mut driver, &mut bridge, 50., 50.);
            present(&mut driver, &mut bridge, root, count + 1);
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|node| { node.kind == NodeKind::Text(count.to_string().into()) })
            );
        }
    }
}

#[test]
#[ignore = "requires REEL_SESSION_FONT for real inline link hit geometry"]
fn rich_links_consume_presented_mouse_touch_and_gamepad_input_without_advancing_story() {
    let source = r#"
    let d = game.dialogue(); let state = model({clicks: 0});
    fn main() {
        stage.replace(stack(width: 640, height: 420) {
            text(value: bind(d.text), reveal: bind(d.reveal), x: 20, y: 20, width: 300, height: 100, font_size: 32,
                on_link: |e| { if e.id != "entry" { throw "wrong link"; } state.clicks += 1; });
            text(value: derive(|| f"clicks {state.clicks}"), y: 150);
        });
        narrator(rich("[link=entry][b]Entry[/b][/link] then stay"));
        narrator("advanced");
    }"#;
    let font = reel_ui::peniko::Blob::from(
        std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
    );
    let checked = reel_cli::check(source, "rich-links.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for encoded in [false, true] {
        for pointer in [Some(PointerType::Mouse), Some(PointerType::Touch), None] {
            let story = if encoded {
                reel_cli::decode_story(&artifact.bytes).unwrap()
            } else {
                reel_engine::aside::source::compile_story(source, "main", 1, "main").unwrap()
            };
            let mut driver = AsideSessionUi::from_compiled(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                story,
                "en",
                VmLimits::default(),
            )
            .unwrap();
            settle(&mut driver);
            let (mut bridge, root) = bridge_with_font(&driver, Some(font.clone()));
            present(&mut driver, &mut bridge, root, 1);
            if pointer.is_none() {
                gamepad_press(
                    &mut driver,
                    &mut bridge,
                    reel_ui::input::GamepadButton::South,
                );
            } else {
                click_at(&mut driver, &mut bridge, 500., 300.);
            }
            present(&mut driver, &mut bridge, root, 2);
            let interaction = driver.ui().session().view().interaction.unwrap().id();
            if let Some(pointer) = pointer {
                click_with_pointer(&mut driver, &mut bridge, 35., 40., pointer);
            } else {
                gamepad_press(
                    &mut driver,
                    &mut bridge,
                    reel_ui::input::GamepadButton::South,
                );
            }
            present(&mut driver, &mut bridge, root, 3);
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text("clicks 1".into())),
                "link callback: encoded={encoded}, pointer={pointer:?}"
            );
            assert_eq!(
                driver.ui().session().view().interaction.unwrap().id(),
                interaction
            );
            click_at(&mut driver, &mut bridge, 500., 300.);
            assert_ne!(
                driver.ui().session().view().interaction.unwrap().id(),
                interaction
            );
        }
    }
}

fn run(story: CompiledStory, pointer_type: Option<PointerType>) {
    let mut driver = AsideSessionUi::from_compiled(
        Session::new(SessionId(1), Epoch(1)).unwrap(),
        story,
        "en",
        VmLimits::default(),
    )
    .unwrap();
    settle(&mut driver);
    assert_eq!(
        driver.state(),
        StoryState::Running,
        "main returned, mounted handlers remain usable"
    );
    let (mut bridge, root_id) = bridge(&driver);
    present(&mut driver, &mut bridge, root_id, 1);
    let caption = driver
        .adapter()
        .stage()
        .nodes()
        .find(|node| node.kind == NodeKind::Text("Ready".into()))
        .unwrap()
        .id;
    let moving = driver.adapter().stage().nodes().find(|node| matches!(&node.kind, NodeKind::Button { label: Some(label), .. } if label == "Move")).unwrap().id;
    if let Some(pointer_type) = pointer_type {
        click_with_pointer(&mut driver, &mut bridge, 50., 50., pointer_type);
    } else {
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
    }
    present(&mut driver, &mut bridge, root_id, 2);
    assert!(!driver.adapter().stage().node(moving).unwrap().props.enabled);
    assert!(driver.activities_need_time());
    driver.advance_time(0.2, 16).unwrap();
    present(&mut driver, &mut bridge, root_id, 3);
    assert_eq!(driver.adapter().stage().node(caption).unwrap().props.x, 60.);
    assert!(driver.advance_time(0.4, 16).unwrap().woke);
    settle(&mut driver);
    present(&mut driver, &mut bridge, root_id, 4);
    let node = driver.adapter().stage().node(caption).unwrap();
    assert_eq!(node.kind, NodeKind::Text("Arrived".into()));
    assert_eq!(node.props.x, 120.);
    assert!(driver.adapter().stage().node(moving).unwrap().props.enabled);
    if let Some(pointer_type) = pointer_type {
        click_with_pointer(&mut driver, &mut bridge, 270., 50., pointer_type);
    } else {
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::DpadRight,
        );
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::DpadRight,
        );
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
    }
    assert_eq!(driver.state(), StoryState::Finished);
    present(&mut driver, &mut bridge, root_id, 5);
    while driver.cleanup_pending() {
        driver
            .cleanup_step(1, SliceBudget::default(), || 0)
            .unwrap();
    }
}
#[test]
fn source_and_asb_keep_buttons_live_then_finish_after_last_handler_retires() {
    let checked = reel_cli::check(SOURCE, "author_buttons.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for pointer_type in [Some(PointerType::Mouse), Some(PointerType::Touch), None] {
        run(
            reel_cli::decode_story(&artifact.bytes).unwrap(),
            pointer_type,
        );
        run(
            reel_engine::aside::source::compile_story(SOURCE, "main", 1, "main").unwrap(),
            pointer_type,
        );
    }
}

#[test]
fn source_and_asb_modal_menu_isolates_dialogue_and_restores_keyboard_focus() {
    const MODAL: &str = include_str!("../../../examples/author_modal/src/main.aside");
    let checked = reel_cli::check(MODAL, "author_modal.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(MODAL, "main", 1, "main").unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        driver.reveal_more(usize::MAX).unwrap();
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        let press = |driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, key| {
            driver
                .handle_text(bridge, TextEvent::key_down(keyboard::Key::Named(key)))
                .unwrap();
            driver
                .handle_text(bridge, TextEvent::key_up(keyboard::Key::Named(key)))
                .unwrap();
        };
        press(&mut driver, &mut bridge, keyboard::NamedKey::Tab);
        let opener = bridge.root().focused_widget().unwrap();
        let caption = driver
            .adapter()
            .stage()
            .nodes()
            .find(|node| node.kind == NodeKind::Text("Ready".into()))
            .unwrap()
            .id;
        press(&mut driver, &mut bridge, keyboard::NamedKey::Enter);
        settle(&mut driver);
        present(&mut driver, &mut bridge, root, 2);
        assert_ne!(bridge.root().focused_widget(), Some(opener));
        // Both the dialogue region and the underlying opener are blocked.
        click_at(&mut driver, &mut bridge, 400., 350.);
        click_at(&mut driver, &mut bridge, 50., 50.);
        assert_eq!(
            driver.adapter().stage().node(caption).unwrap().kind,
            NodeKind::Text("Ready".into())
        );
        press(&mut driver, &mut bridge, keyboard::NamedKey::Enter);
        settle(&mut driver);
        present(&mut driver, &mut bridge, root, 3);
        assert_eq!(
            driver.adapter().stage().node(caption).unwrap().kind,
            NodeKind::Text("Confirmed".into())
        );
        press(&mut driver, &mut bridge, keyboard::NamedKey::Tab);
        press(&mut driver, &mut bridge, keyboard::NamedKey::Enter);
        settle(&mut driver);
        present(&mut driver, &mut bridge, root, 4);
        assert_eq!(bridge.root().focused_widget(), Some(opener));
        click_at(&mut driver, &mut bridge, 400., 350.);
        present(&mut driver, &mut bridge, root, 5);
        assert_eq!(
            driver.adapter().stage().node(caption).unwrap().kind,
            NodeKind::Text("Story continued".into())
        );
        driver.cancel().unwrap();
        while driver.cleanup_pending() {
            driver
                .cleanup_step(1, SliceBudget::default(), || 0)
                .unwrap();
        }
    }
}

#[test]
fn source_and_asb_dialogue_confirmation_uses_presented_pairing() {
    const STORY: &str = r#"fn main() { narrator("First"); narrator("Second"); }"#;
    let checked = reel_cli::check(STORY, "gamepad.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(STORY, "main", 1, "main").unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        let first = driver.ui().session().view().interaction.unwrap().id();
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        driver.sync(&mut bridge, root).unwrap();
        // First confirmation finishes reveal. Only an accepted new projection
        // can admit the next confirmation which advances to another dialogue.
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        assert_eq!(
            driver.ui().session().view().interaction.unwrap().id(),
            first
        );
        present(&mut driver, &mut bridge, root, 2);
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        assert_ne!(
            driver.ui().session().view().interaction.unwrap().id(),
            first
        );
    }
}

#[test]
fn source_and_asb_author_actions_share_keyboard_gamepad_values_and_cancellation() {
    use reel_ui::input::*;
    const INPUT: &str = include_str!("../../../examples/author_input/src/main.aside");
    let checked = reel_cli::check(INPUT, "author_input.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(INPUT, "main", 1, "main").unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let marker = driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| matches!(&n.kind, NodeKind::Text(s) if s.starts_with("Move with")))
            .unwrap()
            .id;
        let status = driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Text("Ready".into()))
            .unwrap()
            .id;
        let (mut bridge, root) = bridge(&driver);
        let mut frame = 1;
        present(&mut driver, &mut bridge, root, frame);
        let focused = bridge.root().focused_widget();
        assert!(focused.is_some(), "modal action scope takes focus");
        let events = [
            (
                InputDeviceId::Keyboard,
                Control::Key(keyboard::Code::KeyD),
                1.,
                280.,
                "pressed",
            ),
            (
                InputDeviceId::Gamepad(2),
                Control::Axis(GamepadAxis::LeftX),
                0.6,
                280.,
                "pressed",
            ),
            (
                InputDeviceId::Keyboard,
                Control::Key(keyboard::Code::KeyD),
                0.,
                220.,
                "changed",
            ),
        ];
        for (device, control, value, x, phase) in events {
            assert_eq!(
                driver
                    .handle_text(
                        &mut bridge,
                        TextEvent::Input(InputEvent::Value {
                            device,
                            control,
                            value
                        })
                    )
                    .unwrap(),
                Handled::Yes
            );
            settle(&mut driver);
            frame += 1;
            present(&mut driver, &mut bridge, root, frame);
            assert!((driver.adapter().stage().node(marker).unwrap().props.x - x).abs() < 0.0001);
            assert_eq!(
                driver.adapter().stage().node(status).unwrap().kind,
                NodeKind::Text(phase.into())
            );
            assert_eq!(
                bridge.root().focused_widget(),
                focused,
                "claimed axis does not navigate"
            );
        }
        driver
            .handle_text(
                &mut bridge,
                TextEvent::Input(InputEvent::Disconnected(InputDeviceId::Gamepad(2))),
            )
            .unwrap();
        settle(&mut driver);
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 160.);
        assert_eq!(
            driver.adapter().stage().node(status).unwrap().kind,
            NodeKind::Text("cancelled".into())
        );
        // Real keyboard conversion uses the physical code and does not repeat presses.
        let keyboard_event = |state| {
            TextEvent::Keyboard(keyboard::KeyboardEvent {
                code: keyboard::Code::KeyA,
                key: keyboard::Key::Character("a".into()),
                state,
                ..Default::default()
            })
        };
        driver
            .handle_text(&mut bridge, keyboard_event(keyboard::KeyState::Down))
            .unwrap();
        settle(&mut driver);
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 40.);
        driver
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(false))
            .unwrap();
        settle(&mut driver);
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 160.);
        driver
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(true))
            .unwrap();
        driver
            .handle_text(
                &mut bridge,
                TextEvent::Keyboard(keyboard::KeyboardEvent {
                    code: keyboard::Code::KeyA,
                    key: keyboard::Key::Character("a".into()),
                    state: keyboard::KeyState::Down,
                    repeat: true,
                    ..Default::default()
                }),
            )
            .unwrap();
        settle(&mut driver);
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        assert_eq!(
            driver.adapter().stage().node(marker).unwrap().props.x,
            160.,
            "system repeat must not resurrect a cancelled physical key"
        );
        // Both physical bindings hold one logical jump, avoiding duplicate callbacks.
        for (device, control, value) in [
            (
                InputDeviceId::Keyboard,
                Control::Key(keyboard::Code::Space),
                1.,
            ),
            (
                InputDeviceId::Gamepad(2),
                Control::Button(GamepadButton::South),
                1.,
            ),
            (
                InputDeviceId::Keyboard,
                Control::Key(keyboard::Code::Space),
                0.,
            ),
        ] {
            driver
                .handle_text(
                    &mut bridge,
                    TextEvent::Input(InputEvent::Value {
                        device,
                        control,
                        value,
                    }),
                )
                .unwrap();
            settle(&mut driver);
        }
        assert!(driver.activities_need_time());
        driver.advance_time(0.15, 32).unwrap();
        settle(&mut driver);
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.y, 16.);
        driver.advance_time(0.3, 32).unwrap();
        settle(&mut driver);
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        assert!(!driver.activities_need_time());
        for value in [1., 0.] {
            driver
                .handle_text(
                    &mut bridge,
                    TextEvent::Input(InputEvent::Value {
                        device: InputDeviceId::Gamepad(2),
                        control: Control::Button(GamepadButton::East),
                        value,
                    }),
                )
                .unwrap();
            settle(&mut driver);
        }
        assert_eq!(driver.state(), StoryState::Finished);
        assert_eq!(
            driver.adapter().stage().nodes().count(),
            0,
            "finishing retires VM nodes, callbacks and activities; the UI keeps its final projection"
        );
        driver.cancel().unwrap();
        while driver.cleanup_pending() {
            driver
                .cleanup_step(1, SliceBudget::default(), || 0)
                .unwrap();
        }
    }
}

#[test]
fn source_and_asb_handler_only_replacement_cancels_held_actions_through_old_handlers() {
    use reel_ui::input::*;
    let source = r#"fn main() {
        let result = text("result", width: 100, height: 30);
        let pad = input(modal: true, width: 640, height: 300, actions: {
            hold: {bindings: ["ControlLeft"], on_press: || {result.x += 1;},
                on_release: || {throw "old release activated";},
                on_cancel: |e| {
                    if e.phase != "cancelled" || e.target.value("hold") != 0 {throw "cancel state";}
                    result.y += 1;
                }},
            replace: {bindings: ["North"], on_press: |e| {
                e.target.actions = {
                    hold: {bindings: ["ControlLeft"],
                        on_release: || {result.rotation += 1;},
                        on_cancel: || {throw "new handler received old cancellation";}},
                    replace: {bindings: ["North"]},
                };
            }},
        }) {result;};
        stage.replace(pad);
    }"#;
    let checked = reel_cli::check(source, "handler-replacement.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_cli::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let result = driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Text("result".into()))
            .unwrap()
            .id;
        let (mut bridge, root) = bridge(&driver);
        let mut frame = 0;
        let mut flush = |driver: &mut AsideSessionUi, bridge: &mut PresentationBridge| {
            for _ in 0..2 {
                settle(driver);
                frame += 1;
                present(driver, bridge, root, frame);
            }
        };
        flush(&mut driver, &mut bridge);
        let send =
            |driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, control, value| {
                driver
                    .handle_text(
                        bridge,
                        TextEvent::Input(InputEvent::Value {
                            device: InputDeviceId::Gamepad(1),
                            control,
                            value,
                        }),
                    )
                    .unwrap()
            };
        assert_eq!(
            send(
                &mut driver,
                &mut bridge,
                Control::Key(keyboard::Code::ControlLeft),
                1.
            ),
            Handled::Yes
        );
        flush(&mut driver, &mut bridge);
        assert_eq!(driver.adapter().stage().node(result).unwrap().props.x, 1.);
        assert_eq!(
            send(
                &mut driver,
                &mut bridge,
                Control::Button(GamepadButton::North),
                1.
            ),
            Handled::Yes
        );
        flush(&mut driver, &mut bridge);
        assert_eq!(driver.adapter().stage().node(result).unwrap().props.y, 1.);
        assert_eq!(
            send(
                &mut driver,
                &mut bridge,
                Control::Key(keyboard::Code::ControlLeft),
                0.
            ),
            Handled::Yes
        );
        flush(&mut driver, &mut bridge);
        assert_eq!(
            driver
                .adapter()
                .stage()
                .node(result)
                .unwrap()
                .props
                .rotation,
            0.,
            "old release cannot activate replacement"
        );
        for value in [1., 0.] {
            assert_eq!(
                send(
                    &mut driver,
                    &mut bridge,
                    Control::Key(keyboard::Code::ControlLeft),
                    value
                ),
                Handled::Yes
            );
        }
        flush(&mut driver, &mut bridge);
        assert_eq!(
            driver
                .adapter()
                .stage()
                .node(result)
                .unwrap()
                .props
                .rotation,
            1.
        );
    }
}

#[test]
fn source_and_asb_input_scope_remaps_and_retires_multitouch_ownership() {
    use reel_ui::input::*;
    let input_source = r#"
fn main() {
    let marker = text("marker", y: 220, width: 100, height: 30);
    let status = text("ready", y: 260, width: 200, height: 30);
    let counts = model({back: 0, pick: 0, other: 0});
    let popup = stack(x: 200, width: 200, height: 100, modal: true, visible: false);
    popup.add(button("Close", width: 180, height: 80, on_click: |e| { popup.visible = false; }));
    fn move(e) { marker.x = e.target.value("move_x") * 100; }
    fn fire(e) { status.value = e.phase; }
    let pad = input(modal: true, width: 640, height: 300,
        actions: {
            move_x: {bindings: ["KeyD"], on_action: move},
            fire: {bindings: ["PointerPrimary"], on_action: fire},
            menu: {bindings: ["Start"], on_press: || { popup.visible = true; }},
            remap: {bindings: ["North"], on_press: |e| {
                e.target.actions = {
                    move_x: {bindings: ["KeyJ"], on_action: move},
                    fire: {bindings: ["PointerPrimary", "PointerSecondary", "South", "Space"], on_action: fire},
                    back: {bindings: ["East"], on_press: || { counts.back += 1; }},
                    pick: {bindings: ["DpadDown"], on_press: || { counts.pick += 1; }},
                    other: {bindings: ["West"], on_press: || { counts.other += 1; }},
                };
                marker.x = e.target.value("move_x") * 100;
            }},
        }
    ) {
        marker; status; popup;
        text(value: derive(|| f"{counts.back}:{counts.pick}:{counts.other}"), y: 300);
        button("UI", x: 420, width: 100, height: 80,
            on_click: |e| { marker.y += 1; });
    };
    let mut rejected = false;
    try { pad.actions = {bad: {bindings: ["no-such-control"]}}; }
    catch error { rejected = true; }
    if !rejected { throw "invalid binding assignment must fail at the script boundary"; }
    stage.replace(pad);
    narrator("input must not advance this dialogue");
}
"#;
    let checked = reel_cli::check(input_source, "input-scope.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(input_source, "main", 1, "main").unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let marker = driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Text("marker".into()))
            .unwrap()
            .id;
        let status = driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Text("ready".into()))
            .unwrap()
            .id;
        let counts = driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Text("0:0:0".into()))
            .unwrap()
            .id;
        let (mut bridge, root) = bridge(&driver);
        let mut frame = 0;
        let flush =
            |driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, frame: &mut u64| {
                for _ in 0..2 {
                    settle(driver);
                    *frame += 1;
                    present(driver, bridge, root, *frame);
                }
            };
        flush(&mut driver, &mut bridge, &mut frame);
        let send =
            |driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, control, value| {
                driver
                    .handle_text(
                        bridge,
                        TextEvent::Input(InputEvent::Value {
                            device: InputDeviceId::Gamepad(1),
                            control,
                            value,
                        }),
                    )
                    .unwrap()
            };
        let mut navigation = NavigationInput::default();
        let route = |driver: &mut AsideSessionUi,
                     bridge: &mut PresentationBridge,
                     navigation: &mut NavigationInput,
                     control,
                     value| {
            let raw = InputEvent::Value {
                device: InputDeviceId::Gamepad(1),
                control,
                value,
            };
            let handled = driver
                .handle_text(bridge, TextEvent::Input(raw.clone()))
                .unwrap();
            let mut events = Vec::new();
            if handled.is_handled() {
                navigation.cancel(|event| events.push(event));
            } else {
                navigation
                    .update(&raw, std::time::Duration::ZERO, |event| events.push(event))
                    .unwrap();
            }
            for event in events {
                driver
                    .handle_text(bridge, TextEvent::Navigation(event))
                    .unwrap();
            }
            handled
        };
        send(
            &mut driver,
            &mut bridge,
            Control::Key(keyboard::Code::KeyD),
            1.,
        );
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 100.);
        send(
            &mut driver,
            &mut bridge,
            Control::Button(GamepadButton::Start),
            1.,
        );
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(marker).unwrap().props.x,
            0.,
            "accepted modal change cancels old held action"
        );
        driver
            .handle_text(
                &mut bridge,
                TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Enter)),
            )
            .unwrap();
        driver
            .handle_text(
                &mut bridge,
                TextEvent::key_up(keyboard::Key::Named(keyboard::NamedKey::Enter)),
            )
            .unwrap();
        flush(&mut driver, &mut bridge, &mut frame);
        send(
            &mut driver,
            &mut bridge,
            Control::Key(keyboard::Code::KeyD),
            1.,
        );
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 100.);
        send(
            &mut driver,
            &mut bridge,
            Control::Button(GamepadButton::North),
            1.,
        );
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(marker).unwrap().props.x,
            0.,
            "rebind is neutral even before the next frame"
        );
        assert_eq!(
            send(
                &mut driver,
                &mut bridge,
                Control::Key(keyboard::Code::KeyD),
                1.
            ),
            Handled::No
        );
        send(
            &mut driver,
            &mut bridge,
            Control::Key(keyboard::Code::KeyJ),
            1.,
        );
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 100.);
        assert_eq!(
            route(
                &mut driver,
                &mut bridge,
                &mut navigation,
                Control::Button(GamepadButton::South),
                1.
            ),
            Handled::Yes,
            "input owns South before child focus"
        );
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(status).unwrap().kind,
            NodeKind::Text("pressed".into())
        );
        driver
            .handle_text(
                &mut bridge,
                TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab)),
            )
            .unwrap();
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(marker).unwrap().props.x,
            0.,
            "child focus cancels the parent game action"
        );
        assert_eq!(
            driver.adapter().stage().node(status).unwrap().kind,
            NodeKind::Text("cancelled".into()),
            "focus transfer cancels the parent's held South action"
        );
        assert_eq!(
            route(
                &mut driver,
                &mut bridge,
                &mut navigation,
                Control::Button(GamepadButton::South),
                0.
            ),
            Handled::No
        );
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(marker).unwrap().props.y,
            220.,
            "release from the previous owner cannot activate a newly focused button"
        );
        for value in [1., 0.] {
            assert_eq!(
                route(
                    &mut driver,
                    &mut bridge,
                    &mut navigation,
                    Control::Button(GamepadButton::South),
                    value
                ),
                Handled::No,
                "parent mapping must not steal a focused child button's confirmation"
            );
        }
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(status).unwrap().kind,
            NodeKind::Text("cancelled".into()),
            "button confirmation does not restart the parent action"
        );
        for button in [
            GamepadButton::East,
            GamepadButton::DpadDown,
            GamepadButton::West,
        ] {
            for value in [1., 0.] {
                assert_eq!(
                    route(
                        &mut driver,
                        &mut bridge,
                        &mut navigation,
                        Control::Button(button),
                        value
                    ),
                    Handled::Yes
                );
            }
        }
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(counts).unwrap().kind,
            NodeKind::Text("1:1:1".into()),
            "unclaimed back, direction and other controls reach the ancestor once"
        );
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.y, 221.);
        for state in [keyboard::KeyState::Down, keyboard::KeyState::Up] {
            driver
                .handle_text(
                    &mut bridge,
                    TextEvent::Keyboard(keyboard::KeyboardEvent {
                        code: keyboard::Code::Space,
                        key: keyboard::Key::Character(" ".into()),
                        state,
                        ..Default::default()
                    }),
                )
                .unwrap();
        }
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(marker).unwrap().props.y,
            222.,
            "keyboard and controller activate the same child control"
        );
        for value in [1., 0.] {
            assert_eq!(
                route(
                    &mut driver,
                    &mut bridge,
                    &mut navigation,
                    Control::Button(GamepadButton::DpadLeft),
                    value
                ),
                Handled::No,
                "unbound direction remains available to spatial navigation"
            );
        }
        flush(&mut driver, &mut bridge, &mut frame);
        // Spatial navigation skips focus ancestors; Tab retains the input's stop.
        for state in [keyboard::KeyState::Down, keyboard::KeyState::Up] {
            driver
                .handle_text(
                    &mut bridge,
                    TextEvent::Keyboard(keyboard::KeyboardEvent {
                        code: keyboard::Code::Tab,
                        key: keyboard::Key::Named(keyboard::NamedKey::Tab),
                        state,
                        ..Default::default()
                    }),
                )
                .unwrap();
        }
        flush(&mut driver, &mut bridge, &mut frame);
        for value in [1., 0.] {
            assert_eq!(
                route(
                    &mut driver,
                    &mut bridge,
                    &mut navigation,
                    Control::Button(GamepadButton::South),
                    value
                ),
                Handled::Yes,
                "South returns to the game mapping after focus returns to input"
            );
        }
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(status).unwrap().kind,
            NodeKind::Text("released".into())
        );
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.y, 222.);
        send(
            &mut driver,
            &mut bridge,
            Control::Key(keyboard::Code::KeyJ),
            1.,
        );
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 100.);
        for (id, down, phase) in [
            (10, true, "pressed"),
            (11, true, "pressed"),
            (10, false, "pressed"),
            (11, false, "released"),
        ] {
            let pointer = PointerInfo {
                pointer_id: PointerId::new(id),
                pointer_type: PointerType::Touch,
                persistent_device_id: None,
            };
            let event = PointerButtonEvent {
                pointer,
                state: PointerState {
                    position: PhysicalPosition::new(40., 40.),
                    ..Default::default()
                },
                button: None,
            };
            driver
                .handle_pointer(
                    &mut bridge,
                    if down {
                        PointerEvent::Down(event)
                    } else {
                        PointerEvent::Up(event)
                    },
                )
                .unwrap();
            flush(&mut driver, &mut bridge, &mut frame);
            assert_eq!(
                driver.adapter().stage().node(status).unwrap().kind,
                NodeKind::Text(phase.into()),
                "both contacts own the same logical action independently"
            );
        }
        let pointer = PointerInfo {
            pointer_id: Some(PointerId::PRIMARY),
            pointer_type: PointerType::Mouse,
            persistent_device_id: None,
        };
        let mut buttons = pointer::PointerButtons::default();
        for (button, down) in [
            (PointerButton::Primary, true),
            (PointerButton::Secondary, true),
            (PointerButton::Primary, false),
        ] {
            if down {
                buttons.insert(button);
            } else {
                buttons.remove(button);
            }
            let event = PointerButtonEvent {
                pointer,
                state: PointerState {
                    position: if down {
                        PhysicalPosition::new(40., 40.)
                    } else {
                        PhysicalPosition::new(900., 900.)
                    },
                    buttons,
                    ..Default::default()
                },
                button: Some(button),
            };
            driver
                .handle_pointer(
                    &mut bridge,
                    if down {
                        PointerEvent::Down(event)
                    } else {
                        PointerEvent::Up(event)
                    },
                )
                .unwrap();
            flush(&mut driver, &mut bridge, &mut frame);
            assert_eq!(
                driver.adapter().stage().node(status).unwrap().kind,
                NodeKind::Text("pressed".into()),
                "remaining mouse button retains the shared action outside the scope bounds"
            );
        }
        assert_eq!(
            driver.ui().session().view().history.len(),
            1,
            "consumed action did not advance dialogue"
        );
        bridge.clear();
        driver.clear_presented().unwrap();
        settle(&mut driver);
        driver.sync(&mut bridge, root).unwrap();
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.x, 0.);
        assert_eq!(
            driver.adapter().stage().node(status).unwrap().kind,
            NodeKind::Text("cancelled".into())
        );
        driver.cancel().unwrap();
        while driver.cleanup_pending() {
            driver
                .cleanup_step(1, SliceBudget::default(), || 0)
                .unwrap();
        }
    }
}

#[test]
fn source_and_asb_characters_share_history_unicode_playback_and_group_advance() {
    use reel_engine::InteractionView;
    use reel_player::story_runtime::StoryRuntime;
    const DIALOGUE: &str = include_str!("../../../examples/author_dialogue/src/main.aside");
    let checked = reel_cli::check(DIALOGUE, "author_dialogue.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(DIALOGUE, "main", 1, "main").unwrap(),
    ] {
        let driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        let mut runtime = StoryRuntime::new(driver);
        let tick = |runtime: &mut StoryRuntime, ns| {
            runtime
                .tick(
                    SliceBudget {
                        deadline_ns: Some(ns + 1),
                        ..Default::default()
                    },
                    || ns,
                )
                .unwrap()
        };
        let reveal = |driver: &AsideSessionUi| {
            let Some(InteractionView::Dialogue(d)) = driver.ui().session().view().interaction
            else {
                panic!("dialogue expected")
            };
            d.lines()
                .iter()
                .map(|l| l.visible_text().to_owned())
                .collect::<Vec<_>>()
        };
        tick(&mut runtime, 0);
        assert!(
            !runtime.needs_tick(),
            "no reveal before an accepted presentation"
        );
        let (mut bridge, root) = bridge(runtime.driver());
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 1);
        tick(&mut runtime, 0);
        let history = &runtime.driver().ui().session().view().history[0];
        assert_eq!(history.lines()[0].speaker_id(), Some("mio"));
        assert_eq!(history.lines()[0].speaker_name(), "Mio");
        tick(&mut runtime, 100_000_000);
        assert_eq!(reveal(runtime.driver()), ["A"]);
        tick(&mut runtime, 200_000_000);
        assert_eq!(reveal(runtime.driver()), ["Aé"]);
        runtime.suspend(200_000_000).unwrap();
        assert!(!runtime.needs_tick());
        runtime.resume(10_200_000_000);
        tick(&mut runtime, 10_200_000_000);
        assert_eq!(reveal(runtime.driver()), ["Aé"]);
        tick(&mut runtime, 10_300_000_000);
        assert_eq!(reveal(runtime.driver()), ["Aé👨‍👩‍👧‍👦"]);
        runtime.driver_mut().unwrap().begin_collection().unwrap();
        while !runtime
            .driver_mut()
            .unwrap()
            .gc_step(Default::default())
            .unwrap()
            .cycle_complete
        {}
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 2);
        gamepad_press(
            runtime.driver_mut().unwrap(),
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        settle(runtime.driver_mut().unwrap());
        assert_eq!(reveal(runtime.driver()), ["Aé👨‍👩‍👧‍👦雨"]);
        assert!(!runtime.needs_tick(), "full text sleeps");
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 3);
        gamepad_press(
            runtime.driver_mut().unwrap(),
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        settle(runtime.driver_mut().unwrap());
        assert_eq!(runtime.driver().ui().session().view().history.len(), 2);
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 4);
        tick(&mut runtime, 11_000_000_000);
        tick(&mut runtime, 11_100_000_000);
        assert_eq!(reveal(runtime.driver()), ["一", "四"]);
        let history = &runtime.driver().ui().session().view().history[1];
        assert_eq!(history.lines()[0].speaker_name(), "Mio");
        assert_eq!(history.lines()[1].speaker_name(), "Eri");
        assert_ne!(
            history.lines()[0].content().id(),
            history.lines()[1].content().id()
        );
        // A held confirmation freezes the accepted text until release.
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 5);
        runtime
            .driver_mut()
            .unwrap()
            .handle_text(
                &mut bridge,
                TextEvent::Navigation(reel_ui::input::ActionEvent {
                    action: reel_ui::input::NavigationAction::Confirm,
                    value: 1.,
                    phase: reel_ui::input::ActionPhase::Pressed,
                }),
            )
            .unwrap();
        tick(&mut runtime, 20_000_000_000);
        assert_eq!(reveal(runtime.driver()), ["一", "四"]);
        runtime
            .driver_mut()
            .unwrap()
            .handle_text(
                &mut bridge,
                TextEvent::Navigation(reel_ui::input::ActionEvent {
                    action: reel_ui::input::NavigationAction::Confirm,
                    value: 0.,
                    phase: reel_ui::input::ActionPhase::Released,
                }),
            )
            .unwrap();
        settle(runtime.driver_mut().unwrap());
        assert_eq!(reveal(runtime.driver()), ["一二三", "四五"]);
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 6);
        gamepad_press(
            runtime.driver_mut().unwrap(),
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        settle(runtime.driver_mut().unwrap());
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 7);
        tick(&mut runtime, 21_000_000_000);
        gamepad_press(
            runtime.driver_mut().unwrap(),
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        settle(runtime.driver_mut().unwrap());
        assert_eq!(
            reveal(runtime.driver()),
            [""],
            "protected reveal consumes advance"
        );
        for frame in 8..=28 {
            tick(&mut runtime, 21_000_000_000 + (frame - 7) * 100_000_000);
            present(runtime.driver_mut().unwrap(), &mut bridge, root, frame);
        }
        assert_eq!(reveal(runtime.driver()), ["Listen."]);
        gamepad_press(
            runtime.driver_mut().unwrap(),
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        settle(runtime.driver_mut().unwrap());
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 29);
        tick(&mut runtime, 24_000_000_000);
        assert_eq!(
            reveal(runtime.driver()),
            ["The conversation stays in history."]
        );
        assert!(!runtime.needs_tick());
        for (index, expected) in [
            vec!["The conversation stays in history."],
            vec!["……"],
            vec!["A fleeting thought."],
            vec!["Noise.", "Stay."],
        ]
        .into_iter()
        .enumerate()
        {
            let frame = 30 + index as u64 * 2;
            present(runtime.driver_mut().unwrap(), &mut bridge, root, frame);
            tick(&mut runtime, 25_000_000_000 + index as u64 * 1_000_000_000);
            assert_eq!(reveal(runtime.driver()), expected);
            present(runtime.driver_mut().unwrap(), &mut bridge, root, frame + 1);
            gamepad_press(
                runtime.driver_mut().unwrap(),
                &mut bridge,
                reel_ui::input::GamepadButton::South,
            );
            settle(runtime.driver_mut().unwrap());
        }
        assert_eq!(runtime.driver().state(), StoryState::Finished);
        let history = &runtime.driver().ui().session().view().history;
        assert_eq!(history.len(), 7);
        assert_eq!(
            history[4].lines()[0].resolved_text(),
            "I remember the rain."
        );
        assert_eq!(
            history[5].lines()[0].resolved_text(),
            "The train is arriving."
        );
        assert_eq!(history[6].lines().len(), 1);
        assert_eq!(history[6].lines()[0].speaker_id(), Some("eri"));
        assert_eq!(history[6].lines()[0].resolved_text(), "Stay.");
    }
}

#[test]
fn dialogue_validation_is_atomic_and_callable_characters_keep_native_boundaries() {
    const DIALOGUE: &str = r#"
        let speaker = character("Same name", id: "one");
        fn helper(voice, ...words) { voice(text: words[0]); }
        fn main() {
            let mut failures = 0;
            try { character("bad", id: 5); } catch e { failures += 1; }
            try { speaker.name = "changed"; } catch e { failures += 1; }
            try { say([]); } catch e { failures += 1; }
            try { say([speaker.line("valid"), text("wrong")]); } catch e { failures += 1; }
            try { let _ = column() { speaker("cannot wait in children"); }; } catch e { failures += 1; }
            try { game.configure(text_speed: -1); } catch e { failures += 1; }
            if failures != 6 { throw failures; }
            if speaker.name != "Same name" || speaker.id != "one" { throw "changed character"; }
            let line = speaker.line;
            let rows = [];
            for i in 0..128 { rows.append(line(text: f"Line {i}")); }
            say(lines: rows);
            helper(speaker, "after group");
        }
    "#;
    let checked = reel_cli::check(DIALOGUE, "dialogue-validation.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(DIALOGUE, "main", 1, "main").unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let view = driver.ui().session().view();
        assert_eq!(
            view.history.len(),
            1,
            "failed groups and NoWait do not publish history"
        );
        assert_eq!(view.history[0].lines().len(), 128);
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        click_at(&mut driver, &mut bridge, 100., 300.);
        present(&mut driver, &mut bridge, root, 2);
        click_at(&mut driver, &mut bridge, 100., 300.);
        assert_eq!(driver.ui().session().view().history.len(), 2);
        assert_eq!(
            driver.ui().session().view().history[1].lines()[0].speaker_id(),
            Some("one")
        );
        driver.cancel().unwrap();
        for _ in 0..10_000 {
            if !driver.cleanup_pending() {
                break;
            }
            driver
                .cleanup_step(256, SliceBudget::default(), || 0)
                .unwrap();
        }
        assert!(!driver.cleanup_pending());
        assert_eq!(driver.state(), StoryState::Cancelled);
    }
}

#[test]
fn source_and_asb_composed_dropdown_confirms_or_cancels_and_restores_its_trigger() {
    use keyboard::NamedKey::*;
    const POPUP: &str = include_str!("../../../examples/author_popup/src/main.aside");
    let checked = reel_cli::check(POPUP, "author_popup.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_engine::aside::source::compile_story(POPUP, "main", 1, "main").unwrap(),
        reel_cli::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let (mut bridge, root) = bridge(&driver);
        let mut frame = 0;
        let flush =
            |driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, frame: &mut u64| {
                for _ in 0..6 {
                    *frame += 1;
                    present(driver, bridge, root, *frame);
                    settle(driver);
                }
            };
        let press = |driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, key| {
            for event in [
                TextEvent::key_down(keyboard::Key::Named(key)),
                TextEvent::key_up(keyboard::Key::Named(key)),
            ] {
                driver.handle_text(bridge, event).unwrap();
            }
            settle(driver);
        };
        flush(&mut driver, &mut bridge, &mut frame);
        let count = driver.adapter().stage().nodes().count();
        let trigger = driver
            .adapter()
            .stage()
            .nodes()
            .find(|node| {
                matches!(&node.kind,
            NodeKind::Button{label:Some(label),..} if label=="Option 1")
            })
            .unwrap()
            .id;
        let caption = |driver: &AsideSessionUi| {
            let NodeKind::Button {
                label: Some(label), ..
            } = &driver.adapter().stage().node(trigger).unwrap().kind
            else {
                panic!("missing trigger");
            };
            label.clone()
        };
        let popups = |driver: &AsideSessionUi| {
            driver
                .adapter()
                .stage()
                .nodes()
                .filter(|node| matches!(node.kind, NodeKind::Popup(_)))
                .count()
        };
        press(&mut driver, &mut bridge, Tab);
        let focus = bridge.root().focused_widget().unwrap();
        press(&mut driver, &mut bridge, Enter);
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(popups(&driver), 1);
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|node| matches!(&node.kind,
            NodeKind::VirtualList(list) if list.total_rows==40 && list.rows.len()<40)),
            "the long menu builds its current window"
        );
        assert_ne!(bridge.root().focused_widget(), Some(focus));
        for _ in 0..3 {
            press(&mut driver, &mut bridge, ArrowDown);
        }
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            caption(&driver),
            "Option 1",
            "temporary focus leaves the committed value unchanged"
        );
        press(&mut driver, &mut bridge, Enter);
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            caption(&driver),
            "Option 5",
            "confirmation returns the option and skips disabled Option 4"
        );
        assert_eq!(popups(&driver), 0);
        assert_eq!(bridge.root().focused_widget(), Some(focus));
        for outside in [false, true] {
            press(&mut driver, &mut bridge, Enter);
            flush(&mut driver, &mut bridge, &mut frame);
            assert_eq!(popups(&driver), 1);
            press(&mut driver, &mut bridge, ArrowDown);
            assert_eq!(caption(&driver), "Option 5");
            if outside {
                click_at(&mut driver, &mut bridge, 630., 410.);
            } else {
                gamepad_press(
                    &mut driver,
                    &mut bridge,
                    reel_ui::input::GamepadButton::East,
                );
            }
            flush(&mut driver, &mut bridge, &mut frame);
            assert_eq!(popups(&driver), 0);
            assert_eq!(
                caption(&driver),
                "Option 5",
                "cancel preserves the previous business value"
            );
            assert_eq!(bridge.root().focused_widget(), Some(focus));
            assert_eq!(
                driver.adapter().stage().nodes().count(),
                count,
                "the menu scope retires its nodes"
            );
        }
    }
}
