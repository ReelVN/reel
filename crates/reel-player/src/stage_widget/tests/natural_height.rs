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
                    focus_on_pointer: true,
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

#[test]
fn virtual_list_feedback_waits_for_ordinary_properties_then_records_same_committed_rows() {
    for encoded in [false, true] {
        let code = r#"fn main(){
            let values=[];for i in 0..80 {values.append(i);}
            let viewport=scroll(width:320,height:120);
            let rows=list(||values,key:|x|x,virtual:true,build:|row|column(height:40){stack(height:20);});
            viewport.add(rows);
            stage.replace(stack(){viewport;stack(x:400,y:derive(||viewport.scroll_y),width:20,height:20);});
            narrator("ready");
        }"#;
        let mut story =
            reel_engine::aside::source::compile_story(code, "feedback", 1, "main").unwrap();
        if encoded {
            use aside_vm::bytecode as bc;
            story.program = bc::verify(
                bc::decode(
                    &bc::encode(story.program.package(), bc::Limits::default()).unwrap(),
                    bc::Limits::default(),
                )
                .unwrap(),
                bc::Limits::default(),
            )
            .unwrap();
        }
        let mut d = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
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
                size: PhysicalSize::new(640, 420),
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
        for _ in 0..10 {
            d.sync(&mut bridge, root).unwrap();
            frame += 1;
            if let Some((source, receipt)) = d.record(&mut bridge, root, FrameId(frame)).unwrap() {
                bridge.resolve(PresentationNotice::new(
                    source.key(),
                    SurfaceEpoch(1),
                    PresentationOutcome::Accepted(FrameId(frame)),
                ));
                d.presented(&bridge, &receipt).unwrap();
            }
            settle(&mut d);
        }
        let list = d
            .adapter()
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::VirtualList(_)))
            .unwrap()
            .id;
        let committed = d.adapter().stage().node(list).unwrap().children.clone();
        d.handle_pointer(
            &mut bridge,
            PointerEvent::Scroll(PointerScrollEvent {
                pointer: PointerInfo {
                    pointer_id: Some(PointerId::PRIMARY),
                    persistent_device_id: None,
                    pointer_type: PointerType::Mouse,
                },
                state: PointerState {
                    position: PhysicalPosition::new(20., 20.),
                    ..Default::default()
                },
                delta: ScrollDelta::PixelDelta(PhysicalPosition::new(0., 100.)),
            }),
        )
        .unwrap();
        d.sync_input(&mut bridge, root).unwrap();
        assert!(!d.adapter().stage_ready());
        assert!(
            d.record(&mut bridge, root, FrameId(100)).unwrap().is_none(),
            "queued scrollbar derive cannot paint stale props"
        );
        let mut feedback = false;
        for _ in 0..100_000 {
            d.tick(
                SliceBudget {
                    instructions: 10,
                    ..Default::default()
                },
                32,
                || 0,
            )
            .unwrap();
            if d.adapter().virtual_feedback_boundary() {
                d.sync(&mut bridge, root).unwrap();
                if let Some((source, receipt)) = d.record(&mut bridge, root, FrameId(101)).unwrap()
                {
                    assert!(!receipt.current);
                    assert!(receipt.geometry.is_none());
                    assert_eq!(d.adapter().stage().node(list).unwrap().children, committed);
                    bridge.resolve(PresentationNotice::new(
                        source.key(),
                        SurfaceEpoch(1),
                        PresentationOutcome::Accepted(FrameId(101)),
                    ));
                    d.presented(&bridge, &receipt).unwrap();
                    assert!(!d.ui().current_presentation_ready());
                    feedback = true;
                    break;
                }
            }
        }
        assert!(
            feedback,
            "whole-row boundary must resume drawing without another input"
        );
        settle(&mut d);
    }
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
    natural_scroll_motion(false, false);
}

#[test]
fn virtual_list_natural_wheel_frames_keep_displayed_rows_stable() {
    natural_scroll_motion(true, false);
}

#[test]
// The external fixture idles with its first virtual list in a 1313x920 scroll
// at the origin under a 0.6666667 scale, matching the built-in motion fixture.
#[ignore = "requires REEL_SCROLL_ASB, REEL_SCROLL_ASSETS and REEL_SESSION_FONT"]
fn virtual_list_natural_external_wheel_frames() {
    natural_scroll_motion(true, true);
}

