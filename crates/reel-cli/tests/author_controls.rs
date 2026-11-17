//! Author controls use the same state, geometry and VM tasks from source and ASB.
#[path = "support/author.rs"]
mod support;
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{
        VmLimits,
        source::{CompiledStory, compile_story},
    },
};
use reel_player::aside_session::AsideSessionUi;
use reel_ui::{core::*, dpi::PhysicalPosition, stage::NodeKind};
use support::*;

const SOURCE: &str = r#"
fn set_value(target: any, value: any) { target.value = value; }
fn main() {
    let changes = text("changes", x: 0, y: 200, height: 20);
    let speed = slider(value: 3, min: 3, max: 13, step: 2, width: 220, height: 40,
        on_change: |event| {
            if event.target.value != event.value { throw "value must commit before callback"; }
            changes.x += 1;
            if event.value == 13 { event.target.max = 23; }
        });
    let enabled = toggle("Enabled", y: 60, width: 220, height: 40,
        on_change: |event| {
            if event.target.value != event.value { throw "toggle must commit before callback"; }
            changes.x += 1;
        });
    let mut failures = 0;
    try { speed.value = 100; } catch e { failures += 1; }
    try { speed.min = 4; } catch e { failures += 1; }
    try { speed.max = 3; } catch e { failures += 1; }
    try { speed.step = 0; } catch e { failures += 1; }
    try { set_value(enabled, 1); } catch e { failures += 1; }
    if failures != 5 || speed.value != 3 || speed.min != 3 || speed.max != 13 || speed.step != 2 {
        throw "failed property write changed the control";
    }
    let reset = button("Reset", y: 120, width: 220, height: 40, on_click: |event| {
        speed.value = 8;
        enabled.value = false;
    });
    stage.replace(stack(width: 640, height: 420, children: [speed, enabled, reset, changes]));
    narrator("Controls must consume their input without advancing this dialogue.");
}
"#;
fn stories(source: &str) -> [CompiledStory; 2] {
    let checked = reel_cli::check(source, "controls.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    [
        compile_story(source, "main", 1, "main").unwrap(),
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
    ]
}
fn driver(story: CompiledStory) -> AsideSessionUi {
    let mut driver = AsideSessionUi::from_compiled(
        Session::new(SessionId(1), Epoch(1)).unwrap(),
        story,
        "en",
        VmLimits::default(),
    )
    .unwrap();
    settle(&mut driver);
    driver
}
fn values(driver: &AsideSessionUi) -> (f64, bool, f64) {
    let stage = driver.adapter().stage();
    let value = stage
        .nodes()
        .find_map(|n| {
            if let NodeKind::Slider { value, .. } = n.kind {
                Some(value)
            } else {
                None
            }
        })
        .unwrap();
    let toggle = stage
        .nodes()
        .find_map(|n| {
            if let NodeKind::Toggle { value, .. } = n.kind {
                Some(value)
            } else {
                None
            }
        })
        .unwrap();
    let changes = stage
        .nodes()
        .find(|n| n.kind == NodeKind::Text("changes".into()))
        .unwrap()
        .props
        .x;
    (value, toggle, changes)
}

#[test]
fn children_spread_forwards_components_in_order_and_skips_empty_items() {
    let source = r#"
fn panel(children = []) {
    let state = model({calls: 0});
    fn get_children() {
        state.calls += 1;
        children
    }
    let result = column {
        text("Before");
        if true { ...get_children(); }
        ...(null, text("Tuple"));
        try { ...[1]; } catch error {}
        text("After");
    };
    if state.calls != 1 { throw "children spread evaluated its source again"; }
    result
}
fn main() {
    let view: Node = panel { text("From caller"); };
    stage.replace(view);
    await completion(owner: view);
}
"#;
    for story in stories(source) {
        let driver = driver(story);
        let stage = driver.adapter().stage();
        let root = stage.node(stage.root().unwrap()).unwrap();
        let labels = root
            .children
            .iter()
            .map(|&id| {
                let NodeKind::Text(value) = &stage.node(id).unwrap().kind else {
                    panic!("expected a text child");
                };
                value.as_ref()
            })
            .collect::<Vec<_>>();
        assert_eq!(labels, ["Before", "From caller", "Tuple", "After"]);
    }
}

#[test]
fn children_spread_reports_invalid_context_and_non_sequence_sources() {
    use aside_compiler::DiagnosticCode;

    for (source, code) in [
        ("fn main(){ ...[]; }", DiagnosticCode::InvalidControl),
        (
            "fn main(){ stage.replace(column { let body = || { ...[]; }; }); }",
            DiagnosticCode::InvalidControl,
        ),
        (
            "fn main(){ stage.replace(column { ...1; }); }",
            DiagnosticCode::TypeMismatch,
        ),
    ] {
        let report = reel_cli::check(source, "spread.aside")
            .err()
            .expect("invalid children spread");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.diagnostic.code == code),
            "{report:?}"
        );
    }
}

