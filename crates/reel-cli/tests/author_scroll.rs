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
    let source = include_str!("../../../examples/author_scroll.aside");
    let checked = reel_cli::check(source, "author_scroll.aside").unwrap();
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
    let source = include_str!("../../../examples/author_scroll.aside")
        .replace("scroll(x: 20, y: 20, width: 300, height: 180) {", "scroll(x: 20, y: 20, width: 320, height: 180) { column() { stack(height: 160); scroll(width: 300, height: 160) {")
        .replace("    });\n    let options", "    } } });\n    let options");
    let story = compile_story(&source, "main", 1, "main").unwrap();
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
    let source = r#"fn main() {
        let value = slider(min: 0, max: 100, value: 0, width: 280, height: 40);
        let area = scroll(x: 20, y: 20, width: 300, height: 180) {
            value; stack(height: 400);
        };
        let mut errors = 0;
        try { area.axis = "diagonal"; } catch e { errors += 1; }
        try { area.follow_end = 2; } catch e { errors += 1; }
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
        game.configure(default_dialogue: false);
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