#[test]
#[ignore = "requires REEL_SCROLL_ASB, REEL_SCROLL_ASSETS and REEL_SESSION_FONT"]
fn virtual_list_natural_external_drag_frames() {
    natural_scroll_motion(false, true);
}

#[derive(Default)]
struct MotionCosts {
    count: [usize; 8],
    total: [std::time::Duration; 8],
    max: [std::time::Duration; 8],
    at: [String; 8],
    slow: Vec<(usize, std::time::Duration, String)>,
}
impl MotionCosts {
    fn add(&mut self, phase: usize, elapsed: std::time::Duration, at: &str) {
        if at != "warm" && elapsed.as_millis() >= 2 {
            self.slow.push((phase, elapsed, at.to_owned()));
        }
        self.count[phase] += 1;
        self.total[phase] += elapsed;
        if elapsed > self.max[phase] {
            self.max[phase] = elapsed;
            self.at[phase] = at.to_owned();
        }
    }
    fn report(&self) {
        for (phase, time, at) in &self.slow {
            eprintln!("slow phase={phase} elapsed={time:?} at={at}");
        }
        for (phase, name) in [
            "input",
            "settle",
            "sync",
            "record",
            "geometry_assert",
            "resolve",
            "presented",
            "sleep",
        ]
        .into_iter()
        .enumerate()
        {
            eprintln!(
                "phase={name} count={} total={:?} max={:?} at={}",
                self.count[phase], self.total[phase], self.max[phase], self.at[phase]
            );
        }
    }
}
fn measured<R>(
    costs: &std::cell::RefCell<MotionCosts>,
    phase: usize,
    at: &str,
    body: impl FnOnce() -> R,
) -> R {
    let start = std::time::Instant::now();
    let value = body();
    costs.borrow_mut().add(phase, start.elapsed(), at);
    value
}