#[test]
fn source_and_asb_progress_bar_keeps_children_and_tracks_a_read_only_binding() {
    let source = r#"
fn main() {
    let state = model({loaded: 2});
    let bar = progress_bar(value: derive(|| state.loaded), max: 8, y: 80, width: 160, height: 20) {
        stack(width: 10, height: 10);
    };
    let mut rejected = false;
    try { bar.max = 0; } catch e { rejected = true; }
    if !rejected || bar.max != 8 { throw "invalid max changed the bar"; }
    let root = stack(width: 640, height: 420) {
        button("Finish", width: 120, height: 40, on_click: || { state.loaded = 10; });
        bar;
    };
    stage.replace(root);
    await completion(owner: root);
}
"#;
    let measure = |driver: &AsideSessionUi| {
        let (bar, value) = driver
            .adapter()
            .stage()
            .nodes()
            .find_map(|node| match node.kind {
                NodeKind::ProgressBar { value, max: 8. } => Some((node, value)),
                _ => None,
            })
            .unwrap();
        (bar.id, bar.children[0], value)
    };
    for story in stories(source) {
        let mut driver = driver(story);
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        let (id, child, value) = measure(&driver);
        assert_eq!(value, 2.);
        click_at(&mut driver, &mut bridge, 40., 20.);
        settle(&mut driver);
        present(&mut driver, &mut bridge, root, 2);
        assert_eq!(measure(&driver), (id, child, 8.));
    }
}

fn pointer(kind: PointerType, x: f64, down: Option<bool>) -> PointerEvent {
    let pointer = PointerInfo {
        pointer_id: Some(PointerId::PRIMARY),
        persistent_device_id: None,
        pointer_type: kind,
    };
    let state = PointerState {
        position: PhysicalPosition::new(x, 20.),
        ..Default::default()
    };
    match down {
        Some(down) => {
            let event = PointerButtonEvent {
                pointer,
                state,
                button: (kind == PointerType::Mouse).then_some(PointerButton::Primary),
            };
            if down {
                PointerEvent::Down(event)
            } else {
                PointerEvent::Up(event)
            }
        }
        None => PointerEvent::Move(PointerUpdate {
            pointer,
            current: state,
            coalesced: vec![],
            predicted: vec![],
        }),
    }
}
#[test]
fn source_and_asb_controls_keep_dragging_and_commit_values_before_callbacks() {
    // The same contract is exercised by distinct mouse and touch contacts.
    for kind in [PointerType::Mouse, PointerType::Touch] {
        for story in stories(SOURCE) {
            let mut driver = driver(story);
            let (mut bridge, root) = bridge(&driver);
            present(&mut driver, &mut bridge, root, 1);
            let interaction = driver.ui().session().view().interaction.unwrap().id();
            driver
                .handle_pointer(&mut bridge, pointer(kind, 110., Some(true)))
                .unwrap();
            settle(&mut driver);
            present(&mut driver, &mut bridge, root, 2);
            assert_eq!(values(&driver), (9., false, 1.));
            driver
                .handle_pointer(&mut bridge, pointer(kind, 150., None))
                .unwrap();
            settle(&mut driver);
            present(&mut driver, &mut bridge, root, 3);
            assert_eq!(values(&driver), (11., false, 2.));
            driver
                .handle_pointer(&mut bridge, pointer(kind, 210., None))
                .unwrap();
            settle(&mut driver);
            // Handler changes range. Sync cancels the old capture before accepting its new frame.
            driver.sync(&mut bridge, root).unwrap();
            driver
                .handle_pointer(&mut bridge, pointer(kind, 110., None))
                .unwrap();
            settle(&mut driver);
            present(&mut driver, &mut bridge, root, 4);
            assert_eq!(values(&driver), (13., false, 3.));
            driver
                .handle_pointer(&mut bridge, pointer(kind, 110., None))
                .unwrap();
            driver
                .handle_pointer(&mut bridge, pointer(kind, 110., Some(false)))
                .unwrap();
            settle(&mut driver);
            present(&mut driver, &mut bridge, root, 5);
            assert_eq!(values(&driver), (13., false, 3.));
            click_with_pointer(&mut driver, &mut bridge, 50., 80., kind);
            present(&mut driver, &mut bridge, root, 6);
            assert_eq!(values(&driver), (13., true, 4.));
            click_at(&mut driver, &mut bridge, 50., 140.);
            present(&mut driver, &mut bridge, root, 7);
            assert_eq!(
                values(&driver),
                (8., false, 4.),
                "programmatic updates are silent and preserve non-step values"
            );
            // Focus slider at the current displayed center, then use controller navigation.
            click_at(&mut driver, &mut bridge, 60., 20.);
            present(&mut driver, &mut bridge, root, 8);
            gamepad_press(
                &mut driver,
                &mut bridge,
                reel_ui::input::GamepadButton::DpadRight,
            );
            present(&mut driver, &mut bridge, root, 9);
            assert_eq!(values(&driver).0, 11.);
            let target = bridge.root().focused_widget().unwrap().into();
            driver
                .handle_access(
                    &mut bridge,
                    reel_ui::accesskit::ActionRequest {
                        action: reel_ui::accesskit::Action::SetValue,
                        target_tree: reel_ui::accesskit::TreeId::ROOT,
                        target_node: target,
                        data: Some(reel_ui::accesskit::ActionData::NumericValue(19.)),
                    },
                )
                .unwrap();
            settle(&mut driver);
            present(&mut driver, &mut bridge, root, 10);
            assert_eq!(values(&driver).0, 19.);
            assert_eq!(
                driver.ui().session().view().interaction.unwrap().id(),
                interaction
            );
            driver.cancel().unwrap();
        }
    }
}

