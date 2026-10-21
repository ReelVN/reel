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
    try { enabled.value = 1; } catch e { failures += 1; }
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
        reel_cli::decode_story(&artifact.bytes).unwrap(),
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
#[test]
fn settings_example_compiles_from_source_and_asb_and_initializes_live_controls() {
    for story in stories(include_str!(
        "../../../examples/author_settings/src/main.aside"
    )) {
        let mut driver = driver(story);
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| matches!(n.kind, NodeKind::Slider { value: 30., .. }))
        );
        driver.cancel().unwrap();
    }
}
