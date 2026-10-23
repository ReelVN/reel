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
fn source_and_asb_hud_clicks_preserve_gameplay_focus_through_a_modal() {
    use reel_ui::input::{Control, GamepadButton, InputDeviceId, InputEvent};
    let source = r#"fn main() {
        let marker = stack(x: 0, y: 200, width: 1, height: 1);
        let menu = input(width: 640, height: 420, modal: true, visible: false);
        menu.actions = {
            resume: {bindings: ["Enter"], on_release: || {menu.visible = false;}}
        };
        stage.replace(input(width: 640, height: 420, modal: true,
            actions: {jump: {bindings: ["South"], on_press: || {marker.x += 1;}}}
        ) {
            marker;
            button("Pause", x: 20, y: 20, width: 180, height: 60,
                focus_on_pointer: false,
                on_click: || {marker.y += 1; menu.visible = true;});
            menu;
        });
    }"#;
    let checked = reel_cli::check(source, "hud-focus.aside").unwrap();
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
        let marker = driver
            .adapter()
            .stage()
            .nodes()
            .find(|node| matches!(node.kind, NodeKind::Stack))
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
        let press = |driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, key| {
            for state in [keyboard::KeyState::Down, keyboard::KeyState::Up] {
                driver
                    .handle_text(
                        bridge,
                        TextEvent::Keyboard(keyboard::KeyboardEvent {
                            state,
                            key: keyboard::Key::Named(key),
                            code: match key {
                                keyboard::NamedKey::Enter => keyboard::Code::Enter,
                                keyboard::NamedKey::Tab => keyboard::Code::Tab,
                                _ => unreachable!(),
                            },
                            ..Default::default()
                        }),
                    )
                    .unwrap();
            }
        };
        flush(&mut driver, &mut bridge, &mut frame);
        let gameplay_focus = bridge.root().focused_widget().unwrap();
        for (index, event) in click_events(50., 50., PointerType::Mouse)
            .into_iter()
            .enumerate()
        {
            driver.handle_pointer(&mut bridge, event).unwrap();
            // Accept the pressed appearance before the physical release.
            flush(&mut driver, &mut bridge, &mut frame);
            if index == 1 {
                assert_eq!(bridge.root().focused_widget(), Some(gameplay_focus));
            }
        }
        assert_eq!(driver.adapter().stage().node(marker).unwrap().props.y, 201.);
        assert_ne!(bridge.root().focused_widget(), Some(gameplay_focus));
        press(&mut driver, &mut bridge, keyboard::NamedKey::Enter);
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(bridge.root().focused_widget(), Some(gameplay_focus));
        for value in [1., 0.] {
            driver
                .handle_text(
                    &mut bridge,
                    TextEvent::Input(InputEvent::Value {
                        device: InputDeviceId::Gamepad(1),
                        control: Control::Button(GamepadButton::South),
                        value,
                    }),
                )
                .unwrap();
        }
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(marker).unwrap().props.x,
            1.,
            "South must return to gameplay after a pointer-opened modal"
        );

        press(&mut driver, &mut bridge, keyboard::NamedKey::Tab);
        flush(&mut driver, &mut bridge, &mut frame);
        let button_focus = bridge.root().focused_widget().unwrap();
        assert_ne!(
            button_focus, gameplay_focus,
            "HUD buttons remain in Tab order"
        );
        gamepad_press(&mut driver, &mut bridge, GamepadButton::South);
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(
            driver.adapter().stage().node(marker).unwrap().props.y,
            202.,
            "a focused HUD button still accepts controller confirmation"
        );
        press(&mut driver, &mut bridge, keyboard::NamedKey::Enter);
        flush(&mut driver, &mut bridge, &mut frame);
        assert_eq!(bridge.root().focused_widget(), Some(button_focus));
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
            let invalid_id: any = 5;
            try { character("bad", id: invalid_id); } catch e { failures += 1; }
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

mod author_exit {
    //! Author shutdown uses the same cleanup path in source and encoded projects.
    use super::support;