mod author_bindings {
    //! One source/ASB workflow covers aliasing, transactions and real control input.
    use super::support;
    use reel_engine::{
        Epoch, Session, SessionId,
        aside::{VmLimits, source::compile_story},
    };
    use reel_player::aside_session::AsideSessionUi;
    use reel_ui::{core::PointerType, stage::NodeKind};
    use support::*;

    const SOURCE: &str = r##"
use reel::prelude::*;
use reel::prelude::bind as link;
fn main() {
    let source = slider(value: 40, max: 100, width: 220, height: 40);
    let peer = slider(value: bind(source.value), max: 80, y: 60, width: 220, height: 40);
    let fill = stack(width: bind(peer.value), height: 20, y: 120, background: "#00ff00");
    let enabled = toggle("Enabled", value: true, y: 160, width: 220, height: 40);
    let panel = text("panel", visible: bind(enabled.value), y: 220, height: 20);
    let root = stack(width: 640, height: 400, children: [source, peer, fill, enabled, panel]);
    stage.replace(root);
    let mut receivers = 0;
    let mut keys = 0;
    fn receiver() { receivers += 1; source }
    fn key() { keys += 1; "value" }
    let reference = link(receiver()[key()]);
    peer.value = reference;
    fill.width = 60;
    if source.value != 60 || peer.value != 60 || receivers != 1 || keys != 1 { throw "location or write-through"; }
    let mut failures = 0;
    try { source.value = 90; } catch e { failures += 1; }
    try { source.value = bind(peer.value); } catch e { failures += 1; }
    try { peer.value = bind(enabled.value); } catch e { failures += 1; }
    if failures != 3 || source.value != 60 || peer.value != 60 || fill.width != 60 { throw "atomic failure"; }
    if unbind(bind(peer.value)) != 60 { throw "unbind result"; }
    source.value = 70;
    if peer.value != 60 || fill.width != 60 { throw "unbind chain"; }
    peer.value = reference;
    source.value = 40;
    root.add(button("Retire source", y: 280, width: 220, height: 40, on_click: |event| {
        let old = peer.value;
        source.remove();
        if peer.value != old || fill.width != old { throw "retirement freeze"; }
        let mut rejected = false;
        try { peer.value = reference; } catch e { rejected = true; }
        if !rejected { throw "retired reference"; }
        peer.value = 25;
        if fill.width != 25 { throw "surviving chain"; }
    }));
    narrator("Bindings keep this scene synchronized without callbacks.");
}
"##;

    #[test]
    fn stable_bindings_drive_controls_from_source_and_asb() {
        let checked = reel_cli::check(SOURCE, "bindings.aside").unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        for story in [
            compile_story(SOURCE, "main", 1, "main").unwrap(),
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
            present(&mut driver, &mut bridge, root, 1);
            click_at(&mut driver, &mut bridge, 110., 80.);
            let values: Vec<_> = driver
                .adapter()
                .stage()
                .nodes()
                .filter_map(|n| match n.kind {
                    NodeKind::Slider { value, .. } => Some(value),
                    _ => None,
                })
                .collect();
            assert_eq!(values, [40., 40.]);
            // A changed peer updates the source and a third, non-control node even
            // with no on_change callback and no runnable story task.
            present(&mut driver, &mut bridge, root, 2);
            click_with_pointer(&mut driver, &mut bridge, 210., 80., PointerType::Touch);
            let stage = driver.adapter().stage();
            assert!(
                stage
                    .nodes()
                    .filter_map(|n| match n.kind {
                        NodeKind::Slider { value, .. } => Some(value),
                        _ => None,
                    })
                    .all(|v| v == 80.)
            );
            assert!(
                stage
                    .nodes()
                    .any(|n| n.props.y == 120. && n.props.width == Some(80.))
            );
            present(&mut driver, &mut bridge, root, 3);
            // The source slider permits 100, but its connected peer permits only 80.
            // A rejected user proposal must not fail the session or remain painted.
            click_at(&mut driver, &mut bridge, 210., 20.);
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .filter_map(|n| match n.kind {
                        NodeKind::Slider { value, .. } => Some(value),
                        _ => None,
                    })
                    .all(|v| v == 80.)
            );
            present(&mut driver, &mut bridge, root, 4);
            click_at(&mut driver, &mut bridge, 30., 180.);
            assert!(
                !driver
                    .adapter()
                    .stage()
                    .nodes()
                    .find(|n| n.kind == NodeKind::Text("panel".into()))
                    .unwrap()
                    .props
                    .visible
            );
            present(&mut driver, &mut bridge, root, 5);
            gamepad_press(
                &mut driver,
                &mut bridge,
                reel_ui::input::GamepadButton::South,
            );
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .find(|n| n.kind == NodeKind::Text("panel".into()))
                    .unwrap()
                    .props
                    .visible
            );
            present(&mut driver, &mut bridge, root, 6);
            click_at(&mut driver, &mut bridge, 30., 300.);
            driver.sync(&mut bridge, root).unwrap();
            assert_eq!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .filter(|n| matches!(n.kind, NodeKind::Slider { .. }))
                    .count(),
                1
            );
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|n| n.props.y == 120. && n.props.width == Some(25.))
            );
            assert!(driver.ui().session().view().interaction.is_some());
        }
    }
}

