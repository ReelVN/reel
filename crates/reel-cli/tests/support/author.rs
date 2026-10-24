use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_engine::aside::SliceBudget;
use reel_player::aside_session::{AsideSessionUi, StoryProgress};
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{
        BridgeLimits, NoticeResult, PresentationBridge, PresentationNotice, PresentationOutcome,
    },
    core::*,
    dpi::{PhysicalPosition, PhysicalSize},
};
use std::sync::Arc;
pub fn settle(driver: &mut AsideSessionUi) {
    for _ in 0..100_000 {
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

pub fn present(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    frame: u64,
) {
    driver.sync(bridge, root).unwrap();
    let Some((recording, stamp)) = driver.record(bridge, root, FrameId(frame)).unwrap() else {
        return;
    };
    let (drawing, _recording) = if stamp.needs_transition_drawing() {
        use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
        use reel_ui::{
            frame::{BuildLimits, FrameBuild, FrameInfo, StepBudget},
            resources::ResourceSnapshot,
        };
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 256,
                max_bytes: 128 << 20,
            },
        );
        let resources = driver
            .ui()
            .image_resources()
            .iter()
            .chain(driver.transition_resources())
            .cloned()
            .map(Ok)
            .collect::<Vec<_>>();
        let resources = ResourceSnapshot::capture(resources.into_iter(), &pool).unwrap();
        let size = driver
            .adapter()
            .game_settings()
            .canvas_size
            .unwrap_or([640, 420]);
        let mut build = FrameBuild::new(
            recording,
            resources,
            FrameInfo {
                logical_size: size.map(f64::from),
                physical_size: size,
            },
            BuildLimits { max_bytes: 8 << 20 },
            &pool,
        )
        .unwrap();
        build
            .set_transitions(stamp.frame_transitions().to_vec())
            .unwrap();
        let mut budget = StepBudget {
            max_commands: 64,
            max_units: 8192,
            max_bytes: 64 << 10,
        };
        loop {
            let progress = build.step(budget).unwrap();
            if progress.complete {
                break;
            }
            budget.max_units = budget.max_units.max(progress.minimum_units);
            budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
        }
        (Some(build.finish().unwrap()), None)
    } else {
        (None, Some(recording))
    };

    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            stamp.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(frame))
        )),
        NoticeResult::Accepted
    );
    if let Some(drawing) = &drawing {
        driver
            .presented_frame(bridge, &stamp, drawing, driver.adapter().logical_time())
            .unwrap();
    } else {
        driver.presented(bridge, &stamp).unwrap();
    }
}

pub fn click_at(driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, x: f64, y: f64) {
    click_with_pointer(driver, bridge, x, y, PointerType::Mouse);
}

pub fn click_with_pointer(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    x: f64,
    y: f64,
    pointer_type: PointerType,
) {
    for event in click_events(x, y, pointer_type) {
        driver.handle_pointer(bridge, event).unwrap();
    }
    settle(driver);
}

pub fn click_events(x: f64, y: f64, pointer_type: PointerType) -> [PointerEvent; 3] {
    let pointer = PointerInfo {
        pointer_id: Some(PointerId::PRIMARY),
        persistent_device_id: None,
        pointer_type,
    };
    let state = PointerState {
        position: PhysicalPosition::new(x, y),
        ..Default::default()
    };
    [
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
    ]
}

pub fn bridge(driver: &AsideSessionUi) -> (PresentationBridge, WidgetId) {
    bridge_with_font(driver, None)
}

pub fn bridge_with_font(
    driver: &AsideSessionUi,
    font: Option<reel_ui::peniko::Blob<u8>>,
) -> (PresentationBridge, WidgetId) {
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
            default_font: font,
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

pub fn gamepad_press(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    button: reel_ui::input::GamepadButton,
) {
    use reel_ui::input::*;
    let mut mapping = NavigationInput::default();
    for value in [1., 0.] {
        let mut events = Vec::new();
        mapping
            .update(
                &InputEvent::Value {
                    device: InputDeviceId::Gamepad(1),
                    control: Control::Button(button),
                    value,
                },
                std::time::Duration::ZERO,
                |event| events.push(event),
            )
            .unwrap();
        for event in events {
            driver
                .handle_text(bridge, TextEvent::Navigation(event))
                .unwrap();
        }
    }
    settle(driver);
}
