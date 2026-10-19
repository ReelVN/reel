use super::*;
use reel_ui::dpi::PhysicalPosition;
use reel_ui::stage::{VirtualListView, VirtualRow};

fn y(f: &Fixture, frame: &TestFrame, node: NodeId) -> f64 {
    frame
        .geometry()
        .get(f.layouts()[&node].0)
        .unwrap()
        .window_transform
        .translation()
        .y
}
fn scroll(f: &mut Fixture, offset: f64) {
    let mut kind = f.stage.node(f.root).unwrap().kind.clone();
    if let NodeKind::Scroll { offset: value, .. } = &mut kind {
        value.y = offset;
    }
    f.stage
        .update(f.root, kind, f.stage.node(f.root).unwrap().props.clone())
        .unwrap();
}
fn click(f: &mut Fixture, ypos: f64) -> Vec<NodeId> {
    let mut nodes = Vec::new();
    for down in [true, false] {
        let e = PointerButtonEvent {
            pointer: PointerInfo {
                pointer_id: Some(PointerId::PRIMARY),
                persistent_device_id: None,
                pointer_type: PointerType::Mouse,
            },
            state: PointerState {
                position: PhysicalPosition::new(20., ypos),
                ..Default::default()
            },
            button: Some(PointerButton::Primary),
        };
        for event in
            f.ui.handle_pointer(
                &mut f.bridge,
                if down {
                    PointerEvent::Down(e)
                } else {
                    PointerEvent::Up(e)
                },
            )
            .unwrap()
            .events
        {
            if let NodeEvent::Click(event) = event {
                nodes.push(event.node);
            }
        }
    }
    nodes
}

#[test]
fn virtual_list_natural_first_frame_packs_measured_neighbors_and_keeps_actual_hit_geometry() {
    let mut f = Fixture::new(None);
    let mut stage = Stage::default();
    let mut rows = Vec::new();
    for index in [0, 28, 29, 30, 31, 32, 33, 34] {
        let height = if index == 0 {
            217.
        } else if index == 30 {
            277.
        } else {
            187.
        };
        let node = stage
            .create(
                NodeKind::Button {
                    label: None,
                    listening: true,
                },
                NodeProps {
                    height: Some(height),
                    background: Some([80, 120, 160, 255]),
                    ..Default::default()
                },
                vec![],
            )
            .unwrap();
        rows.push(VirtualRow {
            node,
            index,
            offset: index as f64 * 217.,
        });
    }
    let pin = rows[0].node;
    let anchor = rows[3].node;
    let neighbor = rows[4].node;
    let list = stage
        .create(
            NodeKind::VirtualList(VirtualListView {
                revision: 1,
                total_height: 80. * 217.,
                total_rows: 80,
                rows: rows.clone().into(),
                navigation: None,
            }),
            NodeProps::default(),
            rows.iter().map(|r| r.node).collect(),
        )
        .unwrap();
    let viewport = stage
        .create(
            NodeKind::Scroll {
                axis: ScrollAxis::Vertical,
                follow_end: false,
                align: reel_ui::widgets::ScrollAlign::Start,
                offset: Vec2::new(0., 6645.),
            },
            NodeProps {
                width: Some(400.),
                height: Some(920.),
                ..Default::default()
            },
            vec![list],
        )
        .unwrap();
    stage.replace(viewport).unwrap();
    f.stage = stage;
    f.root = viewport;
    let first = f.record();
    f.accept(&first);
    assert_eq!(y(&f, &first, anchor), -135.);
    assert_eq!(
        y(&f, &first, neighbor),
        142.,
        "the old implementation first painted 82px, then moved this row 60px"
    );
    assert_eq!(
        y(&f, &first, pin),
        -6645.,
        "a distant retained row is not packed into the contiguous window"
    );
    assert_eq!(click(&mut f, 100.), [anchor]);
    assert_eq!(click(&mut f, 160.), [neighbor]);
    // Moving back and forth before the measured index arrives must retain the
    // same placement; choosing a different anchor on each compose would jump.
    let layouts = f.layouts();
    for offset in [6800., 6500., 6645.] {
        scroll(&mut f, offset);
        let frame = f.record();
        f.accept(&frame);
        assert_eq!(y(&f, &frame, neighbor), 142. + 6645. - offset);
        assert_eq!(f.layouts(), layouts);
    }
    // Simulate the engine's measured prefix and mean update with its matching
    // key-anchor scroll correction. First and accepted follow-up geometry agree.
    let mut cursor = 28. * 192.;
    for row in &mut rows[1..] {
        row.offset = cursor;
        cursor += f.stage.node(row.node).unwrap().props.height.unwrap();
    }
    let new_anchor = rows[3].offset;
    f.stage
        .update(
            list,
            NodeKind::VirtualList(VirtualListView {
                revision: 2,
                total_height: 80. * 192.,
                total_rows: 80,
                rows: rows.into(),
                navigation: None,
            }),
            NodeProps::default(),
        )
        .unwrap();
    scroll(&mut f, new_anchor + 135.);
    let measured = f.record();
    f.accept(&measured);
    assert_eq!(y(&f, &measured, anchor), -135.);
    assert_eq!(y(&f, &measured, neighbor), 142.);
    // A late size change above the anchor is absorbed in the same frame, even
    // before the engine has applied its new prefix/scroll correction.
    let preceding = f.stage.node(list).unwrap().children[2];
    f.edit(preceding, |p| p.height = Some(267.));
    f.edit(viewport, |p| p.width = Some(300.));
    let resized = f.record();
    f.accept(&resized);
    assert_eq!(y(&f, &resized, anchor), -135.);
    assert_eq!(y(&f, &resized, neighbor), 142.);
}