    use reel_engine::{
        Epoch, OwnerLifecycle, Session, SessionId,
        aside::{SliceBudget, VmLimits, source::compile_story},
    };
    use reel_player::{aside_session::AsideSessionUi, story_runtime::StoryRuntime};

    #[test]
    fn source_and_asb_exit_from_story_or_waiting_callback_retire_the_session() {
        for callback in [false, true] {
            let source = if callback {
                r#"fn main() {
                stage.replace(button("Exit", width: 200, height: 80, on_click: |event| {
                    await run(delay(0.5));
                    game.exit();
                    throw "callback continued after exit";
                }));
                await completion();
                throw "main continued after exit";
            }"#
            } else {
                r#"fn main() {
                run(repeat(delay(60)));
                game.exit();
                throw "main continued after exit";
            }"#
            };
            let checked = reel_cli::check(source, "main.aside").unwrap();
            let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
            for story in [
                compile_story(source, "main", 1, "main").unwrap(),
                reel_cli::decode_story(&artifact.bytes).unwrap(),
            ] {
                let mut driver = AsideSessionUi::from_compiled(
                    Session::new(SessionId(1), Epoch(1)).unwrap(),
                    story,
                    "en",
                    VmLimits::default(),
                )
                .unwrap();
                let owner = driver.ui().session().root_owner();
                if callback {
                    support::settle(&mut driver);
                    let (mut bridge, root) = support::bridge(&driver);
                    support::present(&mut driver, &mut bridge, root, 1);
                    support::click_at(&mut driver, &mut bridge, 50., 40.);
                    assert!(!driver.adapter().exit_requested());
                    driver.advance_time(1., 64).unwrap();
                }
                let mut runtime = StoryRuntime::new(driver);
                for _ in 0..10_000 {
                    runtime
                        .tick(
                            SliceBudget {
                                instructions: 3,
                                ..Default::default()
                            },
                            || 1_000_000_000,
                        )
                        .unwrap();
                    if runtime.close_complete() {
                        break;
                    }
                }
                assert!(runtime.driver().adapter().exit_requested());
                assert!(runtime.close_complete());
                assert!(!runtime.needs_tick());
                assert!(runtime.driver().adapter().task().is_none());
                assert_eq!(
                    runtime.driver().ui().session().owner_lifecycle(owner),
                    Some(OwnerLifecycle::Retired)
                );
                assert!(runtime.driver().fault().unwrap().is_none());
            }
        }
    }
}

mod author_pointer {
    //! Source and actual ASB exercise the example through accepted UI input.
    //! Host completion is covered by the engine contract checks; this has no native window.
    use super::support;
    use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};
    use reel_player::aside_session::AsideSessionUi;
    use reel_ui::stage::NodeKind;
    use support::*;

    #[test]
    fn source_and_asb_handle_refusal_and_destination_pointer_input() {
        let checked = reel_cli::check(
            r#"fn main() {
    game.configure(width: 640, height: 360, fonts: ["fonts/body.ttf"]);
    let status = text("Click Move while using a mouse.", x: 24, y: 24);
    let root = stack(width: 640, height: 360);
    root.add(status);
    root.add(
        button(
            "Move pointer", x: 24, y: 100, width: 180, height: 60,
            on_click: |event| {
                let moved = await stage.move_pointer(440, 120, owner: root);
                status.value = if moved {
                    "The pointer moved through ordinary hit testing."
                } else {
                    "Pointer movement is unavailable for the current input or host."
                };
            }
        )
    );
    root.add(
        button(
            "Destination", x: 400, y: 100, width: 180, height: 60,
            on_click: |event| {
                status.value = "Destination clicked.";
            }
        )
    );
    stage.replace(root);
}
"#,
            "author_pointer.aside",
        )
        .unwrap();
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
            let (mut bridge, root) = bridge(&driver);
            present(&mut driver, &mut bridge, root, 1);
            click_at(&mut driver, &mut bridge, 30., 110.);
            assert!(driver.adapter().stage().nodes().any(|node| node.kind
                == NodeKind::Text(
                    "Pointer movement is unavailable for the current input or host.".into()
                )));
            present(&mut driver, &mut bridge, root, 2);
            click_at(&mut driver, &mut bridge, 440., 120.);
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|node| node.kind == NodeKind::Text("Destination clicked.".into()))
            );
            driver.cancel().unwrap();
        }
    }
}

