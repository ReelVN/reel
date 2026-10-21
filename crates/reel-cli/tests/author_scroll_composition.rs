//! Scroll feedback and captured local dragging work through source and ASB.
#[path = "support/author.rs"]
#[allow(dead_code)]
mod support;
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
    try { page.content_height = 5; } catch e { rejected = true; }
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
