//! Long author menus run through the VM and accepted UI geometry.
#[path = "support/author.rs"]
mod support;
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{VmLimits, source::compile_story},
};
use reel_player::aside_session::AsideSessionUi;
use reel_ui::{core::*, dpi::PhysicalPosition, input::*, stage::NodeKind};
use support::*;

fn touch(kind: &str, x: f64, y: f64) -> PointerEvent {
    let pointer = PointerInfo {
        pointer_id: Some(PointerId::PRIMARY),
        persistent_device_id: None,
        pointer_type: PointerType::Touch,
    };
    let state = PointerState {
        position: PhysicalPosition::new(x, y),
        ..Default::default()
    };
    match kind {
        "move" => PointerEvent::Move(PointerUpdate {
            pointer,
            current: state,
            coalesced: vec![],
            predicted: vec![],
        }),
        "down" => PointerEvent::Down(PointerButtonEvent {
            pointer,
            state,
            button: None,
        }),
        "up" => PointerEvent::Up(PointerButtonEvent {
            pointer,
            state,
            button: None,
        }),
        _ => PointerEvent::Cancel(pointer),
    }
}
fn wheel(y: f64) -> PointerEvent {
    PointerEvent::Scroll(PointerScrollEvent {
        pointer: PointerInfo {
            pointer_id: Some(PointerId::PRIMARY),
            persistent_device_id: None,
            pointer_type: PointerType::Mouse,
        },
        state: PointerState {
            position: PhysicalPosition::new(80., 80.),
            ..Default::default()
        },
        delta: ScrollDelta::PixelDelta(PhysicalPosition::new(0., y)),
    })
}

#[test]
fn source_and_asb_long_choices_scroll_without_activating_the_dragged_button() {
    let source = include_str!("../../../examples/author_scroll/src/main.aside");
    let checked = reel_cli::check(source, "author_scroll.aside").unwrap();
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
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        // Starting on a button still permits panning. A drag never chooses it.
        driver
            .handle_pointer(&mut bridge, touch("down", 80., 120.))
            .unwrap();
        driver
            .handle_pointer(&mut bridge, touch("move", 80., 60.))
            .unwrap();
        present(&mut driver, &mut bridge, root, 2);
        driver
            .handle_pointer(&mut bridge, touch("move", 80., 30.))
            .unwrap();
        driver
            .handle_pointer(&mut bridge, touch("up", 80., 30.))
            .unwrap();
        settle(&mut driver);
        assert!(driver.ui().session().view().interaction.is_some());
        assert_eq!(
            driver
                .adapter()
                .stage()
                .nodes()
                .filter(|n| matches!(n.kind, NodeKind::Button { .. }))
                .count(),
            20
        );
        present(&mut driver, &mut bridge, root, 3);
        driver.handle_pointer(&mut bridge, wheel(10_000.)).unwrap();
        present(&mut driver, &mut bridge, root, 4);
        driver
            .handle_pointer(&mut bridge, touch("down", 80., 140.))
            .unwrap();
        click_at(&mut driver, &mut bridge, 80., 180.);
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("Selected 19".into()))
        );
        let current = driver.ui().session().view().interaction.unwrap().id();
        present(&mut driver, &mut bridge, root, 5);
        // Replacing the menu retires the first contact's target. Its remainder
        // cannot start a pan on a removed widget or advance the next dialogue.
        driver
            .handle_pointer(&mut bridge, touch("move", 80., 90.))
            .unwrap();
        driver
            .handle_pointer(&mut bridge, touch("up", 80., 90.))
            .unwrap();
        settle(&mut driver);
        assert_eq!(
            driver.ui().session().view().interaction.unwrap().id(),
            current
        );
    }
}