mod author_control_states {
    //! Input ownership, read-only feedback and authored visuals through the real UI/VM.
    use super::support;
    use reel_engine::{
        Epoch, Session, SessionId,
        aside::{VmLimits, source::compile_story},
    };
    use reel_player::aside_session::AsideSessionUi;
    use reel_ui::{core::*, dpi::PhysicalPosition, input::*, stage::NodeKind};
    use support::*;

    const SOURCE: &str = r##"
fn main() {
    fn set_state(target: any, key: string, value: any) { target[key] = value; }
    let count = text("count", y: 180);
    let control = button("Activate", width: 120, height: 60, decorated: false,
        on_click: |event| {
            if event.target.pressed { throw "callback must observe release"; }
            count.x += 1;
        });
    control.add(stack(width: 120, height: 60, background: "#ff0000", visible: bind(control.pressed)));
    let parent = stack(width: 120, height: 60) { control; };
    let mut failures = 0;
    try { set_state(control, "pressed", true); } catch e { failures += 1; }
    try { set_state(control, "hover", bind(control.enabled)); } catch e { failures += 1; }
    try { unbind(bind(control.focused)); } catch e { failures += 1; }
    try { let _ = toggle(value: bind(control.pressed)); } catch e { failures += 1; }
    if failures != 4 { throw "interaction permissions"; }
    stage.replace(stack(width: 640, height: 400) {
        parent;
        button("Enable", x: 150, width: 100, height: 60, on_click: |e| { parent.enabled = !parent.enabled; });
        button("Show", x: 280, width: 100, height: 60, on_click: |e| { parent.visible = !parent.visible; });
        button("Remove", x: 410, width: 100, height: 60, on_click: |e| { control.remove(); });
        text("hover", visible: bind(control.hover), y: 100);
        text("pressed", visible: bind(control.pressed), y: 120);
        text("focused", visible: bind(control.focused), y: 140);
        text("disabled", visible: bind(control.disabled), y: 160);
        count;
    });
    narrator("Read-only state drives authored visuals without callbacks.");
}
"##;