use crate::aside_session::{AsideSessionUi, StoryProgress};
use reel_engine::aside::{SliceBudget, VmLimits};
fn settle(d: &mut AsideSessionUi) {
    for _ in 0..100_000 {
        match d.tick(SliceBudget::default(), 32, || 0).unwrap() {
            StoryProgress::Idle | StoryProgress::Vm(reel_engine::aside::RunOutcome::Waiting(_))
                if !d.has_runnable_tasks() =>
            {
                return;
            }
            StoryProgress::Faulted(_) => panic!("{:?}", d.fault().unwrap()),
            StoryProgress::HostFailed(_) => panic!("{:?}", d.host_failure()),
            _ => {}
        }
    }
    panic!("list failed to settle");
}
fn actual_rows(
    d: &AsideSessionUi,
    b: &PresentationBridge,
    root: WidgetId,
    source: &RecordedSource,
) -> BTreeMap<usize, (f64, f64)> {
    fn walk(
        w: WidgetRef<'_, dyn Widget>,
        s: &RecordedSource,
        result: &mut BTreeMap<NodeId, (f64, f64)>,
    ) {
        if let Some(scene) = w.downcast::<SceneWidget>() {
            let g = s.geometry().get(w.id()).unwrap();
            result.insert(
                scene.node.id,
                (g.window_transform.translation().y, g.size.height),
            );
        }
        for c in w.children() {
            walk(c, s, result);
        }
    }
    let mut geometry = BTreeMap::new();
    walk(b.root().get_widget(root).unwrap(), source, &mut geometry);
    let view = d
        .adapter()
        .stage()
        .nodes()
        .find_map(|n| match &n.kind {
            NodeKind::VirtualList(v) => Some(v),
            _ => None,
        })
        .unwrap();
    view.rows
        .iter()
        .filter_map(|r| geometry.get(&r.node).map(|g| (r.index, *g)))
        .collect()
}
#[test]
fn virtual_list_natural_real_host_drag_frames_keep_displayed_rows_stable() {
    let code = r#"
        fn main(){
            let items=[];for id in 0..80 {items.append({id:id,height:if id==0 {217} else if id%10==0 {277} else {187}});}
            let rows=list(||items,key:|item|item.id,virtual:true,build:|row|column(height:row.value.height));
            let viewport=scroll(width:1313,height:920){rows};
            let mut step=0;let fractions=[0.8,0.1,0.55,0.93,0.0,1.0];
            stage.replace(stack(width:1600,height:1000){viewport;
                button("jump",x:1400,y:20,width:100,height:50,on_click:||{
                    viewport.scroll_y=fractions[step]*(viewport.content_height-viewport.viewport_height);step+=1;
                });
            });narrator("ready");
        }
    "#;
    for encoded in [false, true] {
        let mut compiled =
            reel_engine::aside::source::compile_story(code, "list", 1, "main").unwrap();
        if encoded {
            use aside_vm::bytecode as bc;
            let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
            compiled.program = bc::verify(
                bc::decode(&bytes, bc::Limits::default()).unwrap(),
                bc::Limits::default(),
            )
            .unwrap();
        }
        let mut d = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            compiled,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut d);
        let widget = d.widget().prepare();
        let root = widget.id();
        let render = RenderRoot::new(
            widget,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(1600, 1000),
                scale_factor: 1.,
                default_font: None,
            },
        );
        let mut bridge = PresentationBridge::new(
            render,
            SurfaceEpoch(1),
            BridgeLimits {
                geometry: GeometryLimits { max_bytes: 1 << 20 },
                max_candidates: 4,
                max_bridge_bytes: 64 << 10,
                max_snapshot_bytes: 8 << 20,
            },
        )
        .unwrap();
        let mut frame = 0;
        let mut render = |d: &mut AsideSessionUi, bridge: &mut PresentationBridge| {
            d.sync(bridge, root).unwrap();
            let accepted = bridge.accepted_key().map(|key| key.frame_id());
            let pending = bridge.pending_count();
            frame += 1;
            let Some((source, receipt)) = d.record(bridge, root, FrameId(frame)).unwrap() else {
                assert!(d.has_runnable_tasks());
                assert_eq!(bridge.pending_count(), pending);
                assert_eq!(bridge.accepted_key().map(|key| key.frame_id()), accepted);
                settle(d);
                return None;
            };
            let rows = actual_rows(d, bridge, root, &source);
            assert_eq!(
                bridge.resolve(PresentationNotice::new(
                    source.key(),
                    SurfaceEpoch(1),
                    PresentationOutcome::Accepted(FrameId(frame))
                )),
                NoticeResult::Accepted
            );
            d.presented(bridge, &receipt).unwrap();
            settle(d);
            Some(rows)
        };
        for _ in 0..5 {
            render(&mut d, &mut bridge);
        }
        for jump in 0..6 {
            for down in [true, false] {
                let e = PointerButtonEvent {
                    pointer: PointerInfo {
                        pointer_id: Some(PointerId::PRIMARY),
                        persistent_device_id: None,
                        pointer_type: PointerType::Mouse,
                    },
                    state: PointerState {
                        position: PhysicalPosition::new(1440., 40.),
                        ..Default::default()
                    },
                    button: Some(PointerButton::Primary),
                };
                d.handle_pointer(
                    &mut bridge,
                    if down {
                        PointerEvent::Down(e)
                    } else {
                        PointerEvent::Up(e)
                    },
                )
                .unwrap();
            }
            settle(&mut d);
            let mut before: BTreeMap<usize, (f64, f64)> = BTreeMap::new();
            let mut accepted = 0;
            for phase in 0..5 {
                let Some(rows) = render(&mut d, &mut bridge) else {
                    continue;
                };
                accepted += 1;
                let visible = rows
                    .iter()
                    .filter(|(_, (y, h))| *y < 920. && y + h > 0.)
                    .count();
                assert!(
                    visible > 0,
                    "jump={jump} phase={phase} accepted an empty window"
                );
                for (index, (old_y, old_h)) in &before {
                    if *old_y < 920.
                        && old_y + old_h > 0.
                        && let Some((new_y, _)) = rows.get(index)
                    {
                        assert!(
                            (old_y - new_y).abs() < 1e-6,
                            "jump={jump} phase={phase} row={index}: {old_y} -> {new_y}"
                        );
                    }
                }
                before = rows;
            }
            assert!(accepted >= 2, "far window did not become ready");
        }
    }
}