#[test]
fn nested_author_scroll_follows_gamepad_focus_to_the_last_choice() {
    let source = r#"
fn main() {
    game.configure(width: 640, height: 420);
    let choices = game.choice();
    stage.replace(scroll(x: 20, y: 20, width: 320, height: 180) {
        column {
            stack(height: 160);
            scroll(width: 300, height: 160) {
                view(|| column {
                    for option in choices.options {
                        button(option.label, width: 280, height: 40,
                            on_click: |event| choices.select(option.id));
                    }
                });
            }
        }
    });
    let options = [];
    for i in 0..20 { options.append({key: f"{i}", label: f"Choice {i}"}); }
    let selected = choose(options);
    stage.replace(text(f"Selected {selected}", x: 20, y: 20));
    narrator(f"Selected {selected}");
}
"#;
    let story = compile_story(source, "main", 1, "main").unwrap();
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
    for i in 0..20 {
        gamepad_press(&mut driver, &mut bridge, GamepadButton::DpadDown);
        present(&mut driver, &mut bridge, root, i + 2);
        let focused = bridge.root().focused_widget().unwrap();
        let reel_ui::app::InputGeometrySource::Presented(Some(geometry)) =
            bridge.root().input_geometry_source()
        else {
            panic!("presentation");
        };
        let record = geometry.get(focused).unwrap();
        let point = record.window_transform * record.size.to_rect().center();
        assert_eq!(
            geometry.hit_test(point),
            Some(focused),
            "focused choice {i} stays inside both clips"
        );
    }
    gamepad_press(&mut driver, &mut bridge, GamepadButton::South);
    assert!(
        driver
            .adapter()
            .stage()
            .nodes()
            .any(|n| n.kind == NodeKind::Text("Selected 19".into()))
    );
}

#[test]
fn scroll_preserves_exclusive_slider_drag_and_rejects_invalid_axis_writes() {
    let source = r#"
    fn set_follow_end(target: any, value: any) { target.follow_end = value; }
    fn main() {
        let value = slider(min: 0, max: 100, value: 0, width: 280, height: 40);
        let area = scroll(x: 20, y: 20, width: 300, height: 180) {
            value; stack(height: 400);
        };
        let mut errors = 0;
        try { area.axis = "diagonal"; } catch e { errors += 1; }
        try { set_follow_end(area, 2); } catch e { errors += 1; }
        if errors != 2 || area.axis != "vertical" || area.follow_end { throw "invalid scroll config"; }
        stage.replace(area);
        narrator("drag");
    }"#;
    let story = compile_story(source, "main", 1, "main").unwrap();
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
    driver
        .handle_pointer(&mut bridge, touch("down", 40., 35.))
        .unwrap();
    driver
        .handle_pointer(&mut bridge, touch("move", 260., 130.))
        .unwrap();
    present(&mut driver, &mut bridge, root, 2);
    driver
        .handle_pointer(&mut bridge, touch("up", 260., 130.))
        .unwrap();
    settle(&mut driver);
    let value = driver
        .adapter()
        .stage()
        .nodes()
        .find_map(|n| match n.kind {
            NodeKind::Slider { value, .. } => Some(value),
            _ => None,
        })
        .unwrap();
    assert!(value > 80., "exclusive slider drag must continue: {value}");
    // The slider remains at its original displayed position after a diagonal drag.
    present(&mut driver, &mut bridge, root, 3);
    click_with_pointer(&mut driver, &mut bridge, 40., 35., PointerType::Touch);
    let value = driver
        .adapter()
        .stage()
        .nodes()
        .find_map(|n| match n.kind {
            NodeKind::Slider { value, .. } => Some(value),
            _ => None,
        })
        .unwrap();
    assert!(value < 20.);
}

#[test]
fn custom_nvl_taps_advance_but_pans_preserve_the_current_dialogue() {
    let source = r#"fn main() {
        let dialogue = game.dialogue();
        stage.replace(scroll(x: 20, y: 20, width: 300, height: 120, follow_end: true) {
            view(|| column() { for line in dialogue.lines { text(value: bind(line.text), reveal: bind(line.reveal), height: 100); } });
        });
        narrator("first"); narrator("second"); narrator("third");
    }"#;
    let checked = reel_cli::check(source, "nvl_scroll.aside").unwrap();
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
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        // First tap reveals, second advances, even though no default dialogue widget exists.
        for frame in 2..=3 {
            click_with_pointer(&mut driver, &mut bridge, 80., 60., PointerType::Touch);
            present(&mut driver, &mut bridge, root, frame);
        }
        assert_eq!(driver.ui().session().view().history.len(), 2);
        let current = driver.ui().session().view().interaction.unwrap().id();
        driver
            .handle_pointer(&mut bridge, touch("down", 80., 50.))
            .unwrap();
        driver
            .handle_pointer(&mut bridge, touch("move", 80., 105.))
            .unwrap();
        present(&mut driver, &mut bridge, root, 4);
        driver
            .handle_pointer(&mut bridge, touch("up", 80., 105.))
            .unwrap();
        settle(&mut driver);
        assert_eq!(
            driver.ui().session().view().interaction.unwrap().id(),
            current
        );
        let reel_engine::InteractionView::Dialogue(line) =
            driver.ui().session().view().interaction.unwrap()
        else {
            panic!("dialogue");
        };
        assert_eq!(
            line.reveal().revealed_count,
            0,
            "drag must not reveal or advance"
        );
        present(&mut driver, &mut bridge, root, 5);
        for frame in 6..=7 {
            click_at(&mut driver, &mut bridge, 80., 60.);
            present(&mut driver, &mut bridge, root, frame);
        }
        assert_eq!(driver.ui().session().view().history.len(), 3);
    }
}

