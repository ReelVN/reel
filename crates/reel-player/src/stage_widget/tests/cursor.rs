use super::*;
use reel_ui::dpi::PhysicalPosition;
use std::{cell::RefCell, rc::Rc};

fn pointer(kind: PointerType, position: (f64, f64), down: Option<bool>) -> PointerEvent {
    let pointer = PointerInfo {
        pointer_id: Some(PointerId::PRIMARY),
        persistent_device_id: None,
        pointer_type: kind,
    };
    let state = PointerState {
        position: PhysicalPosition::new(position.0, position.1),
        ..Default::default()
    };
    if let Some(down) = down {
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
    } else {
        PointerEvent::Move(PointerUpdate {
            pointer,
            current: state,
            coalesced: vec![],
            predicted: vec![],
        })
    }
}

fn fixture() -> (Fixture, NodeId, NodeId, NodeId, Rc<RefCell<Vec<bool>>>) {
    let mut f = Fixture::new(None);
    let mut stage = Stage::default();
    let picture = stage
        .create(
            NodeKind::Button {
                label: None,
                listening: true,
            },
            NodeProps {
                width: Some(70.),
                height: Some(70.),
                anchor_x: 0.5,
                anchor_y: 0.5,
                background: Some([255, 0, 0, 255]),
                modal: true,
                decorated: false,
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let cursor = stage
        .create(
            NodeKind::Cursor,
            NodeProps {
                x: 3.,
                y: 4.,
                width: Some(70.),
                height: Some(70.),
                ..Default::default()
            },
            vec![picture],
        )
        .unwrap();
    let owner = stage
        .create(
            NodeKind::Button {
                label: None,
                listening: true,
            },
            NodeProps {
                x: 100.,
                y: 80.,
                width: Some(100.),
                height: Some(50.),
                scale_x: 2.,
                scale_y: 2.,
                clip: true,
                opacity: 0.25,
                decorated: false,
                ..Default::default()
            },
            vec![cursor],
        )
        .unwrap();
    let foreground = stage
        .create(
            NodeKind::Stack,
            NodeProps {
                width: Some(640.),
                height: Some(420.),
                background: Some([0, 0, 255, 255]),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let root = stage
        .create(
            NodeKind::Stack,
            NodeProps {
                width: Some(640.),
                height: Some(420.),
                ..Default::default()
            },
            vec![owner, foreground],
        )
        .unwrap();
    stage.replace(root).unwrap();
    f.stage = stage;
    f.root = root;
    f.red = picture;
    f.blue = owner;
    f.ui.set_stage(&f.stage);
    let widget = f.ui.widget().prepare();
    f.widget = widget.id();
    let visibility = Rc::new(RefCell::new(Vec::new()));
    let seen = visibility.clone();
    f.bridge = PresentationBridge::new(
        RenderRoot::new(
            widget,
            move |signal| {
                if let reel_ui::app::RenderRootSignal::SetCursorVisible(value) = signal {
                    seen.borrow_mut().push(value);
                }
            },
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(640, 420),
                scale_factor: 1.,
                default_font: None,
            },
        ),
        SurfaceEpoch(1),
        BridgeLimits {
            geometry: GeometryLimits { max_bytes: 1 << 20 },
            max_candidates: 4,
            max_bridge_bytes: 64 << 10,
            max_snapshot_bytes: 8 << 20,
        },
    )
    .unwrap();
    (f, owner, cursor, picture, visibility)
}

#[test]
fn cursor_uses_window_geometry_topmost_paint_and_excludes_subtree_input() {
    let (mut f, owner, cursor, picture, visibility) = fixture();
    let initial = f.record();
    f.accept(&initial);
    drop(initial);
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Mouse, (110., 90.), None),
    )
    .unwrap();
    let frame = f.record();
    f.accept(&frame);
    let ids = f.layouts();
    assert_eq!(
        frame.plan().layers.last().unwrap().widget_id,
        ids[&cursor].0
    );
    affine(
        frame
            .geometry()
            .get(ids[&picture].0)
            .unwrap()
            .window_transform,
        [1., 0., 0., 1., 78., 59.],
    );
    assert_eq!(visibility.borrow().as_slice(), &[false]);
    assert_eq!(
        frame.geometry().modal_scope(),
        None,
        "cursor descendants cannot create input scopes"
    );
    f.ui.handle_text(
        &mut f.bridge,
        TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab)),
    )
    .unwrap();
    assert_eq!(f.bridge.root().focused_widget(), Some(ids[&owner].0));
    f.ui.handle_text(
        &mut f.bridge,
        TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab)),
    )
    .unwrap();
    assert_eq!(
        f.bridge.root().focused_widget(),
        Some(ids[&owner].0),
        "cursor children are excluded from Tab traversal"
    );
    let before = f.layouts();
    for i in 0..100 {
        f.ui.handle_pointer(
            &mut f.bridge,
            pointer(PointerType::Mouse, (110. + f64::from(i % 20), 90.), None),
        )
        .unwrap();
        let next = f.record();
        f.accept(&next);
    }
    assert_eq!(
        f.layouts(),
        before,
        "mouse motion does not relayout the scene or cursor"
    );
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Mouse, (110., 90.), Some(true)),
    )
    .unwrap();
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Mouse, (400., 300.), None),
    )
    .unwrap();
    let captured = f.record();
    f.accept(&captured);
    assert_eq!(
        captured.plan().layers.last().unwrap().widget_id,
        ids[&cursor].0,
        "captured owner keeps its cursor outside its bounds"
    );
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Mouse, (400., 300.), Some(false)),
    )
    .unwrap();
    assert_eq!(visibility.borrow().last(), Some(&true));
}

