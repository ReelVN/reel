//! Modal controls and focus restoration.
use super::*;

#[test]
fn source_and_asb_modal_menu_isolates_dialogue_and_restores_keyboard_focus() {
    const MODAL: &str = include_str!("../../../../examples/author_modal/src/main.aside");
    let checked = reel_cli::check(MODAL, "author_modal.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
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
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
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
fn source_and_asb_composed_dropdown_confirms_or_cancels_and_restores_its_trigger() {
    use keyboard::NamedKey::*;
    const POPUP: &str = include_str!("../../../../examples/author_popup/src/main.aside");
    let checked = reel_cli::check(POPUP, "author_popup.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_engine::aside::source::compile_story(POPUP, "main", 1, "main").unwrap(),
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
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