mod author_scroll_composition {
    //! Scroll feedback and captured local dragging work through source and ASB.
    use super::support;
    use reel_engine::{
        Epoch, Session, SessionId,
        aside::{VmLimits, source::compile_story},
    };
    use reel_player::aside_session::AsideSessionUi;
    use reel_ui::{core::*, dpi::PhysicalPosition, stage::NodeKind};
    use support::*;

    const SOURCE: &str = r#"
fn main() {
    let state = model({position: 0, cancelled: 0, actions: 0});
    let page = scroll(width: 120, height: 100, scroll_y: bind(state.position), follow_end: true, content_align: "end") {
        stack(height: 300, width: 100);
    };
    let info = text("metrics", y: 350, width: derive(|| page.viewport_height), height: derive(|| page.content_height));
    let position = text("position", x: bind(state.position), y: 380);
    let cancelled = text("cancelled", x: bind(state.cancelled), y: 400);
    let actions = text("actions", x: bind(state.actions), y: 420);
    let drag = input(x: 200, y: 20, width: 40, height: 100, scale_y: 2,
        actions: {drag: {bindings: ["PointerPrimary"],
            on_press: || { state.actions += 1; },
            on_release: || { state.actions += 1; }}},
        on_pointer: |e| {
            if e.phase == "cancelled" { state.cancelled += 1; }
            if e.phase == "pressed" || e.phase == "moved" { page.scroll_y = e.y * 2; }
            if e.phase == "pressed" && e.x > 30 { e.target.visible = false; }
        });
    stage.replace(stack() {
        page; info; position; cancelled; actions; drag;
        button("Append", x: 300, y: 60, width: 80, height: 40, on_click: |e| { page.add(stack(height: 100)); });
        button("Clamp", x: 300, y: 120, width: 80, height: 40, on_click: |e| { page.scroll_y = 10000; });
    });
    let mut rejected = false;
    fn set_height(target: any) { target.content_height = 5; }
    try { set_height(page); } catch e { rejected = true; }
    if !rejected { throw "metrics must be readonly"; }
}
"#;
    fn pointer(kind: PointerType, x: f64, y: f64, down: Option<bool>) -> PointerEvent {
        let pointer = PointerInfo {
            pointer_id: Some(PointerId::PRIMARY),
            persistent_device_id: None,
            pointer_type: kind,
        };
        let state = PointerState {
            position: PhysicalPosition::new(x, y),
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
    fn label(driver: &AsideSessionUi, caption: &str) -> reel_ui::stage::NodeProps {
        driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Text(caption.into()))
            .unwrap()
            .props
            .clone()
    }
    fn flush(
        driver: &mut AsideSessionUi,
        bridge: &mut reel_ui::bridge::PresentationBridge,
        root: WidgetId,
        frame: &mut u64,
    ) {
        // Layout feedback is consumed before publishing the binding-dependent next view.
        for _ in 0..3 {
            *frame += 1;
            present(driver, bridge, root, *frame);
            settle(driver);
        }
    }
    #[test]
    fn scroll_feedback_dragging_and_retirement_share_one_authoritative_position() {
        let artifact = reel_cli::build(
            &reel_cli::check(SOURCE, "scroll.aside").unwrap(),
            Some("main::main"),
        )
        .unwrap();
        for kind in [PointerType::Mouse, PointerType::Touch] {
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
                flush(&mut driver, &mut bridge, root, &mut frame);
                assert_eq!(
                    (
                        label(&driver, "metrics").width,
                        label(&driver, "metrics").height
                    ),
                    (Some(100.), Some(300.))
                );
                assert_eq!(label(&driver, "position").x, 200.);
                for (y, expected, down) in [
                    (40., 20., Some(true)),
                    (120., 100., None),
                    (420., 200., None),
                    (120., 100., Some(false)),
                ] {
                    driver
                        .handle_pointer(&mut bridge, pointer(kind, 220., y, down))
                        .unwrap();
                    settle(&mut driver);
                    flush(&mut driver, &mut bridge, root, &mut frame);
                    assert_eq!(
                        label(&driver, "position").x,
                        if down == Some(false) { 200. } else { expected }
                    );
                    if down == Some(true) && kind == PointerType::Mouse {
                        let mut secondary_up = pointer(kind, 220., y, Some(false));
                        if let PointerEvent::Up(e) = &mut secondary_up {
                            e.button = Some(PointerButton::Secondary);
                            e.state.buttons.insert(PointerButton::Primary);
                        }
                        driver.handle_pointer(&mut bridge, secondary_up).unwrap();
                        settle(&mut driver);
                    }
                }
                assert_eq!(label(&driver, "actions").x, 2.);
                // Scroll back, append, then follow the end again, with model binding feedback.
                driver
                    .handle_pointer(
                        &mut bridge,
                        PointerEvent::Scroll(PointerScrollEvent {
                            pointer: PointerInfo {
                                pointer_id: Some(PointerId::PRIMARY),
                                persistent_device_id: None,
                                pointer_type: PointerType::Mouse,
                            },
                            state: PointerState {
                                position: PhysicalPosition::new(40., 40.),
                                ..Default::default()
                            },
                            delta: ScrollDelta::PixelDelta(PhysicalPosition::new(0., -80.)),
                        }),
                    )
                    .unwrap();
                flush(&mut driver, &mut bridge, root, &mut frame);
                assert_eq!(label(&driver, "position").x, 120.);
                click_at(&mut driver, &mut bridge, 330., 80.);
                flush(&mut driver, &mut bridge, root, &mut frame);
                assert_eq!(label(&driver, "position").x, 120.);
                click_at(&mut driver, &mut bridge, 330., 140.);
                flush(&mut driver, &mut bridge, root, &mut frame);
                assert_eq!(label(&driver, "position").x, 300.);
                click_at(&mut driver, &mut bridge, 330., 80.);
                flush(&mut driver, &mut bridge, root, &mut frame);
                assert_eq!(label(&driver, "position").x, 400.);
                driver
                    .handle_pointer(&mut bridge, pointer(kind, 235., 40., Some(true)))
                    .unwrap();
                settle(&mut driver);
                // The pressed callback hides its own input while capture is still held.
                flush(&mut driver, &mut bridge, root, &mut frame);
                assert_eq!(label(&driver, "cancelled").x, 1.);
                driver.cancel().unwrap();
            }
        }
    }

