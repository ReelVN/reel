//! Source and ASB execute the same interactive scene without a window/GPU adapter.
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{SliceBudget, VmLimits, source::CompiledStory},
};
use reel_player::aside_session::{AsideSessionUi, StoryProgress, StoryState};
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{
        BridgeLimits, NoticeResult, PresentationBridge, PresentationNotice, PresentationOutcome,
    },
    core::*,
    dpi::{PhysicalPosition, PhysicalSize},
    stage::NodeKind,
};
use std::sync::Arc;

const SOURCE: &str = include_str!("../../../examples/author_buttons.aside");
fn settle(driver: &mut AsideSessionUi) {
    for _ in 0..10_000 {
        match driver
            .tick(
                SliceBudget {
                    instructions: 3,
                    ..Default::default()
                },
                16,
                || 0,
            )
            .unwrap()
        {
            StoryProgress::Idle | StoryProgress::Vm(reel_engine::aside::RunOutcome::Waiting(_))
                if !driver.has_runnable_tasks() && !driver.activities_pending() =>
            {
                return;
            }
            StoryProgress::Finished => return,
            StoryProgress::Faulted(_) => panic!("{:?}", driver.fault().unwrap()),
            StoryProgress::HostFailed(_) => panic!("{:?}", driver.host_failure()),
            _ => {}
        }
    }
    panic!("scene did not suspend");
}
fn present(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    frame: u64,
) {
    driver.sync(bridge, root).unwrap();
    let recording = bridge.record(FrameId(frame)).unwrap();
    let stamp = driver.projection_stamp();
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            recording.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(frame))
        )),
        NoticeResult::Accepted
    );
    driver.presented(stamp);
}
fn click_at(driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, x: f64, y: f64) {
    click_with_pointer(driver, bridge, x, y, PointerType::Mouse);
}
fn click_with_pointer(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    x: f64,
    y: f64,
    pointer_type: PointerType,
) {
    let pointer = PointerInfo {
        pointer_id: Some(PointerId::PRIMARY),
        persistent_device_id: None,
        pointer_type,
    };
    let state = PointerState {
        position: PhysicalPosition::new(x, y),
        ..Default::default()
    };
    for event in [
        PointerEvent::Move(PointerUpdate {
            pointer,
            current: state.clone(),
            coalesced: vec![],
            predicted: vec![],
        }),
        PointerEvent::Down(PointerButtonEvent {
            pointer,
            state: state.clone(),
            button: Some(PointerButton::Primary),
        }),
        PointerEvent::Up(PointerButtonEvent {
            pointer,
            state,
            button: Some(PointerButton::Primary),
        }),
    ] {
        driver.handle_pointer(bridge, event).unwrap();
    }
    settle(driver);
}
fn run(story: CompiledStory, pointer_type: PointerType) {
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
    click_with_pointer(&mut driver, &mut bridge, 50., 50., pointer_type);
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
    click_with_pointer(&mut driver, &mut bridge, 270., 50., pointer_type);
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
    for pointer_type in [PointerType::Mouse, PointerType::Touch] {
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

fn bridge(driver: &AsideSessionUi) -> (PresentationBridge, WidgetId) {
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
    let bridge = PresentationBridge::new(
        root,
        SurfaceEpoch(1),
        BridgeLimits {
            geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
            max_candidates: 4,
            max_bridge_bytes: 64 << 10,
            max_snapshot_bytes: 8 << 20,
        },
    )
    .unwrap();
    (bridge, root_id)
}

#[test]
fn source_and_asb_modal_menu_isolates_dialogue_and_restores_keyboard_focus() {
    const MODAL: &str = include_str!("../../../examples/author_modal.aside");
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