fn natural_scroll_motion(wheel: bool, external: bool) {
    let code = r#"
        fn main(){
            let items=[];for id in 0..80 {items.append({id:id,height:if id==0 {217} else if id%10==0 {277} else {187}});}
            let rows=list(||items,key:|item|item.id,virtual:true,build:|row|column(height:row.value.height));
            let viewport=scroll(width:1313,height:920){rows};
            let mut step=0;let fractions=[0.8,0.1,0.55,0.93,0.0,1.0];
            stage.replace(stack(width:1600,height:1000,scale_x:0.6666667,scale_y:0.6666667){viewport;
                button("jump",x:1400,y:20,width:100,height:50,on_click:||{
                    viewport.scroll_y=fractions[step]*(viewport.content_height-viewport.viewport_height);step+=1;
                });
            });narrator("ready");
        }
    "#;
    for encoded in [false, true] {
        if external && encoded {
            continue;
        }
        let mut compiled = if external {
            let bytes = std::fs::read(std::env::var_os("REEL_SCROLL_ASB").unwrap()).unwrap();
            let (program, metadata) = reel_package::decode(&bytes, reel_package::Limits::default())
                .unwrap()
                .into_parts();
            reel_engine::aside::source::CompiledStory {
                program,
                initializer: aside_vm::bytecode::FunctionId(metadata.initializer.function_id),
                entry: aside_vm::bytecode::FunctionId(metadata.game_entry.unwrap().function_id),
                catalogs: Default::default(),
            }
        } else {
            reel_engine::aside::source::compile_story(code, "list", 1, "main").unwrap()
        };
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
        if external {
            d = d.with_asset_options(crate::assets::AssetOptions {
                root: std::env::var_os("REEL_SCROLL_ASSETS").unwrap().into(),
                ..Default::default()
            });
        }
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
                default_font: if external {
                    Some(
                        std::fs::read(std::env::var_os("REEL_SESSION_FONT").unwrap())
                            .unwrap()
                            .into(),
                    )
                } else {
                    None
                },
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
        let costs = std::cell::RefCell::new(MotionCosts::default());
        let mut frame = 0;
        let mut resource_wait: Option<(std::time::Instant, String, usize, std::time::Duration)> =
            None;
        let cadence = std::time::Duration::from_secs_f64(
            std::env::var("REEL_SCROLL_FRAME_MS")
                .ok()
                .map(|s| s.parse::<f64>().unwrap())
                .unwrap_or(1000. / 60.)
                / 1000.,
        );
        let mut last_accepted = std::time::Instant::now();
        let mut next_frame = last_accepted;
        let mut blocked = false;
        let mut render = |d: &mut AsideSessionUi, bridge: &mut PresentationBridge, step: &str| {
            loop {
                measured(&costs, 2, step, || d.sync(bridge, root).unwrap());
                if external && d.ui().images_pending() && resource_wait.is_none() {
                    resource_wait = Some((
                        std::time::Instant::now(),
                        step.to_owned(),
                        0,
                        std::time::Duration::ZERO,
                    ));
                }
                if !external || std::time::Instant::now() >= next_frame {
                    break;
                }
                // Resource completions and VM feedback keep progressing between
                // display deadlines, as they do on the window host's wake path.
                measured(&costs, 1, step, || settle(d));
                std::thread::sleep(
                    std::time::Duration::from_millis(1)
                        .min(next_frame.saturating_duration_since(std::time::Instant::now())),
                );
            }
            let accepted = bridge.accepted_key().map(|key| key.frame_id());
            let pending = bridge.pending_count();
            frame += 1;
            let Some((source, receipt)) = measured(&costs, 3, step, || {
                d.record(bridge, root, FrameId(frame)).unwrap()
            }) else {
                assert!(d.has_runnable_tasks() || d.ui().images_pending());
                assert_eq!(bridge.pending_count(), pending);
                assert_eq!(bridge.accepted_key().map(|key| key.frame_id()), accepted);
                if external && !blocked {
                    eprintln!(
                        "blocked step={step} stage_ready={} images_pending={} images_block={}",
                        d.adapter().stage_ready(),
                        d.ui().images_pending(),
                        d.ui().images_block_presentation()
                    );
                    blocked = true;
                }
                measured(&costs, 1, step, || settle(d));
                if external {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                return None;
            };
            let rows = measured(&costs, 4, step, || actual_rows(d, bridge, root, &source));
            assert_eq!(
                bridge.resolve(PresentationNotice::new(
                    source.key(),
                    SurfaceEpoch(1),
                    PresentationOutcome::Accepted(FrameId(frame))
                )),
                NoticeResult::Accepted
            );
            let now = std::time::Instant::now();
            if let Some((start, label, frames, longest)) = &mut resource_wait {
                *longest = (*longest).max(now - last_accepted);
                if !receipt.current {
                    *frames += 1;
                } else {
                    eprintln!(
                        "resources step={label} pending={:?} retained_accepted={} longest_accept_gap={:?}",
                        start.elapsed(),
                        frames,
                        longest
                    );
                    resource_wait = None;
                }
            }
            last_accepted = now;
            next_frame = now + cadence;
            blocked = false;
            measured(&costs, 6, step, || d.presented(bridge, &receipt).unwrap());
            measured(&costs, 1, step, || settle(d));
            Some(rows)
        };
        let mut previous_rows = BTreeMap::new();
        let mut warm_frames = 0;
        for _ in 0..2000 {
            if let Some(rows) = render(&mut d, &mut bridge, "warm") {
                previous_rows = rows;
                warm_frames = if d.ui().images_pending() {
                    0
                } else {
                    warm_frames + 1
                };
                if warm_frames >= 5 {
                    break;
                }
            }
        }
        assert!(!previous_rows.is_empty());
        if external && !wheel {
            d.handle_pointer(
                &mut bridge,
                super::cursor::pointer(PointerType::Mouse, (876., 20.), Some(true)),
            )
            .unwrap();
            settle(&mut d);
        }
        let motion_started = std::time::Instant::now();
        for jump in 0..if wheel || external { 180 } else { 6 } {
            let input_started = std::time::Instant::now();
            if wheel {
                d.handle_pointer(
                    &mut bridge,
                    PointerEvent::Scroll(PointerScrollEvent {
                        pointer: PointerInfo {
                            pointer_id: Some(PointerId::PRIMARY),
                            persistent_device_id: None,
                            pointer_type: PointerType::Mouse,
                        },
                        state: PointerState {
                            position: PhysicalPosition::new(100., 100.),
                            ..Default::default()
                        },
                        delta: ScrollDelta::PixelDelta(PhysicalPosition::new(
                            0.,
                            if jump < 90 { 40.25 } else { -37.25 },
                        )),
                    }),
                )
                .unwrap();
            } else if external {
                let y = if jump < 90 {
                    20. + (jump + 1) as f64 * 3.
                } else {
                    290. - (jump - 89) as f64 * 3.
                };
                d.handle_pointer(
                    &mut bridge,
                    super::cursor::pointer(PointerType::Mouse, (876., y), None),
                )
                .unwrap();
            } else {
                for down in [true, false] {
                    let e = PointerButtonEvent {
                        pointer: PointerInfo {
                            pointer_id: Some(PointerId::PRIMARY),
                            persistent_device_id: None,
                            pointer_type: PointerType::Mouse,
                        },
                        state: PointerState {
                            position: PhysicalPosition::new(960., 26.666668),
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
            }
            costs
                .borrow_mut()
                .add(0, input_started.elapsed(), &format!("input:{jump}"));
            measured(&costs, 1, &format!("input:{jump}"), || settle(&mut d));
            let mut before: BTreeMap<usize, (f64, f64)> = BTreeMap::new();
            let mut accepted = 0;
            let started = std::time::Instant::now();
            let mut waiting_images = 0;
            for phase in 0..if external { 1000 } else { 5 } {
                let Some(rows) = render(
                    &mut d,
                    &mut bridge,
                    &format!(
                        "{}-{}:{jump}",
                        if wheel { "wheel" } else { "drag" },
                        if jump < 90 { "down" } else { "up" }
                    ),
                ) else {
                    waiting_images += usize::from(d.ui().images_pending());
                    continue;
                };
                if external && accepted == 0 && waiting_images > 0 {
                    eprintln!(
                        "wheel={jump} image_wait_frames={waiting_images} accepted_after={:?}",
                        started.elapsed()
                    );
                }
                accepted += 1;
                if wheel {
                    let motion = if accepted == 1 {
                        if jump < 90 { 40.25 } else { -37.25 }
                    } else {
                        0.
                    };
                    for (index, (old_y, old_h)) in &previous_rows {
                        if *old_y < 920. * 0.6666667
                            && old_y + old_h * 0.6666667 > 0.
                            && let Some((new_y, _)) = rows.get(index)
                        {
                            assert!(
                                (old_y - new_y - motion * 0.6666667).abs() < 1e-6,
                                "wheel={jump} phase={phase} row={index}: {old_y} -> {new_y}, expected delta {}",
                                motion * 0.6666667
                            );
                        }
                    }
                    previous_rows = rows.clone();
                }
                let visible = rows
                    .iter()
                    .filter(|(_, (y, h))| *y < 920. * 0.6666667 && y + h * 0.6666667 > 0.)
                    .count();
                assert!(
                    visible > 0,
                    "jump={jump} phase={phase} accepted an empty window"
                );
                for (index, (old_y, old_h)) in &before {
                    if *old_y < 920. * 0.6666667
                        && old_y + old_h * 0.6666667 > 0.
                        && let Some((new_y, _)) = rows.get(index)
                    {
                        assert!(
                            (old_y - new_y).abs() < 1e-6,
                            "jump={jump} phase={phase} row={index}: {old_y} -> {new_y}"
                        );
                    }
                }
                if external && !wheel && jump == 89 {
                    assert!(
                        rows.keys().next().copied().unwrap() > 0,
                        "drag must move to a later window"
                    );
                }
                before = rows;
                if accepted == 3 {
                    break;
                }
            }
            assert!(accepted >= 2, "far window did not become ready");
        }
        if external {
            eprintln!(
                "motion kind={} cadence={cadence:?} duration={:?}",
                if wheel { "wheel" } else { "drag" },
                motion_started.elapsed()
            );
            if !wheel {
                d.handle_pointer(
                    &mut bridge,
                    super::cursor::pointer(PointerType::Mouse, (876., 20.), Some(false)),
                )
                .unwrap();
                settle(&mut d);
            }
            for _ in 0..1000 {
                render(&mut d, &mut bridge, "drain");
                if !d.ui().images_pending() {
                    break;
                }
            }
            assert!(!d.ui().images_pending());
            costs.borrow().report();
        }
    }
}
