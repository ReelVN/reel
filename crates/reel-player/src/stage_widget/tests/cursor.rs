use super::*;
use reel_ui::dpi::PhysicalPosition;
use std::{cell::RefCell, rc::Rc};

fn pointer(kind: PointerType, position: (f64, f64), down: Option<bool>) -> PointerEvent {
    let pointer = PointerInfo { pointer_id: Some(PointerId::PRIMARY), persistent_device_id: None, pointer_type: kind };
    let state = PointerState { position: PhysicalPosition::new(position.0, position.1), ..Default::default() };
    if let Some(down) = down {
        let event = PointerButtonEvent { pointer, state, button: (kind == PointerType::Mouse).then_some(PointerButton::Primary) };
        if down { PointerEvent::Down(event) } else { PointerEvent::Up(event) }
    } else {
        PointerEvent::Move(PointerUpdate { pointer, current: state, coalesced: vec![], predicted: vec![] })
    }
}

fn fixture() -> (Fixture, NodeId, NodeId, NodeId, Rc<RefCell<Vec<bool>>>) {
    let mut f = Fixture::new(None);
    let mut stage = Stage::default();
    let picture = stage.create(NodeKind::Button { label: None, listening: true }, NodeProps {
        width: Some(70.), height: Some(70.), anchor_x: 0.5, anchor_y: 0.5,
        background: Some([255,0,0,255]), modal: true, decorated: false, ..Default::default()
    }, vec![]).unwrap();
    let cursor = stage.create(NodeKind::Cursor, NodeProps {
        x: 3., y: 4., width: Some(70.), height: Some(70.), ..Default::default()
    }, vec![picture]).unwrap();
    let owner = stage.create(NodeKind::Button { label: None, listening: true }, NodeProps {
        x: 100., y: 80., width: Some(100.), height: Some(50.), scale_x: 2., scale_y: 2.,
        clip: true, opacity: 0.25, decorated: false, ..Default::default()
    }, vec![cursor]).unwrap();
    let foreground = stage.create(NodeKind::Stack, NodeProps {
        width: Some(640.), height: Some(420.), background: Some([0,0,255,255]), ..Default::default()
    }, vec![]).unwrap();
    let root = stage.create(NodeKind::Stack, NodeProps { width: Some(640.), height: Some(420.), ..Default::default() }, vec![owner,foreground]).unwrap();
    stage.replace(root).unwrap();
    f.stage=stage;f.root=root;f.red=picture;f.blue=owner;
    f.ui.set_stage(&f.stage);
    let widget=f.ui.widget().prepare();f.widget=widget.id();
    let visibility=Rc::new(RefCell::new(Vec::new()));
    let seen=visibility.clone();
    f.bridge=PresentationBridge::new(RenderRoot::new(widget, move |signal| {
        if let reel_ui::app::RenderRootSignal::SetCursorVisible(value)=signal {seen.borrow_mut().push(value);}
    }, RenderRootOptions { default_properties: Arc::new(DefaultProperties::new()), use_system_fonts:false,
        size_policy:WindowSizePolicy::User,size:PhysicalSize::new(640,420),scale_factor:1.,default_font:None }),
        SurfaceEpoch(1),BridgeLimits {geometry:GeometryLimits{max_bytes:1<<20},max_candidates:4,max_bridge_bytes:64<<10,max_snapshot_bytes:8<<20}).unwrap();
    (f,owner,cursor,picture,visibility)
}

#[test]
fn cursor_uses_window_geometry_topmost_paint_and_excludes_subtree_input() {
    let (mut f, owner, cursor, picture, visibility)=fixture();
    let initial=f.record();f.accept(&initial);drop(initial);
    f.ui.handle_pointer(&mut f.bridge,pointer(PointerType::Mouse,(110.,90.),None)).unwrap();
    let frame=f.record();f.accept(&frame);
    let ids=f.layouts();
    assert_eq!(frame.plan().layers.last().unwrap().widget_id,ids[&cursor].0);
    affine(frame.geometry().get(ids[&picture].0).unwrap().window_transform,[1.,0.,0.,1.,78.,59.]);
    assert_eq!(visibility.borrow().as_slice(), &[false]);
    assert_eq!(frame.geometry().modal_scope(),None,"cursor descendants cannot create input scopes");
    f.ui.handle_text(&mut f.bridge,TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab))).unwrap();
    assert_eq!(f.bridge.root().focused_widget(),Some(ids[&owner].0));
    f.ui.handle_text(&mut f.bridge,TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab))).unwrap();
    assert_eq!(f.bridge.root().focused_widget(),Some(ids[&owner].0), "cursor children are excluded from Tab traversal");
    let before=f.layouts();
    for i in 0..100 {
        f.ui.handle_pointer(&mut f.bridge,pointer(PointerType::Mouse,(110.+f64::from(i%20),90.),None)).unwrap();
        let next=f.record();f.accept(&next);
    }
    assert_eq!(f.layouts(),before,"mouse motion does not relayout the scene or cursor");
    f.ui.handle_pointer(&mut f.bridge,pointer(PointerType::Mouse,(110.,90.),Some(true))).unwrap();
    f.ui.handle_pointer(&mut f.bridge,pointer(PointerType::Mouse,(400.,300.),None)).unwrap();
    let captured=f.record();f.accept(&captured);
    assert_eq!(captured.plan().layers.last().unwrap().widget_id,ids[&cursor].0,"captured owner keeps its cursor outside its bounds");
    f.ui.handle_pointer(&mut f.bridge,pointer(PointerType::Mouse,(400.,300.),Some(false))).unwrap();
    assert_eq!(visibility.borrow().last(),Some(&true));
}