    fn visible(driver: &AsideSessionUi, name: &str) -> bool {
        driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Text(name.into()))
            .unwrap()
            .props
            .visible
    }
    fn clicks(driver: &AsideSessionUi) -> f64 {
        driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Text("count".into()))
            .unwrap()
            .props
            .x
    }
    fn pointer(kind: PointerType, down: Option<bool>) -> PointerEvent {
        let pointer = PointerInfo {
            pointer_id: Some(PointerId::PRIMARY),
            persistent_device_id: None,
            pointer_type: kind,
        };
        let state = PointerState {
            position: PhysicalPosition::new(40., 25.),
            ..Default::default()
        };
        match down {
            Some(down) => {
                let event = PointerButtonEvent {
                    pointer,
                    state,
                    button: (kind == PointerType::Mouse).then_some(PointerButton::Primary),
                };
                if down {
                    PointerEvent::Down(event)
                } else {
                    PointerEvent::Up(event)
                }
            }
            None => PointerEvent::Move(PointerUpdate {
                pointer,
                current: state,
                coalesced: vec![],
                predicted: vec![],
            }),
        }
    }

    #[test]
    fn source_and_asb_interaction_feedback_survives_presentation_and_cancels_with_ownership() {
        let checked = reel_cli::check(SOURCE, "control_states.aside").unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        for story in [
            compile_story(SOURCE, "main", 1, "main").unwrap(),
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
            let mut show =
                |driver: &mut AsideSessionUi, bridge: &mut reel_ui::bridge::PresentationBridge| {
                    frame += 1;
                    present(driver, bridge, root, frame);
                };
            show(&mut driver, &mut bridge);
            for kind in [PointerType::Mouse, PointerType::Touch] {
                driver
                    .handle_pointer(&mut bridge, pointer(kind, None))
                    .unwrap();
                assert!(visible(&driver, "hover"));
                if kind == PointerType::Mouse {
                    let before = driver.adapter().vm().heap().stats();
                    let revision = driver.adapter().stage().revision();
                    let started = std::time::Instant::now();
                    for _ in 0..2_000 {
                        driver
                            .handle_pointer(&mut bridge, pointer(kind, None))
                            .unwrap();
                    }
                    let after = driver.adapter().vm().heap().stats();
                    assert_eq!(after.chunk_allocations, before.chunk_allocations);
                    assert_eq!(after.charged_bytes, before.charged_bytes);
                    assert_eq!(driver.adapter().stage().revision(), revision);
                    eprintln!(
                        "2000 unchanged hover events: {:?}; VM allocation and stage revision unchanged",
                        started.elapsed()
                    );
                }
                driver
                    .handle_pointer(&mut bridge, pointer(kind, Some(true)))
                    .unwrap();
                assert!(visible(&driver, "pressed"));
                assert!(visible(&driver, "focused"));
                // A newly presented pressed appearance must preserve the activation.
                show(&mut driver, &mut bridge);
                driver
                    .handle_pointer(&mut bridge, pointer(kind, Some(false)))
                    .unwrap();
                assert!(!visible(&driver, "pressed"));
                settle(&mut driver);
                driver.sync(&mut bridge, root).unwrap();
                show(&mut driver, &mut bridge);
            }
            assert_eq!(clicks(&driver), 2.);
            for state in [keyboard::KeyState::Down, keyboard::KeyState::Up] {
                driver
                    .handle_text(
                        &mut bridge,
                        TextEvent::Keyboard(keyboard::KeyboardEvent {
                            key: keyboard::Key::Character(" ".into()),
                            code: keyboard::Code::Space,
                            state,
                            ..Default::default()
                        }),
                    )
                    .unwrap();
                assert_eq!(
                    visible(&driver, "pressed"),
                    state == keyboard::KeyState::Down
                );
                show(&mut driver, &mut bridge);
            }
            settle(&mut driver);
            driver.sync(&mut bridge, root).unwrap();
            assert_eq!(clicks(&driver), 3.);
            show(&mut driver, &mut bridge);
            gamepad_press(&mut driver, &mut bridge, GamepadButton::South);
            driver.sync(&mut bridge, root).unwrap();
            assert_eq!(clicks(&driver), 4.);
            show(&mut driver, &mut bridge);
            driver
                .handle_text(
                    &mut bridge,
                    TextEvent::Navigation(ActionEvent {
                        action: NavigationAction::Confirm,
                        phase: ActionPhase::Pressed,
                        value: 1.,
                    }),
                )
                .unwrap();
            assert!(visible(&driver, "pressed"));
            driver
                .handle_text(&mut bridge, TextEvent::WindowFocusChange(false))
                .unwrap();
            assert!(!visible(&driver, "pressed"));
            assert!(!visible(&driver, "focused"));
            driver
                .handle_text(&mut bridge, TextEvent::WindowFocusChange(true))
                .unwrap();
            show(&mut driver, &mut bridge);
            // A second contact disables/hides the first contact's active control.
            for (x, disabled) in [(180., true), (310., false)] {
                driver
                    .handle_pointer(&mut bridge, pointer(PointerType::Mouse, Some(true)))
                    .unwrap();
                assert!(visible(&driver, "pressed"));
                click_with_pointer(&mut driver, &mut bridge, x, 25., PointerType::Touch);
                driver.sync(&mut bridge, root).unwrap();
                assert!(!visible(&driver, "pressed"));
                assert!(!visible(&driver, "focused"));
                assert_eq!(visible(&driver, "disabled"), disabled);
                driver
                    .handle_pointer(&mut bridge, pointer(PointerType::Mouse, Some(false)))
                    .unwrap();
                settle(&mut driver);
                assert_eq!(clicks(&driver), 4.);
                show(&mut driver, &mut bridge);
                click_at(&mut driver, &mut bridge, x, 25.);
                show(&mut driver, &mut bridge);
            }
            driver
                .handle_pointer(&mut bridge, pointer(PointerType::Mouse, Some(true)))
                .unwrap();
            assert!(visible(&driver, "pressed"));
            click_with_pointer(&mut driver, &mut bridge, 440., 25., PointerType::Touch);
            driver.sync(&mut bridge, root).unwrap();
            assert!(!visible(&driver, "pressed"));
            assert!(!visible(&driver, "focused"));
            assert!(!visible(&driver, "hover"));
        }
    }

    #[test]
    fn derived_pressed_visual_preserves_mouse_and_touch_activation() {
        let source = SOURCE.replace(
            "visible: bind(control.pressed)",
            "visible: derive(|| control.pressed)",
        );
        let checked = reel_cli::check(&source, "derived_controls.aside").unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        for story in [
            compile_story(&source, "main", 1, "main").unwrap(),
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
            let mut frame = 1;
            present(&mut driver, &mut bridge, root, frame);
            for kind in [PointerType::Mouse, PointerType::Touch] {
                driver
                    .handle_pointer(&mut bridge, pointer(kind, Some(true)))
                    .unwrap();
                settle(&mut driver);
                frame += 1;
                present(&mut driver, &mut bridge, root, frame);
                driver
                    .handle_pointer(&mut bridge, pointer(kind, Some(false)))
                    .unwrap();
                settle(&mut driver);
                frame += 1;
                present(&mut driver, &mut bridge, root, frame);
            }
            assert_eq!(clicks(&driver), 2.);
        }
    }

    #[test]
    fn author_narrative_example_initializes_from_source_and_asb() {
        let source = include_str!("../../../examples/author_narrative/src/main.aside");
        let checked = reel_cli::check(source, "author_narrative.aside").unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        for story in [
            compile_story(source, "main", 1, "main").unwrap(),
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
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|node| node.kind == NodeKind::Text("雨还没有停。".into()))
            );
        }
    }
}

mod author_selection {
    //! Shared business values keep author-owned controls exclusive from source and ASB.
    use super::support;
    use reel_engine::{
        Epoch, Session, SessionId,
        aside::{VmLimits, source::compile_story},
    };
    use reel_player::aside_session::AsideSessionUi;
    use reel_ui::{
        core::*,
        stage::{NodeId, NodeKind, ToggleAppearance},
    };
    use support::*;

