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