mod dynamic_stage {
    //! Real source/ASB editing changes published geometry without replacing its root.
    use reel_core::{
        budget::{BudgetCategory, BudgetPool, QueueLimits},
        identity::{FrameId, SurfaceEpoch},
    };
    use reel_engine::{
        Epoch, Session, SessionId,
        aside::{RunOutcome, SliceBudget, VmLimits, source::CompiledStory},
    };
    use reel_player::aside_session::{AsideSessionUi, StoryProgress};
    use reel_ui::{
        app::{GeometryLimits, RenderRoot, RenderRootOptions, WindowSizePolicy},
        bridge::{
            BridgeLimits, NoticeResult, PresentationBridge, PresentationNotice, PresentationOutcome,
        },
        core::{DefaultProperties, Widget},
        dpi::PhysicalSize,
        frame::{BuildLimits, FrameBuild, FrameDraw, FrameInfo, FrameOp, StepBudget},
        peniko::Brush,
        resources::ResourceSnapshot,
    };
    use std::sync::Arc;

    #[test]
    fn children_compose_owned_subtrees_without_duplicate_mounting() {
        let source = r#"fn component(children) { column(children: children) }
    fn main() {
        let outside_child = text("outside");
        let outside = stack(children: [outside_child]);
        let old = stack();
        stage.replace(old);
        let retired = text("retired");
        retired.remove();
        let mut rejected = 0;
        try { let _ = column { outside_child; }; } catch e { rejected += 1; }
        try { let _ = column { outside; outside; }; } catch e { rejected += 1; }
        try { let _ = column { old; }; } catch e { rejected += 1; }
        try { let _ = column { retired; }; } catch e { rejected += 1; }
        if rejected != 4 { throw "ownership validation"; }
        let constructor = component;
        let result = constructor {
            let panel = stack();
            let control = button("control");
            control.add(text("caption"));
            if true { control.add(text("conditional")); }
            panel.add(control);
            panel;
            for name in ["one", "two"] {
                let card = stack();
                card.add(text(name));
                card;
            }
            // A failed construction above left this detached component intact.
            row(children: [outside_child, outside]);
        };
        stage.replace(result);
        narrator("composed");
        result.clear();
        result.add(text("replacement"));
        narrator("cleared");
    }"#;
        let checked = reel_cli::check(source, "composition.aside").unwrap();
        let bytes = reel_cli::build(&checked, Some("main::main")).unwrap().bytes;
        for story in [
            reel_engine::aside::source::compile_story(source, "main", 1, "main").unwrap(),
            reel_cli::decode_story(&bytes).unwrap(),
        ] {
            let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
            let mut vm =
                reel_engine::aside::SessionVm::load(&session, story.program, VmLimits::default())
                    .unwrap();
            for function in [story.initializer, story.entry] {
                vm.start(&session, function).unwrap();
                loop {
                    match vm
                        .step(
                            &mut session,
                            reel_engine::aside::SessionBudget::Exact(SliceBudget::default()),
                            || 0,
                        )
                        .unwrap()
                    {
                        reel_engine::aside::SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                            if let Err(error) = vm.dispatch(
                                &mut session,
                                ticket,
                                "en",
                                reel_engine::aside::NativeBudget::Exact(1024),
                            ) {
                                vm.reject_native(&session, ticket, error).unwrap();
                            }
                        }
                        reel_engine::aside::SessionStep::Vm(RunOutcome::Returned(_)) => {
                            vm.discard_delivery(&mut session).unwrap();
                            break;
                        }
                        reel_engine::aside::SessionStep::Vm(RunOutcome::Waiting(_)) => break,
                        reel_engine::aside::SessionStep::Vm(RunOutcome::Fault(id)) => {
                            panic!("{:?}", vm.vm().delivery_fault(id).unwrap())
                        }
                        _ => {}
                    }
                }
            }
            vm.begin_collection().unwrap();
            while !vm
                .gc_step(reel_engine::aside::GcBudget {
                    units: 4096,
                    ..Default::default()
                })
                .unwrap()
                .cycle_complete
            {}
            vm.sync_stage(&session).unwrap();
            let root = vm.stage().root().unwrap();
            assert_eq!(vm.stage().node(root).unwrap().children.len(), 4);
            let mut pending = vec![root];
            let mut labels = Vec::new();
            while let Some(id) = pending.pop() {
                let node = vm.stage().node(id).unwrap();
                if let reel_ui::stage::NodeKind::Text(text) = &node.kind {
                    labels.push(text.to_string());
                }
                for child in node.children.iter().rev() {
                    assert_eq!(vm.stage().node(*child).unwrap().parent, Some(id));
                    pending.push(*child);
                }
            }
            assert_eq!(labels, ["caption", "conditional", "one", "two", "outside"]);
            let interaction = session.view().interaction.unwrap().id();
            for _ in 0..2 {
                session
                    .handle_local_input(
                        session.root_owner(),
                        reel_engine::InputAction::Advance(interaction),
                    )
                    .unwrap();
            }
            loop {
                match vm
                    .step(
                        &mut session,
                        reel_engine::aside::SessionBudget::Exact(SliceBudget::default()),
                        || 0,
                    )
                    .unwrap()
                {
                    reel_engine::aside::SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                        vm.dispatch(
                            &mut session,
                            ticket,
                            "en",
                            reel_engine::aside::NativeBudget::Exact(1024),
                        )
                        .unwrap();
                    }
                    reel_engine::aside::SessionStep::Vm(RunOutcome::Waiting(_)) => break,
                    reel_engine::aside::SessionStep::Vm(RunOutcome::Fault(id)) => {
                        panic!("{:?}", vm.vm().delivery_fault(id).unwrap())
                    }
                    _ => {}
                }
            }
            vm.sync_stage(&session).unwrap();
            assert_eq!(vm.stage().root(), Some(root));
            assert_eq!(
                vm.stage().nodes().count(),
                2,
                "clear retires every composed descendant"
            );
        }
    }

    const SOURCE: &str = r##"fn main() {
    let red = stack(width: 20, height: 20, background: "#ff0000");
    let blue = stack(width: 20, height: 20, background: "#0000ff");
    let root = row(x: 10, y: 10) { red; blue; };
    stage.replace(root);
    await run(tween(root, duration: 1));
    root.add(stack(width: 20, height: 20, background: "#00ff00"), index: 1);
    await run(tween(root, duration: 1));
    red.remove();
    await run(tween(root, duration: 1));
    root.clear();
    await run(tween(root, duration: 1));
    root.add(stack(width: 20, height: 20, background: "#ffff00"));
    await run(tween(root, duration: 1));
}"##;

    fn wait(driver: &mut AsideSessionUi) {
        for _ in 0..10000 {
            match driver.tick(SliceBudget::default(), 256, || 0).unwrap() {
                StoryProgress::Vm(RunOutcome::Waiting(_)) if !driver.activities_pending() => return,
                StoryProgress::Faulted(_) => panic!("{:?}", driver.fault().unwrap()),
                StoryProgress::HostFailed(_) => panic!("{:?}", driver.host_failure()),
                StoryProgress::Finished => panic!("ended before expected wait"),
                _ => {}
            }
        }
        panic!("expected animation wait");
    }
    type Fill = ([u8; 4], [f64; 6]);
    fn frames(story: CompiledStory) -> Vec<Vec<Fill>> {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        wait(&mut driver);
        let node_id = driver.adapter().stage().root();
        let widget = driver.widget().prepare();
        let root_id = widget.id();
        let root = RenderRoot::new(
            widget,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(640, 420),
                scale_factor: 1.,
                default_font: None,
            },
        );
        let mut bridge = PresentationBridge::new(
            root,
            SurfaceEpoch(1),
            BridgeLimits {
                geometry: GeometryLimits { max_bytes: 1 << 20 },
                max_candidates: 4,
                max_bridge_bytes: 64 << 10,
                max_snapshot_bytes: 8 << 20,
            },
        )
        .unwrap();
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 32,
                max_bytes: 32 << 20,
            },
        );
        let mut frames = Vec::new();
        for second in 0..5 {
            if second > 0 {
                wait(&mut driver);
            }
            assert_eq!(driver.adapter().stage().root(), node_id);
            driver.sync(&mut bridge, root_id).unwrap();
            let (recording, receipt) = driver
                .record(&mut bridge, root_id, FrameId(second + 1))
                .unwrap()
                .expect("settled frame");
            let key = recording.key();

            let mut build = FrameBuild::new(
                recording,
                ResourceSnapshot::capture(std::iter::empty(), &pool).unwrap(),
                FrameInfo {
                    logical_size: [640., 420.],
                    physical_size: [640, 420],
                },
                BuildLimits { max_bytes: 8 << 20 },
                &pool,
            )
            .unwrap();
            while !build
                .step(StepBudget {
                    max_commands: 64,
                    max_units: 8192,
                    max_bytes: 64 << 10,
                })
                .unwrap()
                .complete
            {}
            let frame = build.finish().unwrap();
            let fills = frame
                .commands()
                .iter()
                .filter_map(|command| match &command.op {
                    FrameOp::Draw(FrameDraw::Fill {
                        transform,
                        brush: Brush::Solid(color),
                        ..
                    }) if [
                        [255, 0, 0, 255],
                        [0, 0, 255, 255],
                        [0, 255, 0, 255],
                        [255, 255, 0, 255],
                    ]
                    .contains(&color.to_rgba8().to_u8_array()) =>
                    {
                        Some((color.to_rgba8().to_u8_array(), transform.as_coeffs()))
                    }
                    _ => None,
                })
                .collect();
            frames.push(fills);
            assert_eq!(
                bridge.resolve(PresentationNotice::new(
                    key,
                    SurfaceEpoch(1),
                    PresentationOutcome::Accepted(FrameId(second + 1))
                )),
                NoticeResult::Accepted
            );
            driver.presented(&bridge, &receipt).unwrap();
            assert!(driver.advance_time((second + 1) as f64, 64).unwrap().woke);
        }
        for _ in 0..10000 {
            match driver.tick(SliceBudget::default(), 256, || 0).unwrap() {
                StoryProgress::Finished => return frames,
                StoryProgress::Faulted(_) | StoryProgress::HostFailed(_) => panic!("ending failed"),
                _ => {}
            }
        }
        panic!("story did not finish");
    }

    #[test]
    fn source_and_asb_publish_the_same_dynamic_child_order_and_removal() {
        let checked = reel_cli::check(SOURCE, "dynamic-stage.aside").unwrap();
        let built = reel_cli::build(&checked, Some("main::main")).unwrap();
        let direct = frames(reel_cli::into_story(checked, "main::main").unwrap());
        assert_eq!(
            direct,
            frames(reel_cli::decode_story(&built.bytes).unwrap())
        );
        let red = [255, 0, 0, 255];
        let green = [0, 255, 0, 255];
        let blue = [0, 0, 255, 255];
        let yellow = [255, 255, 0, 255];
        for (actual, expected) in direct.iter().zip([
            vec![red, blue],
            vec![red, green, blue],
            vec![green, blue],
            vec![],
            vec![yellow],
        ]) {
            assert_eq!(actual.iter().map(|v| v.0).collect::<Vec<_>>(), expected);
            for (i, (_, transform)) in actual.iter().enumerate() {
                assert_eq!(*transform, [1., 0., 0., 1., 10. + 20. * i as f64, 10.]);
            }
        }
    }
}