    const SOURCE: &str = r##"
fn dynamic(value: any) -> any { value }
fn main() {
    let data = model({enabled: false, language: "en", extra: "fr", modal: false});
    let count = text("changes", x: 0, y: 380);
    let changed = |event| {
        if event.target.value != event.value { throw "callback observed uncommitted value"; }
        count.x += 1;
    };
    let enabled = checkbox("Enabled", width: 220, height: 40, value: bind(data.enabled), on_change: changed);
    let chinese = radio("zh-CN", "Chinese", y: 60, width: 220, height: 40, value: bind(data.language), on_change: changed);
    let english = radio("e" + "n", "English", y: 110, width: 220, height: 40, value: bind(data.language), on_change: changed);
    let extra = radio(bind(data.extra), "Extra", y: 160, width: 220, height: 40, value: bind(data.language), on_change: changed);
    let disabled = radio("disabled", "Disabled", y: 210, width: 220, height: 40, enabled: false, value: bind(data.language), on_change: changed);
    let reset = button("Reset", x: 300, y: 0, width: 220, height: 40, on_click: |e| {
        data.enabled = false; data.language = "en"; data.extra = "de";
    });
    let custom = checkbox(value: bind(data.enabled), x: 300, y: 60, width: 220, height: 40, decorated: false, on_change: changed);
    custom.add(text("Custom", color: derive(|| if custom.value { "#00ff00" } else { "#ffffff" })));
    let retire = button("Remove", x: 300, y: 110, width: 220, height: 40, on_click: |e| { extra.remove(); });
    let overlay = stack(width: 640, height: 420, modal: true, visible: bind(data.modal)) {
        button("Close", width: 100, height: 40, on_click: |e| { data.modal = false; });
    };
    let open = button("Modal", x: 300, y: 160, width: 220, height: 40, on_click: |e| { data.modal = true; });
    let mut rejected = 0;
    try { let _ = checkbox(value: derive(|| data.enabled)); } catch e { rejected += 1; }
    try { let _ = radio("en", value: derive(|| data.language)); } catch e { rejected += 1; }
    try { enabled.value = dynamic(1); } catch e { rejected += 1; }
    if rejected != 3 { throw "writable value contract"; }
    stage.replace(stack(children: [enabled, chinese, english, extra, disabled, reset, custom, retire, open, count, overlay]));
    narrator("Control input must not advance this line.");
}
"##;