    #[test]
    fn scroll_cross_axis_source_and_asb_keep_explicit_list_sizes() {
        let source = r#"
fn main() {
    let values = [0, 1, 2, 3, 4, 5];
    let ordinary = list(|| values, key: |value| value, width: 200, align_x: 0.625,
        build: |entry| stack(width: 200, height: 60));
    let virtual_rows = list(|| values, key: |value| value, virtual: true, width: 200, align_x: 0.625,
        build: |entry| stack(width: 200, height: 60));
    let first = scroll(width: 360, height: 100) { ordinary; };
    let second = scroll(y: 150, width: 360, height: 100) { virtual_rows; };
    let horizontal_child = stack(width: 600, height: 40, align_y: 0.5);
    let third = scroll(y: 280, width: 300, height: 120, axis: "horizontal") { horizontal_child; };
    stage.replace(stack() { first; second; third; });
    let a = await stage.geometry(ordinary, relative_to: first);
    let b = await stage.geometry(virtual_rows, relative_to: second);
    let c = await stage.geometry(horizontal_child, relative_to: third);
    if a.bounds.width != 200 || b.bounds.width != 200 || a.transform.tx != 100 || b.transform.tx != 100
        || c.bounds.height != 40 || c.transform.ty != 40 {
        throw "Scroll replaced a child's explicit cross-axis dimension";
    }
    stage.replace(text("aligned"));
    narrator("aligned");
}
"#;
        let artifact = reel_cli::build(
            &reel_cli::check(source, "scroll_alignment.aside").unwrap(),
            Some("main::main"),
        )
        .unwrap();
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
            let (mut bridge, root) = bridge(&driver);
            for frame in 1..16 {
                present(&mut driver, &mut bridge, root, frame);
                settle(&mut driver);
            }
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|node| { node.kind == NodeKind::Text("aligned".into()) })
            );
            driver.cancel().unwrap();
        }
    }
}