#[test]
fn cursor_leave_touch_override_visibility_and_retirement_follow_current_ownership() {
    let (mut f, owner, cursor, _, visibility)=fixture();
    let initial=f.record();f.accept(&initial);drop(initial);
    let mouse=PointerInfo {pointer_id:Some(PointerId::PRIMARY),persistent_device_id:None,pointer_type:PointerType::Mouse};
    f.ui.handle_pointer(&mut f.bridge,pointer(PointerType::Mouse,(110.,90.),None)).unwrap();
    let old=f.record();f.accept(&old);
    f.ui.handle_pointer(&mut f.bridge,PointerEvent::Leave(mouse)).unwrap();
    assert_eq!(visibility.borrow().last(),Some(&true));
    f.ui.handle_pointer(&mut f.bridge,pointer(PointerType::Mouse,(110.,90.),None)).unwrap();
    f.ui.handle_pointer(&mut f.bridge,pointer(PointerType::Touch,(110.,90.),Some(true))).unwrap();
    assert_eq!(visibility.borrow().last(),Some(&true));
    f.ui.handle_pointer(&mut f.bridge,pointer(PointerType::Touch,(110.,90.),Some(false))).unwrap();
    let no_mouse=f.record();f.accept(&no_mouse);
    assert_ne!(no_mouse.plan().layers.last().unwrap().widget_id,f.layouts()[&cursor].0);
    drop(no_mouse);
    f.ui.handle_pointer(&mut f.bridge,pointer(PointerType::Mouse,(110.,90.),None)).unwrap();
    let empty=f.stage.create(NodeKind::Cursor,NodeProps::default(),vec![]).unwrap();
    f.stage.add(owner,empty,1).unwrap();
    let added=f.record();f.accept(&added);drop(added);
    let hidden=f.record();f.accept(&hidden);
    assert_eq!(hidden.plan().layers.last().unwrap().widget_id,f.layouts()[&empty].0);
    drop(hidden);
    f.edit(empty,|props|props.visible=false);
    let configured=f.record();f.accept(&configured);drop(configured);
    let fallback=f.record();f.accept(&fallback);
    assert_eq!(fallback.plan().layers.last().unwrap().widget_id,f.layouts()[&cursor].0);
    drop(fallback);
    f.edit(owner,|props|props.visible=false);
    let hidden_owner=f.record();
    assert_eq!(visibility.borrow().last(),Some(&true),"live hidden state rejects the old accepted owner");
    f.accept(&hidden_owner);drop(hidden_owner);
    f.edit(owner,|props|props.visible=true);
    let shown=f.record();f.accept(&shown);drop(shown);
    f.ui.handle_pointer(&mut f.bridge,pointer(PointerType::Mouse,(110.,90.),None)).unwrap();
    f.stage.remove(cursor).unwrap();
    let retired=f.record();f.accept(&retired);drop(retired);
    assert_eq!(visibility.borrow().last(),Some(&true));
}

#[cfg(feature="window")]
#[test]
#[ignore="requires an actual graphics adapter"]
fn cursor_offscreen_pixels_keep_hotspot_above_parent_clip_and_opacity() {
    use reel_render::gpu::{DeviceGeneration,Gpu,GpuLimits,Renderer};
    let (mut f,_,_,_,_)=fixture();
    let initial=f.record();f.accept(&initial);drop(initial);
    f.ui.handle_pointer(&mut f.bridge,pointer(PointerType::Mouse,(110.,90.),None)).unwrap();
    let gpu=pollster::block_on(Gpu::request(&wgpu::Instance::default(),None,DeviceGeneration(1),GpuLimits::default())).unwrap();
    eprintln!("cursor adapter={:?}",gpu.adapter.get_info());
    let mut renderer=Renderer::new(gpu);
    let frame=f.render_frame();
    let pixels=renderer.readback(renderer.prepare(&frame).unwrap()).unwrap();
    for (x,y,color) in [(80,60,[1.,0.,0.,1.]),(145,125,[1.,0.,0.,1.]),(76,60,[0.,0.,1.,1.]),(149,129,[0.,0.,1.,1.])] {
        assert!(pixels[y*640+x].iter().zip(color).all(|(actual,expected)|(actual-expected).abs()<0.006),"pixel {x},{y}: {:?}",pixels[y*640+x]);
    }
}