#[test]
fn cursor_leave_touch_override_visibility_and_retirement_follow_current_ownership() {
    let (mut f, owner, cursor, _, visibility) = fixture();
    let initial = f.record();
    f.accept(&initial);
    drop(initial);
    let mouse = PointerInfo {
        pointer_id: Some(PointerId::PRIMARY),
        persistent_device_id: None,
        pointer_type: PointerType::Mouse,
    };
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Mouse, (110., 90.), None),
    )
    .unwrap();
    let old = f.record();
    f.accept(&old);
    f.ui.handle_pointer(&mut f.bridge, PointerEvent::Leave(mouse))
        .unwrap();
    assert_eq!(visibility.borrow().last(), Some(&true));
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Mouse, (110., 90.), None),
    )
    .unwrap();
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Touch, (110., 90.), Some(true)),
    )
    .unwrap();
    assert_eq!(visibility.borrow().last(), Some(&true));
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Touch, (110., 90.), Some(false)),
    )
    .unwrap();
    let no_mouse = f.record();
    f.accept(&no_mouse);
    assert_ne!(
        no_mouse.plan().layers.last().unwrap().widget_id,
        f.layouts()[&cursor].0
    );
    drop(no_mouse);
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Mouse, (110., 90.), None),
    )
    .unwrap();
    let empty = f
        .stage
        .create(NodeKind::Cursor, NodeProps::default(), vec![])
        .unwrap();
    f.stage.add(owner, empty, 1).unwrap();
    let added = f.record();
    f.accept(&added);
    drop(added);
    let hidden = f.record();
    f.accept(&hidden);
    assert_eq!(
        hidden.plan().layers.last().unwrap().widget_id,
        f.layouts()[&empty].0
    );
    drop(hidden);
    f.edit(empty, |props| props.visible = false);
    let configured = f.record();
    f.accept(&configured);
    drop(configured);
    let fallback = f.record();
    f.accept(&fallback);
    assert_eq!(
        fallback.plan().layers.last().unwrap().widget_id,
        f.layouts()[&cursor].0
    );
    drop(fallback);
    f.edit(owner, |props| props.visible = false);
    let hidden_owner = f.record();
    assert_eq!(
        visibility.borrow().last(),
        Some(&true),
        "live hidden state rejects the old accepted owner"
    );
    f.accept(&hidden_owner);
    drop(hidden_owner);
    f.edit(owner, |props| props.visible = true);
    let shown = f.record();
    f.accept(&shown);
    drop(shown);
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Mouse, (110., 90.), None),
    )
    .unwrap();
    f.stage.remove(cursor).unwrap();
    let retired = f.record();
    f.accept(&retired);
    drop(retired);
    assert_eq!(visibility.borrow().last(), Some(&true));
}