    fn drivers(source: &str) -> Vec<AsideSessionUi> {
        let checked = reel_cli::check(source, "selection.aside").unwrap();
        let bytes = reel_cli::build(&checked, Some("main::main")).unwrap().bytes;
        [
            compile_story(source, "main", 1, "main").unwrap(),
            reel_player::distribution::decode_story(&bytes).unwrap(),
        ]
        .into_iter()
        .map(|story| {
            let mut d = AsideSessionUi::from_compiled(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                story,
                "en",
                VmLimits::default(),
            )
            .unwrap();
            settle(&mut d);
            d
        })
        .collect()
    }
    fn radio(driver: &AsideSessionUi, label: &str) -> (NodeId, bool, u64) {
        driver
            .adapter()
            .stage()
            .nodes()
            .find_map(|n| match &n.kind {
                NodeKind::Radio {
                    label: Some(name),
                    selected,
                    revision,
                    ..
                } if name == label => Some((n.id, *selected, *revision)),
                _ => None,
            })
            .unwrap()
    }
    fn count(driver: &AsideSessionUi) -> f64 {
        driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Text("changes".into()))
            .unwrap()
            .props
            .x
    }
    fn checked(driver: &AsideSessionUi) -> Vec<bool> {
        driver
            .adapter()
            .stage()
            .nodes()
            .filter_map(|n| match n.kind {
                NodeKind::Toggle {
                    value,
                    appearance: ToggleAppearance::Checkbox,
                    ..
                } => Some(value),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn selections_share_values_and_keep_identity_across_input_and_programmatic_changes() {
        for pointer in [PointerType::Mouse, PointerType::Touch] {
            for mut d in drivers(SOURCE) {
                let (mut b, root) = bridge(&d);
                present(&mut d, &mut b, root, 1);
                let interaction = d.ui().session().view().interaction.unwrap().id();
                let chinese = radio(&d, "Chinese").0;
                assert!(radio(&d, "English").1, "strings compare by contents");
                click_with_pointer(&mut d, &mut b, 50., 20., pointer);
                present(&mut d, &mut b, root, 2);
                assert_eq!(checked(&d), [true, true]);
                assert_eq!(count(&d), 1.);
                click_with_pointer(&mut d, &mut b, 50., 80., pointer);
                present(&mut d, &mut b, root, 3);
                assert!(radio(&d, "Chinese").1);
                assert!(!radio(&d, "English").1);
                assert_eq!(count(&d), 2.);
                click_with_pointer(&mut d, &mut b, 50., 80., pointer);
                present(&mut d, &mut b, root, 4);
                assert_eq!(count(&d), 2., "selecting again is silent");
                gamepad_press(&mut d, &mut b, reel_ui::input::GamepadButton::DpadDown);
                gamepad_press(&mut d, &mut b, reel_ui::input::GamepadButton::South);
                present(&mut d, &mut b, root, 5);
                assert!(radio(&d, "English").1);
                assert_eq!(count(&d), 3.);
                let target = b.root().focused_widget().unwrap().into();
                d.handle_access(
                    &mut b,
                    reel_ui::accesskit::ActionRequest {
                        action: reel_ui::accesskit::Action::Click,
                        target_tree: reel_ui::accesskit::TreeId::ROOT,
                        target_node: target,
                        data: None,
                    },
                )
                .unwrap();
                settle(&mut d);
                assert_eq!(
                    count(&d),
                    3.,
                    "accessible activation has the same exclusive behavior"
                );
                click_with_pointer(&mut d, &mut b, 50., 230., pointer);
                assert_eq!(count(&d), 3., "disabled radio cannot choose");
                click_with_pointer(&mut d, &mut b, 350., 20., pointer);
                click_with_pointer(&mut d, &mut b, 50., 180., pointer);
                assert_eq!(
                    count(&d),
                    3.,
                    "old presentation cannot choose a replaced option"
                );
                present(&mut d, &mut b, root, 6);
                assert_eq!(checked(&d), [false, false]);
                assert_eq!(count(&d), 3., "programmatic resets are silent");
                assert_eq!(radio(&d, "Chinese").0, chinese);
                click_with_pointer(&mut d, &mut b, 50., 180., pointer);
                present(&mut d, &mut b, root, 7);
                assert!(radio(&d, "Extra").1, "changed option is selectable");
                assert!(!radio(&d, "English").1);
                click_with_pointer(&mut d, &mut b, 350., 80., pointer);
                present(&mut d, &mut b, root, 8);
                assert_eq!(
                    checked(&d),
                    [true, true],
                    "custom children preserve checkbox behavior"
                );
                assert!(
                    d.adapter()
                        .stage()
                        .nodes()
                        .any(|n| n.kind == NodeKind::Text("Custom".into())
                            && n.props.color == [0, 255, 0, 255])
                );
                click_with_pointer(&mut d, &mut b, 350., 180., pointer);
                present(&mut d, &mut b, root, 9);
                let before = count(&d);
                click_with_pointer(&mut d, &mut b, 50., 80., pointer);
                assert_eq!(count(&d), before, "modal owns input");
                click_with_pointer(&mut d, &mut b, 50., 20., pointer);
                present(&mut d, &mut b, root, 10);
                click_with_pointer(&mut d, &mut b, 350., 130., pointer);
                click_with_pointer(&mut d, &mut b, 50., 180., pointer);
                assert_eq!(count(&d), before, "retired option cannot choose");
                present(&mut d, &mut b, root, 11);
                assert_eq!(
                    d.ui().session().view().interaction.unwrap().id(),
                    interaction
                );
            }
        }
    }
}

mod author_text_area {
    //! Source and actual ASB use the same editor, accepted input and model commits.
    use super::support;
    use reel_engine::{
        Epoch, Session, SessionId,
        aside::{VmLimits, source::compile_story},
    };
    use reel_player::aside_session::AsideSessionUi;
    use reel_ui::{core::*, stage::NodeKind};
    use support::*;

    const SOURCE: &str = r#"
fn dynamic(value: any) -> any { value }
fn main() {
    let notes = model({body: "first\nsecond", changes: 0, submits: 0});
    let editor = text_area(value: bind(notes.body), width: 240, height: 80,
        on_change: |event| {
            if event.value != notes.body || event.target.value != notes.body { throw "commit order"; }
            notes.changes += 1;
        }, on_submit: || { notes.submits += 1; });
    let mut rejected = 0;
    try { let _ = text_area(value: derive(|| notes.body)); } catch e { rejected += 1; }
    try { editor.value = dynamic(1); } catch e { rejected += 1; }
    try { let _ = text_input(value: "two\nlines"); } catch e { rejected += 1; }
    if rejected != 3 { throw "input contract"; }
    stage.replace(column() {
        editor;
        text(derive(|| f"{notes.changes}:{notes.submits}"));
        button("Replace", on_click: || { notes.body = "author\nreplacement"; });
    });
    narrator("Editor input must leave this dialogue waiting.");
}
"#;

    fn drivers(source: &str) -> Vec<AsideSessionUi> {
        let checked = reel_cli::check(source, "text_area.aside").unwrap();
        let bytes = reel_cli::build(&checked, Some("main::main")).unwrap().bytes;
        [
            compile_story(source, "main", 1, "main").unwrap(),
            reel_player::distribution::decode_story(&bytes).unwrap(),
        ]
        .into_iter()
        .map(|story| {
            let mut d = AsideSessionUi::from_compiled(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                story,
                "en",
                VmLimits::default(),
            )
            .unwrap();
            settle(&mut d);
            d
        })
        .collect()
    }
    fn key(key: keyboard::Key, modifiers: keyboard::Modifiers) -> TextEvent {
        TextEvent::Keyboard(keyboard::KeyboardEvent {
            state: keyboard::KeyState::Down,
            key,
            code: keyboard::Code::Unidentified,
            location: keyboard::Location::Standard,
            modifiers,
            repeat: false,
            is_composing: false,
        })
    }
    fn state(d: &AsideSessionUi) -> (String, String) {
        let nodes = d.adapter().stage();
        let value = nodes
            .nodes()
            .find_map(|node| match &node.kind {
                NodeKind::TextInput {
                    value,
                    multiline: true,
                    ..
                } => Some(value.to_string()),
                _ => None,
            })
            .unwrap();
        let counts = nodes
            .nodes()
            .find_map(|node| match &node.kind {
                NodeKind::Text(value) => Some(value.to_string()),
                _ => None,
            })
            .unwrap();
        (value, counts)
    }

    #[test]
    fn text_area_source_asb_edit_submit_preedit_cancel_and_navigation() {
        use keyboard::{Key, Modifiers, NamedKey};
        let command = if cfg!(target_os = "macos") {
            Modifiers::META
        } else {
            Modifiers::CONTROL
        };
        for mut d in drivers(SOURCE) {
            let font =
                include_bytes!("../../reel-player/tests/fixtures/font-weight/400.ttf").to_vec();
            let (mut bridge, root) = bridge_with_font(&d, Some(font.into()));
            present(&mut d, &mut bridge, root, 1);
            let interaction = d.ui().session().view().interaction.unwrap().id();
            let mut frame = 1;
            let mut send =
                |d: &mut AsideSessionUi, bridge: &mut reel_ui::bridge::PresentationBridge, e| {
                    d.handle_text(bridge, e).unwrap();
                    settle(d);
                    frame += 1;
                    present(d, bridge, root, frame);
                    assert_eq!(
                        d.ui().session().view().interaction.unwrap().id(),
                        interaction
                    );
                };
            send(
                &mut d,
                &mut bridge,
                key(Key::Named(NamedKey::Tab), Modifiers::empty()),
            );
            let focus = bridge.root().focused_widget();
            send(
                &mut d,
                &mut bridge,
                key(Key::Character("a".into()), command),
            );
            send(
                &mut d,
                &mut bridge,
                TextEvent::ClipboardPaste("one\r\ntwo\rthree".into()),
            );
            assert_eq!(state(&d), ("one\ntwo\nthree".into(), "1:0".into()));
            send(
                &mut d,
                &mut bridge,
                key(Key::Named(NamedKey::Enter), Modifiers::empty()),
            );
            send(
                &mut d,
                &mut bridge,
                TextEvent::Ime(Ime::Commit("four".into())),
            );
            assert_eq!(state(&d), ("one\ntwo\nthree\nfour".into(), "3:0".into()));
            assert_eq!(
                bridge.root().focused_widget(),
                focus,
                "value echo keeps editor identity"
            );
            send(
                &mut d,
                &mut bridge,
                key(Key::Named(NamedKey::Enter), command),
            );
            assert_eq!(state(&d).1, "3:1");

            // Home/End operate on the current visual line; selection crosses line breaks.
            send(
                &mut d,
                &mut bridge,
                key(Key::Named(NamedKey::Home), Modifiers::empty()),
            );
            send(
                &mut d,
                &mut bridge,
                key(Key::Named(NamedKey::End), Modifiers::SHIFT),
            );
            send(
                &mut d,
                &mut bridge,
                TextEvent::Ime(Ime::Preedit("pending".into(), Some((7, 7)))),
            );
            send(
                &mut d,
                &mut bridge,
                key(Key::Named(NamedKey::Enter), Modifiers::empty()),
            );
            assert_eq!(state(&d), ("one\ntwo\nthree\nfour".into(), "3:1".into()));
            send(
                &mut d,
                &mut bridge,
                TextEvent::Ime(Ime::Preedit("".into(), None)),
            );
            send(
                &mut d,
                &mut bridge,
                TextEvent::Ime(Ime::Commit("last".into())),
            );
            assert_eq!(state(&d).0, "one\ntwo\nthree\nlast");

            send(
                &mut d,
                &mut bridge,
                key(Key::Named(NamedKey::Home), command),
            );
            send(
                &mut d,
                &mut bridge,
                key(Key::Named(NamedKey::ArrowDown), Modifiers::empty()),
            );
            send(
                &mut d,
                &mut bridge,
                key(Key::Named(NamedKey::End), Modifiers::SHIFT),
            );
            send(&mut d, &mut bridge, TextEvent::ClipboardPaste("TWO".into()));
            assert_eq!(state(&d).0, "one\nTWO\nthree\nlast");
            send(
                &mut d,
                &mut bridge,
                key(Key::Named(NamedKey::ArrowUp), Modifiers::empty()),
            );
            send(
                &mut d,
                &mut bridge,
                key(Key::Named(NamedKey::Home), Modifiers::SHIFT),
            );
            send(&mut d, &mut bridge, TextEvent::ClipboardPaste("ONE".into()));
            assert_eq!(state(&d).0, "ONE\nTWO\nthree\nlast");

            // Losing focus cancels an unfinished composition; Tab still reaches another control.
            send(
                &mut d,
                &mut bridge,
                TextEvent::Ime(Ime::Preedit("discard".into(), None)),
            );
            send(&mut d, &mut bridge, TextEvent::WindowFocusChange(false));
            send(&mut d, &mut bridge, TextEvent::WindowFocusChange(true));
            assert_eq!(state(&d), ("ONE\nTWO\nthree\nlast".into(), "6:1".into()));
            send(
                &mut d,
                &mut bridge,
                key(Key::Named(NamedKey::Tab), Modifiers::empty()),
            );
            assert_ne!(bridge.root().focused_widget(), focus);
            send(
                &mut d,
                &mut bridge,
                key(Key::Named(NamedKey::Enter), Modifiers::empty()),
            );
            let mut released = key(Key::Named(NamedKey::Enter), Modifiers::empty());
            if let TextEvent::Keyboard(event) = &mut released {
                event.state = keyboard::KeyState::Up;
            }
            send(&mut d, &mut bridge, released);
            assert_eq!(state(&d), ("author\nreplacement".into(), "6:1".into()));
            d.cancel().unwrap();
        }
    }
}
