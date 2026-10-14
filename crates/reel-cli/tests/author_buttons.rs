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
fn click(driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, x: f64) {
    let pointer = PointerInfo {
        pointer_id: Some(PointerId::PRIMARY),
        persistent_device_id: None,
        pointer_type: PointerType::Mouse,
    };
    let state = PointerState {
        position: PhysicalPosition::new(x, 50.),
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
fn run(story: CompiledStory) {
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
            geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
            max_candidates: 4,
            max_bridge_bytes: 64 << 10,
            max_snapshot_bytes: 8 << 20,
        },
    )
    .unwrap();
    present(&mut driver, &mut bridge, root_id, 1);
    let caption = driver
        .adapter()
        .stage()
        .nodes()
        .find(|node| node.kind == NodeKind::Text("Ready".into()))
        .unwrap()
        .id;
    let moving = driver.adapter().stage().nodes().find(|node| matches!(&node.kind, NodeKind::Button { label: Some(label), .. } if label == "Move")).unwrap().id;
    click(&mut driver, &mut bridge, 50.);
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
    click(&mut driver, &mut bridge, 270.);
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
    run(reel_cli::decode_story(&artifact.bytes).unwrap());
    run(reel_engine::aside::source::compile_story(SOURCE, "main", 1, "main").unwrap());
}