#[cfg(feature = "window")]
#[test]
#[ignore = "requires an actual graphics adapter"]
fn cursor_offscreen_pixels_keep_hotspot_above_parent_clip_and_opacity() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    let (mut f, _, _, _, _) = fixture();
    let initial = f.record();
    f.accept(&initial);
    drop(initial);
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Mouse, (110., 90.), None),
    )
    .unwrap();
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits::default(),
    ))
    .unwrap();
    eprintln!("cursor adapter={:?}", gpu.adapter.get_info());
    let mut renderer = Renderer::new(gpu);
    let frame = f.render_frame();
    let pixels = renderer
        .readback(renderer.prepare(&frame).unwrap())
        .unwrap();
    for (x, y, color) in [
        (80, 60, [1., 0., 0., 1.]),
        (145, 125, [1., 0., 0., 1.]),
        (76, 60, [0., 0., 1., 1.]),
        (149, 129, [0., 0., 1., 1.]),
    ] {
        assert!(
            pixels[y * 640 + x]
                .iter()
                .zip(color)
                .all(|(actual, expected)| (actual - expected).abs() < 0.006),
            "pixel {x},{y}: {:?}",
            pixels[y * 640 + x]
        );
    }
}

#[test]
fn cursor_modal_focus_loss_and_cleared_presentation_withdraw_without_stale_revival() {
    let (mut f, _, cursor, _, visibility) = fixture();
    let initial = f.record();
    f.accept(&initial);
    drop(initial);
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Mouse, (110., 90.), None),
    )
    .unwrap();
    let old = f.record();
    f.accept(&old);
    f.ui.handle_text(&mut f.bridge, TextEvent::WindowFocusChange(false))
        .unwrap();
    assert_eq!(visibility.borrow().last(), Some(&true));
    f.ui.handle_text(&mut f.bridge, TextEvent::WindowFocusChange(true))
        .unwrap();
    assert_eq!(visibility.borrow().last(), Some(&true));
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Mouse, (110., 90.), None),
    )
    .unwrap();
    let modal = f
        .stage
        .create(
            NodeKind::Stack,
            NodeProps {
                width: Some(640.),
                height: Some(420.),
                modal: true,
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    f.stage.add(f.root, modal, 2).unwrap();
    let next = f.record();
    f.accept(&next);
    drop(next);
    assert_eq!(visibility.borrow().last(), Some(&true));
    f.stage.remove(modal).unwrap();
    let next = f.record();
    f.accept(&next);
    drop(next);
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Mouse, (110., 90.), None),
    )
    .unwrap();
    assert_eq!(visibility.borrow().last(), Some(&false));
    f.bridge.clear();
    assert_eq!(visibility.borrow().last(), Some(&true));
    let next = f.record();
    f.accept(&next);
    assert_ne!(
        next.plan().layers.last().unwrap().widget_id,
        f.layouts()[&cursor].0
    );
    assert_eq!(
        visibility.borrow().last(),
        Some(&true),
        "accepting a frame is not new mouse input"
    );
    assert_ne!(
        f.bridge.resolve(PresentationNotice::new(
            old.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(old.key().frame_id())
        )),
        NoticeResult::Accepted
    );
}

#[test]
fn cursor_snapshot_restores_subtree_and_owned_activity() {
    use crate::aside_session::{AsideSessionUi, StoryProgress};
    use aside_vm::snapshot::SnapshotBudget;
    use reel_engine::SessionIdentity;
    use reel_engine::aside::{SessionVm, SliceBudget, VmLimits};
    fn drain(d: &mut AsideSessionUi) {
        for _ in 0..10000 {
            match d.tick(SliceBudget::default(), 32, || 0).unwrap() {
                StoryProgress::Faulted(_) => panic!("{:?}", d.fault()),
                StoryProgress::HostFailed(_) => panic!("{:?}", d.host_failure()),
                _ => {}
            }
            if !d.has_runnable_tasks() && !d.activities_pending() {
                return;
            }
        }
        panic!("cursor did not settle");
    }
    let mut live=AsideSessionUi::from_source(Session::new(SessionId(1),Epoch(1)).unwrap(),
        r##"fn main() {
            let pointer=cursor(width:20,height:20) {stack(width:20,height:20,background:"#ff0000");};
            stage.replace(stack(width:640,height:420,children:[pointer]));
            run(repeat(sequence(tween(pointer,opacity:0.2,duration:1),tween(pointer,opacity:1,duration:1))),owner:pointer);
        }"##,"main",1,"main","en",VmLimits::default()).unwrap();
    drain(&mut live);
    while live.advance_time(0.5, 16).unwrap().pending {}
    drain(&mut live);
    let opacity = live
        .adapter()
        .stage()
        .nodes()
        .find(|n| n.kind == NodeKind::Cursor)
        .unwrap()
        .props
        .opacity;
    assert!((opacity - 0.6).abs() < 0.001);
    let mut capture = live.snapshot_begin().unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let bytes = capture.finish().unwrap().encode().unwrap();
    let mut image = reel_engine::aside::snapshot::SessionSnapshot::decode(&bytes).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        live.adapter().vm().program().clone(),
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(2),
            epoch: Epoch(2),
        },
        Default::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (session, vm) = restore.finish().unwrap();
    let mut restored = AsideSessionUi::restored(
        session,
        vm,
        "en".into(),
        live.ui().assets(),
        live.initializer(),
    )
    .unwrap();
    while !restored.prepare_restored(SliceBudget::default()).unwrap() {}
    drain(&mut restored);
    assert!(restored.activities_need_time());
    let pointer = restored
        .adapter()
        .stage()
        .nodes()
        .find(|n| n.kind == NodeKind::Cursor)
        .unwrap();
    assert_eq!(pointer.children.len(), 1);
    assert_eq!(pointer.props.opacity, opacity);
    while restored.advance_time(0.75, 16).unwrap().pending {}
    drain(&mut restored);
    let value = restored
        .adapter()
        .stage()
        .nodes()
        .find(|n| n.kind == NodeKind::Cursor)
        .unwrap()
        .props
        .opacity;
    assert!(
        value < opacity,
        "restored activity continues from captured progress"
    );
}

#[test]
fn cursor_does_not_contribute_to_natural_flow_size() {
    let mut f = Fixture::new(None);
    let child = f
        .stage
        .create(
            NodeKind::Stack,
            NodeProps {
                width: Some(40.),
                height: Some(20.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let cursor = f
        .stage
        .create(
            NodeKind::Cursor,
            NodeProps {
                width: Some(3000.),
                height: Some(3000.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let column = f
        .stage
        .create(NodeKind::Column, NodeProps::default(), vec![cursor, child])
        .unwrap();
    f.stage.replace(column).unwrap();
    let frame = f.record();
    f.accept(&frame);
    assert_eq!(
        frame.geometry().get(f.layouts()[&column].0).unwrap().size,
        Size::new(40., 20.)
    );
}

#[test]
fn cursor_deepest_owner_wins_and_disabled_owner_yields_to_ancestor() {
    let (mut f, owner, cursor, _, visibility) = fixture();
    let ancestor = f
        .stage
        .create(NodeKind::Cursor, NodeProps::default(), vec![])
        .unwrap();
    f.stage.add(f.root, ancestor, 2).unwrap();
    let first = f.record();
    f.accept(&first);
    drop(first);
    f.ui.handle_pointer(
        &mut f.bridge,
        pointer(PointerType::Mouse, (110., 90.), None),
    )
    .unwrap();
    let active = f.record();
    f.accept(&active);
    assert_eq!(
        active.plan().layers.last().unwrap().widget_id,
        f.layouts()[&cursor].0
    );
    drop(active);
    f.edit(owner, |props| props.enabled = false);
    let disabled = f.record();
    f.accept(&disabled);
    drop(disabled);
    let fallback = f.record();
    f.accept(&fallback);
    assert_eq!(
        fallback.plan().layers.last().unwrap().widget_id,
        f.layouts()[&ancestor].0
    );
    drop(fallback);
    assert_eq!(
        visibility.borrow().last(),
        Some(&false),
        "empty ancestor intentionally keeps native cursor hidden"
    );
    f.stage.remove(ancestor).unwrap();
    let removed = f.record();
    f.accept(&removed);
    assert_eq!(visibility.borrow().last(), Some(&true));
}
