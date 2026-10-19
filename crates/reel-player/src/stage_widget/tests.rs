use super::*;
use crate::session::SessionUi;
#[cfg(feature = "window")]
use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_engine::{Epoch, Session, SessionId};
use reel_ui::{
    app::{GeometryLimits, RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{
        BridgeLimits, NoticeResult, PresentationBridge, PresentationNotice, PresentationOutcome,
        RecordedSource,
    },
    dpi::PhysicalSize,
    kurbo::{Affine, Rect},
    stage::NodeProps,
};
#[cfg(feature = "window")]
use reel_ui::{
    frame::{BuildLimits, FrameBuild, FrameInfo, RenderFrame, StepBudget},
    resources::ResourceSnapshot,
};
use std::sync::Arc;

struct Fixture {
    ui: SessionUi,
    bridge: PresentationBridge,
    widget: WidgetId,
    stage: Stage,
    root: NodeId,
    red: NodeId,
    blue: NodeId,
    frame: u64,
}

fn virtual_fixture(total: usize) -> (Fixture, NodeId, NodeId) {
    virtual_fixture_with_font(total, None)
}
fn virtual_fixture_with_font(
    total: usize,
    font: Option<reel_ui::peniko::Blob<u8>>,
) -> (Fixture, NodeId, NodeId) {
    use reel_ui::stage::{VirtualListView, VirtualRow};
    let mut f = Fixture::new(font);
    let mut stage = Stage::default();
    let row = stage
        .create(
            NodeKind::Button {
                label: None,
                listening: true,
            },
            NodeProps {
                height: Some(40.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let list = stage
        .create(
            NodeKind::VirtualList(VirtualListView {
                revision: 1,
                total_height: total as f64 * 42.,
                total_rows: total,
                rows: Arc::from([VirtualRow {
                    node: row,
                    index: 0,
                    offset: 0.,
                }]),
                navigation: None,
            }),
            NodeProps {
                padding: 4.,
                gap: 2.,
                ..Default::default()
            },
            vec![row],
        )
        .unwrap();
    let scroll = stage
        .create(
            NodeKind::Scroll {
                axis: ScrollAxis::Vertical,
                follow_end: false,
                align: reel_ui::widgets::ScrollAlign::Start,
                offset: Vec2::ZERO,
            },
            NodeProps {
                width: Some(200.),
                height: Some(100.),
                ..Default::default()
            },
            vec![list],
        )
        .unwrap();
    stage.replace(scroll).unwrap();
    f.stage = stage;
    f.root = scroll;
    (f, list, row)
}

#[test]
#[ignore = "requires REEL_SESSION_FONT for actual rich-text shaping"]
fn virtual_list_rich_text_layout_cost_depends_on_mounted_rows() {
    let bytes = std::fs::read(std::env::var_os("REEL_SESSION_FONT").unwrap()).unwrap();
    let mut counts = Vec::new();
    for total in [1000, 10_000, 100_000] {
        let (mut f, list, row) = virtual_fixture_with_font(total, Some(bytes.clone().into()));
        let rich = reel_core::text::Text::parse(
            "[b]A measured row[/b] with words that wrap across the available width. ".repeat(3),
        )
        .unwrap();
        f.stage
            .update(
                row,
                NodeKind::Text(rich),
                NodeProps {
                    font_size: 20.,
                    ..Default::default()
                },
            )
            .unwrap();
        let started = std::time::Instant::now();
        let first = f.record();
        f.accept(&first);
        let accepted = started.elapsed();
        let initial = f.layouts();
        let first_height =
            f.ui.take_events()
                .into_iter()
                .find_map(|e| match e {
                    NodeEvent::VirtualList(e) => Some(e.measurements[0].height),
                    _ => None,
                })
                .unwrap();
        assert!(first_height > 40.);
        counts.push(initial[&row].1);
        let props = f.stage.node(f.root).unwrap().props.clone();
        f.stage
            .update(
                f.root,
                NodeKind::Scroll {
                    axis: ScrollAxis::Vertical,
                    follow_end: false,
                    align: reel_ui::widgets::ScrollAlign::Start,
                    offset: Vec2::new(0., 10_000.),
                },
                props,
            )
            .unwrap();
        let moved = f.record();
        f.accept(&moved);
        assert_eq!(f.layouts(), initial, "scrolling reuses shaped text");
        f.ui.take_events();
        f.edit(f.root, |p| p.width = Some(100.));
        let narrow = f.record();
        f.accept(&narrow);
        let narrowed =
            f.ui.take_events()
                .into_iter()
                .find_map(|e| match e {
                    NodeEvent::VirtualList(e) if e.node == list => Some(e.measurements[0].height),
                    _ => None,
                })
                .unwrap();
        assert!(narrowed > first_height);
        assert_eq!(f.layouts()[&row].1, initial[&row].1 + 1);
        eprintln!(
            "virtual-rich-layout total={total} widgets={} first_row_layouts={} accepted_frame_us={} initial_height={first_height} narrow_height={narrowed}",
            initial.len(),
            initial[&row].1,
            accepted.as_micros()
        );
    }
    assert!(counts.iter().all(|c| *c == counts[0]));
}

#[test]
fn virtual_list_reports_only_mounted_rows_and_keeps_layouts_while_scrolling() {
    for total in [1_000, 10_000, 100_000] {
        let (mut f, list, row) = virtual_fixture(total);
        let initial = f.record();
        f.accept(&initial);
        let feedback =
            f.ui.take_events()
                .into_iter()
                .find_map(|event| match event {
                    NodeEvent::VirtualList(feedback) => Some(feedback),
                    _ => None,
                })
                .unwrap();
        assert_eq!(feedback.node, list);
        assert_eq!(feedback.width, 192.);
        assert_eq!(feedback.viewport, Size::new(200., 100.));
        assert_eq!(feedback.visible_start, -4.);
        assert_eq!(
            feedback.measurements.as_ref(),
            &[VirtualRowMeasurement {
                node: row,
                height: 40.
            }]
        );
        assert_eq!(
            f.layouts().len(),
            3,
            "total rows do not create placeholder widgets"
        );
        let before = f.layouts();
        let props = f.stage.node(f.root).unwrap().props.clone();
        f.stage
            .update(
                f.root,
                NodeKind::Scroll {
                    axis: ScrollAxis::Vertical,
                    follow_end: false,
                    align: reel_ui::widgets::ScrollAlign::Start,
                    offset: Vec2::new(0., 20_000.),
                },
                props,
            )
            .unwrap();
        let moved = f.record();
        f.accept(&moved);
        assert_eq!(
            f.layouts(),
            before,
            "a distant jump does not measure intermediate rows"
        );
        let feedback =
            f.ui.take_events()
                .into_iter()
                .find_map(|event| match event {
                    NodeEvent::VirtualList(feedback) => Some(feedback),
                    _ => None,
                })
                .unwrap();
        assert_eq!(feedback.visible_start, 19_996.);
        assert_eq!(feedback.measurements.len(), 1);
        let idle = f.record();
        f.accept(&idle);
        assert!(
            !f.ui
                .take_events()
                .iter()
                .any(|event| matches!(event, NodeEvent::VirtualList(_)))
        );
        assert_eq!(f.layouts(), before);
    }
}

#[test]
fn virtual_list_navigation_waits_for_accepted_rows_and_cancels_old_confirmation() {
    use reel_ui::input::{ActionPhase as P, NavigationAction as A, NavigationEvent};
    use reel_ui::stage::VirtualRow;
    for tab in [false, true] {
        let (mut f, list, first) = virtual_fixture(100);
        let nav = |action, phase| {
            TextEvent::Navigation(NavigationEvent {
                action,
                phase,
                value: 1.,
            })
        };
        let advance = || {
            if tab {
                TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab))
            } else {
                nav(A::Down, P::Pressed)
            }
        };
        let initial = f.record();
        f.accept(&initial);
        f.ui.handle_text(&mut f.bridge, advance()).unwrap();
        assert_eq!(
            f.bridge.root().focused_widget(),
            Some(f.layouts()[&first].0)
        );
        let next = f.ui.handle_text(&mut f.bridge, advance()).unwrap();
        assert!(next.events.iter().any(|event| matches!(event,
            NodeEvent::VirtualList(feedback) if feedback.navigation == Some(VirtualListNavigation { request: 1, ready: false, index: 1, resolved_index: 1, forward: true })
        )));
        assert!(
            next.events
                .iter()
                .filter_map(|event| match event {
                    NodeEvent::VirtualList(feedback) => Some(feedback),
                    _ => None,
                })
                .all(|feedback| feedback.retained.contains(&first))
        );
        f.ui.handle_text(&mut f.bridge, nav(A::Confirm, P::Pressed))
            .unwrap();
        let second = f
            .stage
            .create(
                NodeKind::Button {
                    label: None,
                    listening: true,
                },
                NodeProps {
                    height: Some(40.),
                    ..Default::default()
                },
                vec![],
            )
            .unwrap();
        f.stage.add(list, second, 1).unwrap();
        let node = f.stage.node(list).unwrap();
        let NodeKind::VirtualList(mut view) = node.kind.clone() else {
            panic!()
        };
        let props = node.props.clone();
        view.revision += 1;
        view.rows = Arc::from([
            VirtualRow {
                node: first,
                index: 0,
                offset: 0.,
            },
            VirtualRow {
                node: second,
                index: 1,
                offset: 42.,
            },
        ]);
        view.navigation = Some(VirtualListNavigation {
            request: 1,
            ready: true,
            index: 1,
            resolved_index: 1,
            forward: true,
        });
        f.stage
            .update(list, NodeKind::VirtualList(view), props)
            .unwrap();
        let pending = f.record();
        assert!(
            f.ui.take_events()
                .iter()
                .filter_map(|event| match event {
                    NodeEvent::VirtualList(feedback) => Some(feedback),
                    _ => None,
                })
                .all(|feedback| feedback.navigation.is_some()),
            "recording does not complete navigation"
        );
        assert_eq!(
            f.bridge.root().focused_widget(),
            Some(f.layouts()[&first].0)
        );
        f.accept(&pending);
        assert_eq!(
            f.bridge.root().focused_widget(),
            Some(f.layouts()[&second].0)
        );
        assert!(
            f.ui.take_events().iter().any(|event| matches!(event,
                NodeEvent::VirtualList(feedback) if feedback.navigation.is_none()
            )),
            "accepted focus completes the engine request"
        );
        let release =
            f.ui.handle_text(&mut f.bridge, nav(A::Confirm, P::Released))
                .unwrap();
        assert!(
            !release
                .events
                .iter()
                .any(|event| matches!(event, NodeEvent::Click(_)))
        );
        f.ui.handle_text(&mut f.bridge, nav(A::Confirm, P::Pressed))
            .unwrap();
        let release =
            f.ui.handle_text(&mut f.bridge, nav(A::Confirm, P::Released))
                .unwrap();
        assert!(
            release
                .events
                .iter()
                .any(|event| matches!(event, NodeEvent::Click(click) if click.node == second))
        );
    }
}

#[test]
fn virtual_list_measures_changed_rows_and_coalesces_latest_viewport() {
    let (mut f, list, row) = virtual_fixture(10_000);
    let initial = f.record();
    f.accept(&initial);
    f.ui.take_events();
    f.edit(row, |props| props.height = Some(85.));
    f.edit(f.root, |props| props.width = Some(140.));
    let resized = f.record();
    f.accept(&resized);
    for offset in [50., 100., 200.] {
        let props = f.stage.node(f.root).unwrap().props.clone();
        f.stage
            .update(
                f.root,
                NodeKind::Scroll {
                    axis: ScrollAxis::Vertical,
                    follow_end: false,
                    align: reel_ui::widgets::ScrollAlign::Start,
                    offset: Vec2::new(0., offset),
                },
                props,
            )
            .unwrap();
        let frame = f.record();
        f.accept(&frame);
    }
    let feedback: Vec<_> =
        f.ui.take_events()
            .into_iter()
            .filter_map(|event| match event {
                NodeEvent::VirtualList(feedback) => Some(feedback),
                _ => None,
            })
            .collect();
    assert_eq!(
        feedback.len(),
        1,
        "unconsumed demand has one latest value per list"
    );
    assert_eq!(feedback[0].node, list);
    assert_eq!(feedback[0].width, 132.);
    assert_eq!(feedback[0].offset.y, 200.);
    assert_eq!(
        feedback[0].measurements.as_ref(),
        &[VirtualRowMeasurement {
            node: row,
            height: 85.
        }]
    );
}

#[test]
fn virtual_list_navigation_finds_controls_beyond_initial_nonfocusable_rows() {
    use reel_ui::stage::VirtualRow;
    for tab in [false, true] {
        let (mut f, list, first) = virtual_fixture(100);
        let props = f.stage.node(first).unwrap().props.clone();
        f.stage.update(first, NodeKind::Stack, props).unwrap();
        let initial = f.record();
        f.accept(&initial);
        f.ui.take_events();
        let event = if tab {
            TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab))
        } else {
            TextEvent::Navigation(reel_ui::input::NavigationEvent {
                action: reel_ui::input::NavigationAction::Down,
                phase: reel_ui::input::ActionPhase::Pressed,
                value: 1.,
            })
        };
        let request = f.ui.handle_text(&mut f.bridge, event).unwrap();
        assert!(request.events.iter().any(|event| matches!(event,
            NodeEvent::VirtualList(feedback) if feedback.navigation == Some(VirtualListNavigation { request: 1, ready: false, index: 1, resolved_index: 1, forward: true })
        )));
        for index in [1, 2] {
            let kind = if index == 1 {
                NodeKind::Stack
            } else {
                NodeKind::Button {
                    label: None,
                    listening: true,
                }
            };
            let child = f
                .stage
                .create(
                    kind,
                    NodeProps {
                        height: Some(40.),
                        ..Default::default()
                    },
                    vec![],
                )
                .unwrap();
            f.stage.add(list, child, index).unwrap();
            let node = f.stage.node(list).unwrap();
            let NodeKind::VirtualList(mut view) = node.kind.clone() else {
                panic!()
            };
            let props = node.props.clone();
            let mut rows = view.rows.to_vec();
            rows.push(VirtualRow {
                node: child,
                index,
                offset: index as f64 * 42.,
            });
            view.rows = rows.into();
            view.revision += 1;
            view.navigation = Some(VirtualListNavigation {
                request: index as u64,
                ready: true,
                resolved_index: index,
                index,
                forward: true,
            });
            f.stage
                .update(list, NodeKind::VirtualList(view), props)
                .unwrap();
            let candidate = f.record();
            assert!(f.bridge.root().focused_widget().is_none());
            f.accept(&candidate);
            let feedback = f.ui.take_events();
            if index == 1 {
                assert!(f.bridge.root().focused_widget().is_none());
                assert!(feedback.iter().any(|event| matches!(event,
                    NodeEvent::VirtualList(feedback) if feedback.navigation == Some(VirtualListNavigation { request: 2, ready: false, index: 2, resolved_index: 2, forward: true })
                )), "nonfocusable rows continue only after acceptance");
            } else {
                assert_eq!(
                    f.bridge.root().focused_widget(),
                    Some(f.layouts()[&child].0)
                );
            }
        }
    }
}
impl Fixture {
    fn new(font: Option<reel_ui::peniko::Blob<u8>>) -> Self {
        let mut stage = Stage::default();
        let red = stage
            .create(
                NodeKind::Stack,
                NodeProps {
                    x: 30.,
                    y: 20.,
                    width: Some(60.),
                    height: Some(30.),
                    background: Some([255, 0, 0, 255]),
                    ..Default::default()
                },
                vec![],
            )
            .unwrap();
        let blue = stage
            .create(
                NodeKind::Text("Stable caption".into()),
                NodeProps {
                    x: 80.,
                    y: 40.,
                    width: Some(60.),
                    height: Some(60.),
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
                    x: 200.,
                    y: 100.,
                    width: Some(100.),
                    height: Some(80.),
                    rotation: 90.,
                    scale_x: 2.,
                    anchor_x: 0.5,
                    anchor_y: 0.5,
                    clip: true,
                    ..Default::default()
                },
                vec![red, blue],
            )
            .unwrap();
        stage.replace(root).unwrap();
        let mut ui = SessionUi::new(Session::new(SessionId(1), Epoch(1)).unwrap()).unwrap();
        ui.set_stage(&stage);
        let widget = ui.widget().prepare();
        let id = widget.id();
        let root_widget = RenderRoot::new(
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
            root_widget,
            SurfaceEpoch(1),
            BridgeLimits {
                geometry: GeometryLimits { max_bytes: 1 << 20 },
                max_candidates: 4,
                max_bridge_bytes: 64 << 10,
                max_snapshot_bytes: 8 << 20,
            },
        )
        .unwrap();
        Self {
            ui,
            bridge,
            widget: id,
            stage,
            root,
            red,
            blue,
            frame: 0,
        }
    }
    fn edit(&mut self, id: NodeId, edit: impl FnOnce(&mut NodeProps)) {
        let node = self.stage.node(id).unwrap();
        let mut props = node.props.clone();
        edit(&mut props);
        self.stage.update(id, node.kind.clone(), props).unwrap();
    }
    fn record(&mut self) -> TestFrame {
        self.ui.set_stage(&self.stage);
        self.ui.sync(&mut self.bridge, self.widget).unwrap();
        self.frame += 1;
        let (source, receipt) = self
            .ui
            .record(&mut self.bridge, self.widget, FrameId(self.frame))
            .unwrap();
        TestFrame { source, receipt }
    }
    fn accept(&mut self, source: &TestFrame) {
        assert_eq!(
            self.bridge.resolve(PresentationNotice::new(
                source.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(source.key().frame_id())
            )),
            NoticeResult::Accepted
        );
        self.ui.presented(&self.bridge, &source.receipt);
    }
    fn layouts(&self) -> BTreeMap<NodeId, (WidgetId, usize)> {
        fn walk(
            widget: WidgetRef<'_, dyn Widget>,
            nodes: &mut BTreeMap<NodeId, (WidgetId, usize)>,
        ) {
            if let Some(scene) = widget.downcast::<SceneWidget>() {
                nodes.insert(scene.node.id, (widget.id(), scene.layouts));
            }
            for child in widget.children() {
                walk(child, nodes);
            }
        }
        let mut nodes = BTreeMap::new();
        walk(
            self.bridge.root().get_widget(self.widget).unwrap(),
            &mut nodes,
        );
        nodes
    }
    #[cfg(feature = "window")]
    fn render_frame(&mut self) -> RenderFrame {
        let source = self.record();
        self.accept(&source);
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 32,
                max_bytes: 32 << 20,
            },
        );
        let resources = ResourceSnapshot::capture(std::iter::empty(), &pool).unwrap();
        let mut build = FrameBuild::new(
            source.source,
            resources,
            FrameInfo {
                logical_size: [640., 420.],
                physical_size: [640, 420],
            },
            BuildLimits { max_bytes: 8 << 20 },
            &pool,
        )
        .unwrap();
        while !build
            .step(StepBudget {
                max_commands: 64,
                max_units: 8192,
                max_bytes: 64 << 10,
            })
            .unwrap()
            .complete
        {}
        build.finish().unwrap()
    }
}
fn affine(actual: Affine, expected: [f64; 6]) {
    assert!(
        actual
            .as_coeffs()
            .into_iter()
            .zip(expected)
            .all(|(a, b)| (a - b).abs() < 1e-9),
        "actual={actual:?}, expected={expected:?}"
    );
}

#[test]
#[ignore = "requires REEL_LATIN_FONT without CJK and REEL_CJK_FONT covering Latin and CJK"]
fn configured_fonts_shape_mixed_text_and_live_family_changes_preserve_widget_identity() {
    use reel_ui::{
        frame::{BuildLimits, FrameBuild, FrameDraw, FrameInfo, FrameOp, StepBudget},
        imaging::record::{Command, Draw},
        reel_text::fontique::{Collection, CollectionOptions},
        resources::{ResourceData, ResourceSnapshot},
    };
    let pool = reel_core::budget::BudgetPool::new(
        reel_core::budget::BudgetCategory::CpuOutput,
        reel_core::budget::QueueLimits {
            max_requests: 64,
            max_bytes: 64 << 20,
        },
    );
    let assets = crate::assets::Assets::directory(Default::default());
    let (send, receive) = std::sync::mpsc::channel();
    assets.set_waker(std::sync::Arc::new(move || {
        let _ = send.send(());
    }));
    let font = |key| {
        let path = std::env::var_os(key).expect(key);
        let id = assets.request(path.to_str().unwrap(), crate::assets::AssetKind::Font);
        let lease = loop {
            if let Some(lease) = assets.poll(id).unwrap() {
                break lease;
            }
            receive
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("font read must wake the host");
        };
        let ResourceData::Font(data) = lease.data() else {
            unreachable!()
        };
        (data.clone(), lease)
    };
    let (latin, latin_lease) = font("REEL_LATIN_FONT");
    let (cjk, cjk_lease) = font("REEL_CJK_FONT");
    let mut collection = Collection::new(CollectionOptions {
        system_fonts: false,
        ..Default::default()
    });
    let families = collection.register_fonts(cjk.clone(), None);
    let family = collection.family_name(families[0].0).unwrap().to_owned();
    let mut f = Fixture::new(None);
    f.stage
        .update(
            f.blue,
            NodeKind::Text("ABC中文".into()),
            NodeProps {
                width: Some(400.),
                height: Some(60.),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(f.bridge.set_default_fonts(&[latin.clone(), cjk.clone()]));
    let used_fonts = |source: &RecordedSource| {
        let scene = source.scene();
        scene
            .commands()
            .iter()
            .filter_map(|command| match command {
                Command::Draw(id) => match scene.draw_op(*id) {
                    Draw::GlyphRun(run) => {
                        assert!(run.glyphs.iter().all(|glyph| glyph.id != 0));
                        Some(run.font.data.id())
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    let mixed = f.record();
    f.accept(&mixed);
    assert_eq!(used_fonts(&mixed), [latin.id(), cjk.id()].into());
    let original = f.layouts();
    f.edit(f.blue, |props| props.font = family.clone());
    let selected = f.record();
    f.accept(&selected);
    assert_eq!(used_fonts(&selected), [cjk.id()].into());
    assert_eq!(f.layouts()[&f.blue].0, original[&f.blue].0);
    assert!(f.layouts()[&f.blue].1 > original[&f.blue].1);
    assert!(f.bridge.set_default_fonts(&[cjk.clone(), latin]));
    f.edit(f.blue, |props| props.font = "system-ui".into());
    let reordered = f.record();
    f.accept(&reordered);
    assert_eq!(used_fonts(&reordered), [cjk.id()].into());
    // Old frames retain their original font data and glyph selection.
    assert_eq!(used_fonts(&mixed).len(), 2);
    let mut build = FrameBuild::new(
        mixed.source,
        ResourceSnapshot::capture([Ok(latin_lease), Ok(cjk_lease)].into_iter(), &pool).unwrap(),
        FrameInfo {
            logical_size: [640., 420.],
            physical_size: [640, 420],
        },
        BuildLimits { max_bytes: 8 << 20 },
        &pool,
    )
    .unwrap();
    while !build
        .step(StepBudget {
            max_commands: 64,
            max_units: 8192,
            max_bytes: 64 << 10,
        })
        .unwrap()
        .complete
    {}
    let frame = build.finish().unwrap();
    let frame_fonts = frame
        .commands()
        .iter()
        .filter_map(|command| match &command.op {
            FrameOp::Draw(FrameDraw::GlyphRun(run)) => Some(run.font),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(frame_fonts.len(), 2);
    assert!(!f.bridge.set_default_fonts(&[]));
    eprintln!("mixed Latin/CJK fallback, live family selection and priority reorder verified");
}

#[test]
fn structural_edits_keep_retained_widgets_and_remove_retired_geometry() {
    let font = std::env::var_os("REEL_SESSION_FONT")
        .map(|path| reel_ui::peniko::Blob::new(Arc::new(std::fs::read(path).unwrap())));
    eprintln!("actual font supplied={}", font.is_some());
    let mut f = Fixture::new(font);
    let first = f.record();
    f.accept(&first);
    let original = f.layouts();
    let fresh = f
        .stage
        .create(
            NodeKind::Stack,
            NodeProps {
                width: Some(15.),
                height: Some(10.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    f.stage.add(f.root, fresh, 1).unwrap();
    let inserted = f.record();
    f.accept(&inserted);
    let after = f.layouts();
    for id in [f.root, f.red, f.blue] {
        assert_eq!(after[&id].0, original[&id].0);
    }
    assert!(inserted.geometry().get(after[&fresh].0).is_some());
    assert_eq!(
        after[&f.blue].1, original[&f.blue].1,
        "unchanged text keeps its layout"
    );
    eprintln!(
        "retained_widgets=3, unchanged_text_additional_layouts={}",
        after[&f.blue].1 - original[&f.blue].1
    );
    f.stage.remove(f.red).unwrap();
    let removed = f.record();
    f.accept(&removed);
    assert!(removed.geometry().get(original[&f.red].0).is_none());
    assert_eq!(f.layouts()[&f.blue].0, original[&f.blue].0);
    assert_eq!(f.layouts()[&fresh].0, after[&fresh].0);
    // The accepted old frame continues to describe its own immutable geometry.
    assert!(first.geometry().get(original[&f.red].0).is_some());
    f.stage.clear_children(f.root).unwrap();
    let cleared = f.record();
    f.accept(&cleared);
    assert_eq!(f.layouts().len(), 1);
    assert_eq!(f.layouts()[&f.root].0, original[&f.root].0);
    assert!(cleared.geometry().get(original[&f.blue].0).is_none());
    assert!(cleared.geometry().get(after[&fresh].0).is_none());
}

#[test]
fn content_caps_and_alignment_follow_parent_space_without_shifting_flow() {
    let mut f = Fixture::new(None);
    f.edit(f.root, |p| {
        *p = NodeProps {
            max_width: Some(200.),
            padding: 10.,
            gap: 7.,
            ..Default::default()
        }
    });
    let props = f.stage.node(f.root).unwrap().props.clone();
    f.stage.update(f.root, NodeKind::Column, props).unwrap();
    f.edit(f.red, |p| {
        *p = NodeProps {
            width: Some(240.),
            max_width: Some(180.),
            height: Some(30.),
            ..Default::default()
        }
    });
    f.edit(f.blue, |p| {
        *p = NodeProps {
            width: Some(60.),
            height: Some(20.),
            align_x: 0.5,
            align_y: 1.,
            ..Default::default()
        }
    });
    let first = f.record();
    f.accept(&first);
    let ids = f.layouts();
    let geometry = |frame: &RecordedSource, id| frame.geometry().get(ids[&id].0).unwrap().clone();
    assert_eq!(geometry(&first, f.root).size, Size::new(200., 77.));
    assert_eq!(geometry(&first, f.red).size, Size::new(180., 30.));
    assert_eq!(
        geometry(&first, f.blue).window_transform.translation(),
        Vec2::new(70., 47.)
    );
    // Content shrink changes the parent and re-centers the existing child.
    f.edit(f.red, |p| p.width = Some(100.));
    let smaller = f.record();
    f.accept(&smaller);
    assert_eq!(geometry(&smaller, f.root).size, Size::new(120., 77.));
    assert_eq!(
        geometry(&smaller, f.blue).window_transform.translation(),
        Vec2::new(30., 47.)
    );
    f.edit(f.blue, |p| p.align_x = 1.);
    let aligned = f.record();
    f.accept(&aligned);
    assert_eq!(
        geometry(&aligned, f.blue).window_transform.translation(),
        Vec2::new(50., 47.)
    );
    assert_eq!(f.layouts()[&f.blue].0, ids[&f.blue].0);
    // Stack alignment uses both axes; a stretched axis overrides the size cap.
    f.edit(f.root, |p| {
        p.width = Some(200.);
        p.height = Some(120.);
    });
    let props = f.stage.node(f.root).unwrap().props.clone();
    f.stage.update(f.root, NodeKind::Stack, props).unwrap();
    f.edit(f.blue, |p| {
        p.align_x = 0.5;
        p.align_y = 0.5;
        p.max_width = Some(20.);
        p.stretch = reel_ui::stage::Stretch::X;
    });
    let stretched = f.record();
    f.accept(&stretched);
    assert_eq!(geometry(&stretched, f.blue).size, Size::new(180., 20.));
    assert_eq!(
        geometry(&stretched, f.blue).window_transform.translation(),
        Vec2::new(10., 50.)
    );
}

#[test]
fn stretched_background_follows_content_without_changing_flow_or_scroll_extent() {
    use reel_ui::stage::Stretch;
    let mut f = Fixture::new(None);
    f.edit(f.root, |p| {
        *p = NodeProps {
            width: Some(100.),
            padding: 10.,
            gap: 7.,
            ..Default::default()
        };
    });
    let props = f.stage.node(f.root).unwrap().props.clone();
    f.stage.update(f.root, NodeKind::Column, props).unwrap();
    f.edit(f.red, |p| {
        *p = NodeProps {
            width: Some(1000.),
            height: Some(1000.),
            stretch: Stretch::Both,
            background: Some([255, 0, 0, 255]),
            ..Default::default()
        };
    });
    f.edit(f.blue, |p| {
        *p = NodeProps {
            width: Some(60.),
            height: Some(30.),
            ..Default::default()
        };
    });
    let tail = f
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
    f.stage.add(f.root, tail, 2).unwrap();
    let first = f.record();
    f.accept(&first);
    let ids = f.layouts();
    let root = ids[&f.root].0;
    let background = ids[&f.red].0;
    assert_eq!(
        first.geometry().get(root).unwrap().size,
        Size::new(100., 77.)
    );
    assert_eq!(
        first.geometry().get(background).unwrap().size,
        Size::new(80., 57.)
    );
    assert_eq!(
        first
            .geometry()
            .get(ids[&tail].0)
            .unwrap()
            .window_transform
            .translation(),
        Vec2::new(10., 47.)
    );
    f.edit(f.blue, |p| p.height = Some(70.));
    let resized = f.record();
    f.accept(&resized);
    assert_eq!(
        resized.geometry().get(root).unwrap().size,
        Size::new(100., 117.)
    );
    assert_eq!(
        resized.geometry().get(background).unwrap().size,
        Size::new(80., 97.)
    );
    assert_eq!(f.layouts()[&f.red], (background, ids[&f.red].1 + 1));
    f.edit(f.root, |p| p.height = Some(80.));
    let props = f.stage.node(f.root).unwrap().props.clone();
    f.stage
        .update(
            f.root,
            NodeKind::Scroll {
                axis: ScrollAxis::Vertical,
                offset: Vec2::ZERO,
                follow_end: false,
                align: reel_ui::widgets::ScrollAlign::Start,
            },
            props,
        )
        .unwrap();
    let scrolled = f.record();
    f.accept(&scrolled);
    assert_eq!(
        scrolled.geometry().get(background).unwrap().size,
        Size::new(80., 60.)
    );
    let scene = f
        .bridge
        .root()
        .get_widget(root)
        .unwrap()
        .downcast::<SceneWidget>()
        .unwrap();
    assert_eq!(scene.scroll.content(), Size::new(80., 97.));
}

#[test]
fn nested_stage_transforms_share_geometry_and_preserve_layout_and_identity() {
    let font = std::env::var_os("REEL_SESSION_FONT")
        .map(|path| reel_ui::peniko::Blob::new(Arc::new(std::fs::read(path).unwrap())));
    eprintln!("actual font supplied={}", font.is_some());
    let mut f = Fixture::new(font);
    let first = f.record();
    f.accept(&first);
    let before = f.layouts();
    let root = before[&f.root].0;
    let red = before[&f.red].0;
    let blue = before[&f.blue].0;
    affine(
        first.geometry().get(root).unwrap().window_transform,
        [0., 2., -1., 0., 240., 0.],
    );
    affine(
        first.geometry().get(red).unwrap().window_transform,
        [0., 2., -1., 0., 220., 60.],
    );
    assert_eq!(
        first.geometry().get(root).unwrap().clip_path,
        Some(Rect::new(0., 0., 100., 80.))
    );
    assert_eq!(
        first.geometry().get(blue).unwrap().size,
        Size::new(60., 60.)
    );
    for step in 1..=32 {
        f.edit(f.root, |p| {
            p.x += 0.25;
            p.rotation += 0.5;
            p.anchor_y -= 0.01;
        });
        f.edit(f.red, |p| {
            p.scale_x = -1.;
            p.opacity = 1. - f64::from(step) / 64.;
        });
        let next = f.record();
        f.accept(&next);
        assert_eq!(
            f.layouts(),
            before,
            "pure composition must not invoke layout"
        );
    }
    eprintln!(
        "32 transform/opacity updates: unchanged widget identities and layout counts {before:?}"
    );
    // The frozen old frame remains independent of the latest composed geometry.
    affine(
        first.geometry().get(root).unwrap().window_transform,
        [0., 2., -1., 0., 240., 0.],
    );
    drop(first);
    f.edit(f.root, |p| {
        p.x = 200.;
        p.rotation = 90.;
        p.anchor_y = 0.5;
        p.width = Some(200.);
    });
    let resized = f.record();
    f.accept(&resized);
    affine(
        resized.geometry().get(root).unwrap().window_transform,
        [0., 2., -1., 0., 240., -100.],
    );
    assert!(f.layouts()[&f.root].1 > before[&f.root].1);
    drop(resized);
    // Singular transforms preserve the model and identities, and compose back normally.
    let layouts = f.layouts();
    f.edit(f.root, |p| p.scale_x = 0.);
    let collapsed = f.record();
    f.accept(&collapsed);
    assert_eq!(
        collapsed
            .geometry()
            .get(red)
            .unwrap()
            .window_transform
            .determinant(),
        0.
    );
    assert_eq!(f.layouts(), layouts);
    drop(collapsed);
    f.edit(f.root, |p| p.scale_x = -2.);
    let restored = f.record();
    f.accept(&restored);
    assert!(
        restored
            .geometry()
            .get(root)
            .unwrap()
            .window_transform
            .determinant()
            < 0.
    );
    assert_eq!(f.layouts(), layouts);
    drop(restored);

    // Container flow measures the original boxes, irrespective of their transforms.
    let left = f
        .stage
        .create(
            NodeKind::Stack,
            NodeProps {
                width: Some(20.),
                height: Some(10.),
                scale_x: 3.,
                rotation: 90.,
                anchor_x: 0.5,
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let right = f
        .stage
        .create(
            NodeKind::Stack,
            NodeProps {
                width: Some(20.),
                height: Some(10.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let row = f
        .stage
        .create(
            NodeKind::Row,
            NodeProps {
                x: 100.,
                y: 50.,
                gap: 8.,
                padding: 4.,
                ..Default::default()
            },
            vec![left, right],
        )
        .unwrap();
    f.stage.replace(row).unwrap();
    let flow = f.record();
    f.accept(&flow);
    let ids = f.layouts();
    assert_eq!(
        flow.geometry().get(ids[&row].0).unwrap().size,
        Size::new(56., 18.)
    );
    affine(
        flow.geometry().get(ids[&left].0).unwrap().window_transform,
        [0., 3., -1., 0., 104., 24.],
    );
    affine(
        flow.geometry().get(ids[&right].0).unwrap().window_transform,
        [1., 0., 0., 1., 132., 54.],
    );
    drop(flow);
    f.edit(left, |p| p.visible = false);
    let hidden = f.record();
    f.accept(&hidden);
    assert_eq!(
        hidden.geometry().get(ids[&row].0).unwrap().size,
        Size::new(28., 18.)
    );
    affine(
        hidden
            .geometry()
            .get(ids[&right].0)
            .unwrap()
            .window_transform,
        [1., 0., 0., 1., 104., 54.],
    );
    assert!(hidden.geometry().get(ids[&left].0).unwrap().is_stashed);
}

#[test]
fn particle_bounds_follow_captured_birth_extent_after_resize_and_restart() {
    use reel_ui::{
        app::GeometryRecord,
        peniko::{Blob, ImageAlphaType, ImageFormat},
        stage::particles::{ParticleBatch, ParticleConfig, ParticleFrame},
    };
    let config = Arc::new(ParticleConfig {
        source: "sprite.png".into(),
        origin: None,
        rate: 100.,
        burst: 0,
        lifetime: 5.,
        lifetime_spread: 0.,
        seed: 42,
        velocity: [-20., 60.],
        spread: [30., 20.],
        acceleration: [10., -12.],
        size: 24.,
        size_spread: 8.,
        size_end: None,
        opacity: [1., 1.],
        spin: 90.,
        spin_spread: 40.,
    });
    let mut frame = ParticleFrame {
        elapsed: 1.,
        duration: None,
        extent: [1000., 700.],
    };
    let mut stage = Stage::default();
    let id = stage
        .create(
            NodeKind::Particles(ParticleBatch {
                config: config.clone(),
                frame: Some(frame),
            }),
            NodeProps {
                width: Some(1000.),
                height: Some(700.),
                x: -600.,
                y: -400.,
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    stage.replace(id).unwrap();
    let mut view = StageView::capture(&stage);
    Arc::make_mut(view.root.as_mut().unwrap()).images.image = Some(ImageData {
        data: Blob::new(Arc::new(vec![255_u8; 2 * 4 * 4])),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: 2,
        height: 4,
    });
    let widget = NewWidget::new(StageWidget::new(Some(&view)));
    let widget_id = widget.id();
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
            geometry: GeometryLimits { max_bytes: 1 << 20 },
            max_candidates: 4,
            max_bridge_bytes: 64 << 10,
            max_snapshot_bytes: 8 << 20,
        },
    )
    .unwrap();
    fn record(
        bridge: &mut PresentationBridge,
        widget: WidgetId,
        view: &StageView,
        frame: u64,
    ) -> (GeometryRecord, usize) {
        bridge.edit_widget(widget, |mut w| StageWidget::sync(&mut w.downcast(), view));
        let source = bridge.record(FrameId(frame)).unwrap();
        let root = bridge.root().get_widget(widget).unwrap();
        let children = root.children();
        let particles = children[0].downcast::<SceneWidget>().unwrap();
        (
            source.geometry().get(particles.id()).unwrap().clone(),
            particles.layouts,
        )
    }
    fn assert_covers_sprites(geometry: &GeometryRecord, batch: &ParticleBatch) {
        let viewport = Rect::new(0., 0., 640., 420.);
        let mut visible = 0;
        for sprite in batch.samples() {
            let transform = geometry.window_transform
                * Affine::translate((sprite.position[0], sprite.position[1]))
                * Affine::rotate(sprite.rotation.to_radians());
            // The fixture image is twice as high as it is wide.
            let bounds = transform.transform_rect_bbox(Rect::new(
                -sprite.size * 0.5,
                -sprite.size,
                sprite.size * 0.5,
                sprite.size,
            ));
            assert_eq!(geometry.bounding_box.union(bounds), geometry.bounding_box);
            visible += usize::from(bounds.intersect(viewport).area() > 0.);
        }
        assert!(
            visible > 0,
            "sprites beyond the shrunken layout remain in the viewport"
        );
    }
    let (_, original_layouts) = record(&mut bridge, widget_id, &view, 1);
    let node = Arc::make_mut(&mut Arc::make_mut(view.root.as_mut().unwrap()).node);
    node.props.width = Some(10.);
    node.props.height = Some(5.);
    let (small, resized_layouts) = record(&mut bridge, widget_id, &view, 2);
    assert!(resized_layouts > original_layouts);
    assert_eq!(small.size, Size::new(10., 5.));
    assert_eq!(
        small
            .window_transform
            .transform_rect_bbox(small.size.to_rect())
            .intersect(Rect::new(0., 0., 640., 420.))
            .area(),
        0.
    );
    assert_covers_sprites(
        &small,
        &ParticleBatch {
            config: config.clone(),
            frame: Some(frame),
        },
    );

    // A restarted emission can capture a different extent even if its final
    // authored dimensions equal the last presentation's dimensions.
    frame.extent = [1400., 1000.];
    Arc::make_mut(&mut Arc::make_mut(view.root.as_mut().unwrap()).node).kind =
        NodeKind::Particles(ParticleBatch {
            config: config.clone(),
            frame: Some(frame),
        });
    let (restarted, restarted_layouts) = record(&mut bridge, widget_id, &view, 3);
    assert!(restarted_layouts > resized_layouts);
    assert!(restarted.bounding_box.x1 > small.bounding_box.x1);
    assert!(restarted.bounding_box.y1 > small.bounding_box.y1);
    assert_covers_sprites(
        &restarted,
        &ParticleBatch {
            config: config.clone(),
            frame: Some(frame),
        },
    );

    for step in 4..20 {
        frame.elapsed += 0.1;
        let node = Arc::make_mut(&mut Arc::make_mut(view.root.as_mut().unwrap()).node);
        node.props.rotation = -17.;
        node.props.scale_x = 1.5;
        let batch = ParticleBatch {
            config: config.clone(),
            frame: Some(frame),
        };
        node.kind = NodeKind::Particles(batch.clone());
        let (geometry, layouts) = record(&mut bridge, widget_id, &view, step);
        assert_eq!(
            layouts, restarted_layouts,
            "time and affine motion remain render/compose only"
        );
        assert_covers_sprites(&geometry, &batch);
    }
}

#[cfg(feature = "window")]
#[test]
#[ignore = "requires actual graphics adapter"]
fn transformed_stage_pixels_obey_group_clip_opacity_mirroring_and_zero_scale() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    let mut f = Fixture::new(None);
    let instance = wgpu::Instance::default();
    let gpu = pollster::block_on(Gpu::request(
        &instance,
        None,
        DeviceGeneration(1),
        GpuLimits::default(),
    ))
    .unwrap();
    eprintln!("adapter={:?}", gpu.adapter.get_info());
    let mut renderer = Renderer::new(gpu);
    let frame = f.render_frame();
    let pixels = renderer
        .readback(renderer.prepare(&frame).unwrap())
        .unwrap();
    let background = pixels[300 * 640 + 300];
    let near = |actual: [f32; 4], expected: [f32; 4]| {
        assert!(
            actual
                .into_iter()
                .zip(expected)
                .all(|(a, b)| (a - b).abs() < 0.006),
            "actual={actual:?}, expected={expected:?}"
        );
    };
    near(pixels[120 * 640 + 205], [1., 0., 0., 1.]);
    near(pixels[180 * 640 + 180], [0., 0., 1., 1.]);
    near(pixels[220 * 640 + 180], background);
    near(pixels[180 * 640 + 150], background);
    f.edit(f.root, |p| {
        p.scale_x = -2.;
        p.y = 240.;
        p.opacity = 0.5;
    });
    let frame = f.render_frame();
    let mirrored = renderer
        .readback(renderer.prepare(&frame).unwrap())
        .unwrap();
    let over = |pixel: [f32; 4]| std::array::from_fn(|i| pixel[i] * 0.5 + background[i] * 0.5);
    near(mirrored[220 * 640 + 205], over([1., 0., 0., 1.]));
    near(mirrored[160 * 640 + 180], over([0., 0., 1., 1.]));
    near(mirrored[120 * 640 + 180], background);
    f.edit(f.root, |p| p.scale_y = 0.);
    let frame = f.render_frame();
    let collapsed = renderer
        .readback(renderer.prepare(&frame).unwrap())
        .unwrap();
    for (x, y) in [(205, 220), (180, 160), (300, 300)] {
        near(collapsed[y * 640 + x], background);
    }
    // The same renderer draws stateful controls, including endpoint thumb centers.
    let range = reel_ui::widgets::SliderRange::new(0., 1., None).unwrap();
    let slider = f
        .stage
        .create(
            NodeKind::Slider {
                range,
                value: 0.,
                listening: false,
            },
            NodeProps {
                width: Some(220.),
                height: Some(40.),
                color: [255, 0, 0, 255],
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let toggle = f
        .stage
        .create(
            NodeKind::Toggle {
                label: None,
                value: true,
                listening: false,
                appearance: reel_ui::stage::ToggleAppearance::Switch,
            },
            NodeProps {
                y: 60.,
                width: Some(100.),
                height: Some(40.),
                color: [0, 255, 0, 255],
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let controls = f
        .stage
        .create(NodeKind::Stack, NodeProps::default(), vec![slider, toggle])
        .unwrap();
    f.stage.replace(controls).unwrap();
    let frame = f.render_frame();
    let start = renderer
        .readback(renderer.prepare(&frame).unwrap())
        .unwrap();
    near(start[20 * 640 + 10], [1., 0., 0., 1.]);
    near(start[80 * 640 + 19], [0., 1., 0., 1.]);
    let props = f.stage.node(slider).unwrap().props.clone();
    f.stage
        .update(
            slider,
            NodeKind::Slider {
                range,
                value: 1.,
                listening: false,
            },
            props,
        )
        .unwrap();
    let frame = f.render_frame();
    let end = renderer
        .readback(renderer.prepare(&frame).unwrap())
        .unwrap();
    near(end[20 * 640 + 210], [1., 0., 0., 1.]);
    assert_eq!(renderer.usage().targets, 0);
}

#[test]
fn continuous_control_projection_preserves_widgets_and_layout() {
    let mut f = Fixture::new(None);
    let range = reel_ui::widgets::SliderRange::new(0., 1., None).unwrap();
    let id = f
        .stage
        .create(
            NodeKind::Slider {
                range,
                value: 0.,
                listening: true,
            },
            NodeProps {
                width: Some(220.),
                height: Some(40.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let check = f
        .stage
        .create(
            NodeKind::Toggle {
                label: Some("Checkbox".into()),
                value: false,
                listening: false,
                appearance: reel_ui::stage::ToggleAppearance::Checkbox,
            },
            NodeProps {
                y: 60.,
                width: Some(220.),
                height: Some(40.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let radio = f
        .stage
        .create(
            NodeKind::Radio {
                label: Some("Radio".into()),
                selected: false,
                revision: 0,
            },
            NodeProps {
                y: 110.,
                width: Some(220.),
                height: Some(40.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let root = f
        .stage
        .create(
            NodeKind::Stack,
            NodeProps::default(),
            vec![id, check, radio],
        )
        .unwrap();
    f.stage.replace(root).unwrap();
    let source = f.record();
    f.accept(&source);
    let baseline = f.layouts();
    let started = std::time::Instant::now();
    for i in 1..=2_000 {
        let props = f.stage.node(id).unwrap().props.clone();
        f.stage
            .update(
                id,
                NodeKind::Slider {
                    range,
                    value: f64::from(i % 100) / 100.,
                    listening: true,
                },
                props,
            )
            .unwrap();
        for selected in [check, radio] {
            let node = f.stage.node(selected).unwrap();
            let mut kind = node.kind.clone();
            match &mut kind {
                NodeKind::Toggle { value, .. } => *value = i % 2 == 0,
                NodeKind::Radio { selected, .. } => *selected = i % 2 == 0,
                _ => unreachable!(),
            }
            f.stage.update(selected, kind, node.props.clone()).unwrap();
        }
        let source = f.record();
        f.accept(&source);
    }
    assert_eq!(
        f.layouts(),
        baseline,
        "value changes must not rebuild or relayout controls"
    );
    eprintln!(
        "2000 slider/checkbox/radio frames with CPU recording/presentation: {:?}; added layouts=0",
        started.elapsed()
    );
}

#[test]
#[ignore = "requires REEL_SESSION_FONT with Latin glyphs for real wrapping geometry"]
fn text_wrap_changes_height_preserves_newlines_and_reuses_the_label() {
    use reel_ui::imaging::record::{Command, Draw};

    let font = reel_ui::peniko::Blob::from(
        std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
    );
    let mut f = Fixture::new(Some(font));
    f.edit(f.root, |props| {
        *props = NodeProps {
            width: Some(600.),
            height: Some(400.),
            ..Default::default()
        };
    });
    let text: reel_core::text::Text =
        "A deliberately long authored sentence that spans several widths.\nSecond explicit line."
            .into();
    f.stage
        .update(
            f.blue,
            NodeKind::Text(text.clone()),
            NodeProps {
                width: Some(120.),
                font_size: 24.,
                text_wrap: false,
                ..Default::default()
            },
        )
        .unwrap();
    let glyphs = |source: &RecordedSource| {
        let scene = source.scene();
        scene
            .commands()
            .iter()
            .filter_map(|command| match command {
                Command::Draw(id) => match scene.draw_op(*id) {
                    Draw::GlyphRun(run) => Some(run),
                    _ => None,
                },
                _ => None,
            })
            .flat_map(|run| run.glyphs.iter().copied())
            .collect::<Vec<_>>()
    };
    let label_identity = |f: &Fixture| {
        let scene = f.bridge.root().get_widget(f.layouts()[&f.blue].0).unwrap();
        let scene = scene.downcast::<SceneWidget>().unwrap();
        let id = scene.label.as_ref().unwrap().id();
        let label = f.bridge.root().get_widget(id).unwrap();
        (id, label.downcast::<Label>().unwrap().text().clone())
    };
    let unwrapped = f.record();
    f.accept(&unwrapped);
    let (label_id, plain) = label_identity(&f);
    let widget_id = f.layouts()[&f.blue].0;
    let height = |source: &RecordedSource| source.geometry().get(widget_id).unwrap().size.height;
    let original_height = height(&unwrapped);
    let original_glyphs = glyphs(&unwrapped);
    assert!(!original_glyphs.is_empty());
    assert!(original_glyphs.iter().all(|glyph| glyph.id != 0));
    assert_eq!(
        original_glyphs
            .iter()
            .map(|glyph| glyph.y.to_bits())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2,
        "only the explicit newline breaks text"
    );
    assert!(
        original_glyphs.iter().any(|glyph| glyph.x > 120.),
        "overflow keeps the complete authored line"
    );
    drop(unwrapped);

    f.edit(f.blue, |props| props.text_wrap = true);
    let wrapped = f.record();
    f.accept(&wrapped);
    assert!(
        height(&wrapped) > original_height * 2.,
        "automatic wrapping changes natural height"
    );
    assert_eq!(label_identity(&f).0, label_id);
    assert!(Arc::ptr_eq(&label_identity(&f).1, &plain));
    drop(wrapped);

    f.edit(f.blue, |props| props.text_wrap = false);
    let restored = f.record();
    f.accept(&restored);
    assert_eq!(height(&restored), original_height);
    assert_eq!(glyphs(&restored), original_glyphs);
    drop(restored);
    f.edit(f.blue, |props| props.text_outline = 2.);
    let outlined = f.record();
    f.accept(&outlined);
    assert_eq!(
        height(&outlined),
        original_height,
        "outline changes ink bounds only"
    );
    drop(outlined);
    let layouts = f.layouts();
    f.edit(f.blue, |props| props.reveal = Some(0));
    let hidden = f.record();
    f.accept(&hidden);
    assert!(glyphs(&hidden).is_empty());
    assert_eq!(f.layouts(), layouts, "reveal must not relayout");
    drop(hidden);
    f.edit(f.blue, |props| {
        props.reveal = Some(text.len());
        props.text_outline_color = [255, 0, 255, 255];
    });
    let revealed = f.record();
    f.accept(&revealed);
    assert!(!glyphs(&revealed).is_empty());
    assert_eq!(f.layouts(), layouts);
    drop(revealed);
    f.edit(f.blue, |props| props.text_outline = 0.);
    let filled = f.record();
    f.accept(&filled);
    assert_eq!(glyphs(&filled), original_glyphs);
    assert_eq!(height(&filled), original_height);
    drop(filled);

    f.edit(f.blue, |props| {
        props.text_fit = reel_ui::core::TextFit::Shrink { min_font_size: 0. }
    });
    let fitted = f.record();
    f.accept(&fitted);
    let fitted_height = height(&fitted);
    let fitted_glyphs = glyphs(&fitted);
    drop(fitted);
    f.edit(f.blue, |props| props.text_wrap = true);
    let fitted_wrapped = f.record();
    f.accept(&fitted_wrapped);
    assert!(fitted_height < original_height);
    assert_eq!(fitted_height, height(&fitted_wrapped));
    assert_eq!(
        fitted_glyphs,
        glyphs(&fitted_wrapped),
        "shrink is independent of automatic wrapping"
    );
    assert_eq!(label_identity(&f).0, label_id);
    assert!(Arc::ptr_eq(&label_identity(&f).1, &plain));
}

#[test]
fn text_wrap_keeps_control_labels_on_their_existing_overflow_path() {
    let mut f = Fixture::new(None);
    f.stage
        .update(
            f.blue,
            NodeKind::Button {
                label: Some("A control label keeps its existing layout".into()),
                listening: true,
            },
            NodeProps {
                width: Some(120.),
                ..Default::default()
            },
        )
        .unwrap();
    let original = f.record();
    f.accept(&original);
    let layouts = f.layouts();
    f.edit(f.blue, |props| props.text_wrap = false);
    let changed = f.record();
    f.accept(&changed);
    assert_eq!(f.layouts(), layouts);
}

#[test]
fn dialogue_reveal_keeps_text_identity_and_layout() {
    let mut f = Fixture::new(None);
    let first = f.record();
    f.accept(&first);
    let original = f.layouts();
    let NodeKind::Text(text) = &f.stage.node(f.blue).unwrap().kind else {
        unreachable!()
    };
    let text = text.clone();
    for end in 0..=text.len() {
        f.edit(f.blue, |props| props.reveal = Some(end));
        let frame = f.record();
        f.accept(&frame);
        assert_eq!(f.layouts(), original, "reveal must not cause layout");
        let NodeKind::Text(current) = &f.stage.node(f.blue).unwrap().kind else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(text.plain(), current.plain()));
    }
}

#[test]
fn scroll_retains_layouts_reading_position_and_end_following() {
    use reel_ui::dpi::PhysicalPosition;
    let mut f = Fixture::new(None);
    f.edit(f.root, |p| {
        *p = NodeProps {
            width: Some(200.),
            height: Some(120.),
            ..Default::default()
        }
    });
    f.edit(f.red, |p| {
        *p = NodeProps {
            width: Some(600.),
            height: Some(200.),
            ..Default::default()
        }
    });
    f.edit(f.blue, |p| {
        *p = NodeProps {
            width: Some(500.),
            height: Some(200.),
            ..Default::default()
        }
    });
    let configure = |f: &mut Fixture, axis| {
        let props = f.stage.node(f.root).unwrap().props.clone();
        f.stage
            .update(
                f.root,
                NodeKind::Scroll {
                    axis,
                    follow_end: true,
                    align: reel_ui::widgets::ScrollAlign::Start,
                    offset: Vec2::ZERO,
                },
                props,
            )
            .unwrap();
        let source = f.record();
        f.accept(&source);
        drop(source);
    };
    configure(&mut f, ScrollAxis::Vertical);
    let position = |f: &Fixture| {
        let id = f.layouts()[&f.root].0;
        f.bridge
            .root()
            .get_widget(id)
            .unwrap()
            .downcast::<SceneWidget>()
            .unwrap()
            .scroll
            .offset()
    };
    let wheel = |f: &mut Fixture, dy| {
        f.ui.handle_pointer(
            &mut f.bridge,
            PointerEvent::Scroll(PointerScrollEvent {
                pointer: PointerInfo {
                    pointer_id: Some(PointerId::PRIMARY),
                    persistent_device_id: None,
                    pointer_type: PointerType::Mouse,
                },
                state: PointerState {
                    position: PhysicalPosition::new(50., 50.),
                    ..Default::default()
                },
                delta: ScrollDelta::PixelDelta(PhysicalPosition::new(0., dy)),
            }),
        )
        .unwrap();
    };
    assert_eq!(position(&f).y, 280.);
    wheel(&mut f, -80.);
    let source = f.record();
    f.accept(&source);
    drop(source);
    f.edit(f.blue, |p| p.height = Some(300.));
    let source = f.record();
    f.accept(&source);
    drop(source);
    assert_eq!(
        position(&f).y,
        200.,
        "appending does not steal the reading position"
    );
    wheel(&mut f, 10_000.);
    let source = f.record();
    f.accept(&source);
    drop(source);
    f.edit(f.blue, |p| p.height = Some(400.));
    let source = f.record();
    f.accept(&source);
    drop(source);
    assert_eq!(
        position(&f).y,
        480.,
        "a reader at the end follows new content"
    );
    f.edit(f.blue, |p| p.height = Some(50.));
    let source = f.record();
    f.accept(&source);
    drop(source);
    assert_eq!(position(&f).y, 130.);
    let layouts = f.layouts();
    let started = std::time::Instant::now();
    for i in 0..2_000 {
        wheel(&mut f, if i % 2 == 0 { -1. } else { 1. });
    }
    let elapsed = started.elapsed();
    let source = f.record();
    f.accept(&source);
    drop(source);
    assert_eq!(
        layouts,
        f.layouts(),
        "scrolling must preserve widgets and every layout count"
    );
    eprintln!("2000 scrolling input/compose updates: {elapsed:?}; no widget replacement or layout");
    configure(&mut f, ScrollAxis::Both);
    assert_eq!(position(&f), Vec2::new(400., 130.));
    configure(&mut f, ScrollAxis::Horizontal);
    assert_eq!(position(&f), Vec2::new(900., 0.));
    f.edit(f.root, |p| p.height = Some(500.));
    let props = f.stage.node(f.root).unwrap().props.clone();
    f.stage
        .update(
            f.root,
            NodeKind::Scroll {
                axis: ScrollAxis::Vertical,
                follow_end: true,
                align: reel_ui::widgets::ScrollAlign::End,
                offset: Vec2::ZERO,
            },
            props,
        )
        .unwrap();
    let source = f.record();
    f.accept(&source);
    let red_id = f.layouts()[&f.red].0;
    assert_eq!(
        source
            .geometry()
            .get(red_id)
            .unwrap()
            .window_transform
            .translation()
            .y,
        250.,
        "short content aligns to the viewport bottom"
    );
}

#[test]
#[ignore = "requires REEL_SESSION_FONT for wrapped text measurement"]
fn authored_text_width_controls_scroll_extent_independently_of_parent_width() {
    let font = std::fs::read(std::env::var_os("REEL_SESSION_FONT").unwrap()).unwrap();
    let mut f = Fixture::new(Some(font.into()));
    f.edit(f.red, |p| p.visible = false);
    f.edit(f.blue, |p| {
        *p = NodeProps {
            width: Some(110.),
            font_size: 24.,
            ..Default::default()
        };
    });
    let props = f.stage.node(f.blue).unwrap().props.clone();
    f.stage
        .update(
            f.blue,
            NodeKind::Text(
                "A wrapped chat message stays completely visible. "
                    .repeat(10)
                    .into(),
            ),
            props,
        )
        .unwrap();
    let props = NodeProps {
        width: Some(200.),
        height: Some(90.),
        ..Default::default()
    };
    f.stage
        .update(
            f.root,
            NodeKind::Scroll {
                axis: ScrollAxis::Vertical,
                follow_end: true,
                align: reel_ui::widgets::ScrollAlign::End,
                offset: Vec2::ZERO,
            },
            props,
        )
        .unwrap();
    let mut measured = None;
    for capped in [false, true] {
        f.edit(f.blue, |p| {
            p.width = if capped { None } else { Some(110.) };
            p.max_width = capped.then_some(110.);
        });
        for width in [200., 500., 140.] {
            f.edit(f.root, |p| p.width = Some(width));
            let source = f.record();
            f.accept(&source);
            let id = f.layouts()[&f.root].0;
            let root = f
                .bridge
                .root()
                .get_widget(id)
                .unwrap()
                .downcast::<SceneWidget>()
                .unwrap();
            let height = root.scroll.content().height;
            assert!(height > 90., "the paragraph must overflow the viewport");
            assert_eq!(
                height,
                *measured.get_or_insert(height),
                "explicit text width determines every wrapped line"
            );
            assert_eq!(
                root.scroll.offset().y,
                height - 90.,
                "end following includes the complete last line"
            );
        }
    }
}

#[test]
fn camera_accepted_frame_pointer_clip_and_composition_cost() {
    use reel_ui::core::{
        PointerButton, PointerButtonEvent, PointerEvent, PointerId, PointerInfo, PointerState,
        PointerType,
    };
    use reel_ui::dpi::PhysicalPosition;
    use reel_ui::stage::{CameraView, InputBindings, NodeEvent, PointerPhase};
    fn pointer(down: bool, x: f64, y: f64) -> PointerEvent {
        let event = PointerButtonEvent {
            pointer: PointerInfo {
                pointer_id: Some(PointerId::PRIMARY),
                persistent_device_id: None,
                pointer_type: PointerType::Mouse,
            },
            state: PointerState {
                position: PhysicalPosition::new(x, y),
                ..Default::default()
            },
            button: Some(PointerButton::Primary),
        };
        if down {
            PointerEvent::Down(event)
        } else {
            PointerEvent::Up(event)
        }
    }
    let font = std::env::var_os("REEL_SESSION_FONT")
        .map(|path| reel_ui::peniko::Blob::new(Arc::new(std::fs::read(path).unwrap())));
    eprintln!("camera actual font supplied={}", font.is_some());
    let mut f = Fixture::new(font);
    f.stage
        .update(
            f.root,
            NodeKind::Camera(CameraView {
                x: 40.,
                y: 30.,
                zoom: 2.,
                rotation: 90.,
            }),
            NodeProps {
                x: 100.,
                y: 80.,
                width: Some(200.),
                height: Some(160.),
                padding: 10.,
                clip: true,
                ..Default::default()
            },
        )
        .unwrap();
    f.stage
        .update(
            f.red,
            NodeKind::Input {
                bindings: InputBindings::new(vec![]).unwrap(),
                listening: false,
                keyboard: false,
                pointer: true,
            },
            NodeProps {
                x: 30.,
                y: 20.,
                width: Some(300.),
                height: Some(200.),
                ..Default::default()
            },
        )
        .unwrap();
    f.edit(f.blue, |p| {
        p.x = 500.;
    });
    let first = f.record();
    f.accept(&first);
    let before = f.layouts();
    let root = before[&f.root].0;
    let red = before[&f.red].0;
    affine(
        first.geometry().get(red).unwrap().window_transform,
        [0., -2., 2., 0., 180., 180.],
    );
    assert_eq!(
        first.geometry().get(root).unwrap().clip_path,
        Some(Rect::new(0., 0., 200., 160.))
    );
    let press = |f: &mut Fixture, x, y| {
        let events =
            f.ui.handle_pointer(&mut f.bridge, pointer(true, x, y))
                .unwrap()
                .events;
        f.ui.handle_pointer(&mut f.bridge, pointer(false, x, y))
            .unwrap();
        events.into_iter().find_map(|event| match event {
            NodeEvent::Pointer(event) if event.phase == PointerPhase::Pressed => Some(event),
            _ => None,
        })
    };
    let event = press(&mut f, 200., 160.).expect("camera input");
    assert_eq!(event.node, f.red);
    assert!((event.position - Point::new(10., 10.)).hypot() < 1e-9);
    assert!(
        press(&mut f, 200., -40.).is_none(),
        "viewport clip excludes transformed overflow"
    );
    let props = f.stage.node(f.root).unwrap().props.clone();
    f.stage
        .update(
            f.root,
            NodeKind::Camera(CameraView {
                x: 60.,
                y: 30.,
                zoom: 2.,
                rotation: 90.,
            }),
            props,
        )
        .unwrap();
    let pending = f.record();
    let event = press(&mut f, 200., 160.).expect("old accepted camera input");
    assert!(
        (event.position - Point::new(10., 10.)).hypot() < 1e-9,
        "unaccepted camera motion cannot alter input coordinates"
    );
    f.accept(&pending);
    let event = press(&mut f, 200., 160.).expect("new accepted camera input");
    assert!((event.position - Point::new(30., 10.)).hypot() < 1e-9);
    assert_eq!(f.layouts(), before);
    drop(pending);
    let started = std::time::Instant::now();
    for step in 0..1000 {
        let props = f.stage.node(f.root).unwrap().props.clone();
        f.stage
            .update(
                f.root,
                NodeKind::Camera(CameraView {
                    x: f64::from(step) * 0.1,
                    y: 30.,
                    zoom: 1. + f64::from(step) * 0.001,
                    rotation: f64::from(step) * 0.1,
                }),
                props,
            )
            .unwrap();
        let source = f.record();
        f.accept(&source);
    }
    assert_eq!(
        f.layouts(),
        before,
        "camera motion preserves all identities and layout counts"
    );
    affine(
        first.geometry().get(red).unwrap().window_transform,
        [0., -2., 2., 0., 180., 180.],
    );
    eprintln!(
        "1000 camera projection/record/accept frames: {:?}; added layouts=0",
        started.elapsed()
    );
    // Nested cameras compose through the same child affine, with the outer
    // camera magnifying the inner viewport's own layout placement.
    let leaf = f
        .stage
        .create(
            NodeKind::Stack,
            NodeProps {
                x: 10.,
                y: 5.,
                width: Some(20.),
                height: Some(10.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let inner = f
        .stage
        .create(
            NodeKind::Camera(CameraView {
                x: 10.,
                y: 5.,
                zoom: 3.,
                rotation: 90.,
            }),
            NodeProps {
                x: 40.,
                y: 20.,
                width: Some(80.),
                height: Some(60.),
                clip: true,
                ..Default::default()
            },
            vec![leaf],
        )
        .unwrap();
    let outer = f
        .stage
        .create(
            NodeKind::Camera(CameraView {
                zoom: 2.,
                ..Default::default()
            }),
            NodeProps {
                x: 20.,
                y: 30.,
                width: Some(200.),
                height: Some(100.),
                clip: true,
                ..Default::default()
            },
            vec![inner],
        )
        .unwrap();
    f.stage.replace(outer).unwrap();
    let nested = f.record();
    f.accept(&nested);
    let leaf_widget = f.layouts()[&leaf].0;
    affine(
        nested.geometry().get(leaf_widget).unwrap().window_transform,
        [0., -6., 6., 0., 280., 180.],
    );
}

#[test]
fn same_parent_reorder_preserves_widgets_focus_and_presented_hit_order() {
    use reel_ui::core::{
        PointerButton, PointerButtonEvent, PointerEvent, PointerId, PointerInfo, PointerState,
        PointerType,
    };
    use reel_ui::dpi::PhysicalPosition;
    use reel_ui::stage::NodeEvent;
    let font = std::env::var_os("REEL_SESSION_FONT")
        .map(|path| reel_ui::peniko::Blob::new(Arc::new(std::fs::read(path).unwrap())));
    let mut f = Fixture::new(font);
    f.edit(f.root, |props| {
        *props = NodeProps {
            width: Some(180.),
            height: Some(80.),
            ..Default::default()
        };
    });
    for (id, label) in [(f.red, "Red"), (f.blue, "Blue")] {
        f.stage
            .update(
                id,
                NodeKind::Button {
                    label: Some(label.into()),
                    listening: true,
                },
                NodeProps {
                    width: Some(160.),
                    height: Some(60.),
                    ..Default::default()
                },
            )
            .unwrap();
    }
    let click = |f: &mut Fixture| {
        let mut activated = Vec::new();
        for down in [true, false] {
            let event = PointerButtonEvent {
                pointer: PointerInfo {
                    pointer_id: Some(PointerId::PRIMARY),
                    persistent_device_id: None,
                    pointer_type: PointerType::Mouse,
                },
                state: PointerState {
                    position: PhysicalPosition::new(30., 30.),
                    ..Default::default()
                },
                button: Some(PointerButton::Primary),
            };
            let event = if down {
                PointerEvent::Down(event)
            } else {
                PointerEvent::Up(event)
            };
            for event in f.ui.handle_pointer(&mut f.bridge, event).unwrap().events {
                if let NodeEvent::Click(event) = event {
                    activated.push(event.node);
                }
            }
        }
        activated
    };
    let first = f.record();
    f.accept(&first);
    let before = f.layouts();
    assert_eq!(click(&mut f), [f.blue]);
    assert_eq!(f.bridge.root().focused_widget(), Some(before[&f.blue].0));
    let retired = f
        .stage
        .reconcile_children(f.root, vec![f.blue, f.red])
        .unwrap();
    assert!(retired.is_empty());
    let pending = f.record();
    assert_eq!(
        click(&mut f),
        [f.blue],
        "input still uses the accepted child order"
    );
    f.accept(&pending);
    assert_eq!(f.bridge.root().focused_widget(), Some(before[&f.blue].0));
    let after = f.layouts();
    assert_eq!(after[&f.root].0, before[&f.root].0);
    for id in [f.red, f.blue] {
        assert_eq!(
            after[&id], before[&id],
            "reordering retains identity and text layout"
        );
    }
    assert_eq!(
        click(&mut f),
        [f.red],
        "new presentation changes the topmost target"
    );
}

#[test]
fn mixed_prepared_background_keeps_particle_sprite_and_rejects_tiled_sprite() {
    use crate::assets::{AssetOptions, Assets};
    use reel_core::prepared_image::PreparedImage;
    use reel_ui::stage::particles::{ParticleBatch, ParticleConfig};
    let mut stage = Stage::default();
    let particle = stage
        .create(
            NodeKind::Particles(ParticleBatch {
                config: Arc::new(ParticleConfig {
                    source: "assets/panels.png".into(),
                    origin: Some("snow.aside:7:3".into()),
                    rate: 4.,
                    burst: 0,
                    lifetime: 2.,
                    lifetime_spread: 0.,
                    seed: 42,
                    velocity: [0., 20.],
                    spread: [0., 0.],
                    acceleration: [0., 0.],
                    size: 8.,
                    size_spread: 0.,
                    size_end: None,
                    opacity: [1., 0.],
                    spin: 0.,
                    spin_spread: 0.,
                }),
                frame: None,
            }),
            NodeProps::default(),
            vec![],
        )
        .unwrap();
    let background = stage
        .create(
            NodeKind::Image {
                source: "background.reel-image".into(),
                origin: Some("snow.aside:2:3".into()),
                fit: Default::default(),
                slice: None,
                slice_scale: 1.,
                repeat: [false; 2],
                tile_scale: 1.,
                corner_colors: None,
                slice_center: true,
            },
            NodeProps::default(),
            vec![],
        )
        .unwrap();
    let root = stage
        .create(
            NodeKind::Stack,
            NodeProps::default(),
            vec![background, particle],
        )
        .unwrap();
    stage.replace(root).unwrap();
    let mut view = StageView::capture(&stage);
    let mut images = crate::images::Images::new(Assets::directory(AssetOptions {
        root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
        ..Default::default()
    }));
    let (send, receive) = std::sync::mpsc::channel();
    images.set_waker(Arc::new(move || {
        let _ = send.send(());
    }));
    images.manifests.insert(
        "background.reel-image".into(),
        PreparedImage::new(4096, 2048, 512).unwrap(),
    );
    images.request_stage(&view);
    while !images.poll().unwrap() {
        receive
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
    }
    view.resolve_metadata(&images);
    let root = view.root.as_ref().unwrap();
    assert_eq!(
        root.children[0].images.tiled_size,
        Some(Size::new(4096., 2048.))
    );
    let sprite = root.children[1].images.image.as_ref().unwrap();
    assert_eq!([sprite.width, sprite.height], [160, 90]);
    assert_eq!(view.resources.len(), 1);
    assert_eq!(
        view.resources[0].handle(),
        images.loaded["assets/panels.png"].handle()
    );
    let widget = SceneWidget::new(root, &mut BTreeMap::new());
    assert!(widget.image.is_none());
    assert_eq!(
        images.loads, 1,
        "metadata path reuses the one decoded sprite"
    );

    let root = Arc::make_mut(view.root.as_mut().unwrap());
    let child = Arc::make_mut(&mut Arc::make_mut(&mut root.children)[1]);
    let NodeKind::Particles(batch) = &mut Arc::make_mut(&mut child.node).kind else {
        unreachable!()
    };
    Arc::make_mut(&mut batch.config).source = "background.reel-image".into();
    images.request_stage(&view);
    let error = images.poll().unwrap_err();
    assert!(error.contains("snow.aside:7:3"), "{error}");
    assert!(error.contains("whole PNG or JPEG sprite"), "{error}");
    assert_eq!(images.loads, 1, "unsupported request is rejected before IO");
}

#[test]
fn decoded_frames_preserve_geometry_skip_asset_io_and_release_pixels_with_last_view() {
    use reel_core::{
        budget::{BudgetCategory, BudgetPool, BudgetTicket, QueueLimits},
        identity::{AssetKey, ContentVersion, ResourceVersion},
        image::{Yuv420Image, Yuv420Storage, YuvColor, YuvMatrix, YuvTransfer},
    };
    use reel_ui::{
        frame::{BuildLimits, FrameBuild, FrameDraw, FrameInfo, FrameOp, StepBudget},
        peniko::Brush,
        resources::{ResourceRegistry, ResourceSnapshot},
        stage::ImageSource,
    };
    #[derive(Debug)]
    struct Pixels {
        planes: [Vec<u8>; 3],
        _ticket: BudgetTicket,
    }
    impl Yuv420Storage for Pixels {
        fn plane(&self, i: usize) -> &[u8] {
            &self.planes[i]
        }
    }
    let pixels = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 4,
            max_bytes: 24,
        },
    );
    let frames = BudgetPool::new(
        BudgetCategory::Snapshot,
        QueueLimits {
            max_requests: 100,
            max_bytes: 32 << 20,
        },
    );
    let mut registry = ResourceRegistry::new(4, 64 << 10).unwrap();
    let kind = |source| NodeKind::Image {
        source,
        origin: None,
        fit: Default::default(),
        slice: None,
        slice_scale: 1.,
        slice_center: true,
        repeat: [false; 2],
        tile_scale: 1.,
        corner_colors: None,
    };
    let mut f = Fixture::new(None);
    let props = f.stage.node(f.red).unwrap().props.clone();
    let blue = f.stage.node(f.blue).unwrap().props.clone();
    f.stage.update(f.blue, NodeKind::Stack, blue).unwrap();
    f.stage
        .update(f.red, kind(ImageSource::empty()), props.clone())
        .unwrap();
    let source = f.record();
    assert!(!f.ui.images_pending());
    assert!(
        StageView::capture(&f.stage)
            .image_requests()
            .unwrap()
            .required
            .is_empty()
    );
    f.accept(&source);
    drop(source);
    let mut first = None;
    let mut stable_layouts = None;
    let mut current = None;
    for index in 0..20 {
        let storage = Arc::new(Pixels {
            planes: [vec![16 + index; 4], vec![128], vec![128]],
            _ticket: pixels.reserve(6).unwrap(),
        });
        let pointer = storage.planes[0].as_ptr();
        let image = Yuv420Image::new(
            [2, 2],
            YuvColor {
                matrix: YuvMatrix::Bt709,
                full_range: false,
                transfer: YuvTransfer::Bt709,
            },
            storage,
        )
        .unwrap();
        let handle = registry
            .register_yuv420(
                ResourceVersion {
                    key: AssetKey("video-frame".into()),
                    version: ContentVersion([index; 32]),
                },
                image,
            )
            .unwrap();
        let lease = registry.acquire(handle).unwrap();
        f.stage
            .update(f.red, kind(ImageSource::resource(lease)), props.clone())
            .unwrap();
        assert!(
            StageView::capture(&f.stage)
                .image_requests()
                .unwrap()
                .required
                .is_empty()
        );
        // Releasing producer registration before stage capture must be legal.
        assert!(registry.release(handle));
        assert!(registry.acquire(handle).is_err());
        let source = f.record();
        f.accept(&source);
        assert!(!f.ui.images_pending());
        let snapshot =
            ResourceSnapshot::capture(f.ui.image_resources().iter().cloned().map(Ok), &frames)
                .unwrap();
        let ResourceData::Yuv420(image) = snapshot.leases()[0].data() else {
            panic!("planar image expected")
        };
        assert_eq!(image.plane(0).as_ptr(), pointer);
        let mut build = FrameBuild::new(
            source.source,
            snapshot,
            FrameInfo {
                logical_size: [640., 420.],
                physical_size: [640, 420],
            },
            BuildLimits { max_bytes: 8 << 20 },
            &frames,
        )
        .unwrap();
        while !build
            .step(StepBudget {
                max_commands: 1,
                max_units: 1024,
                max_bytes: 64 << 10,
            })
            .unwrap()
            .complete
        {}
        let frame = build.finish().unwrap();
        assert!(
            frame
                .commands()
                .iter()
                .any(|c| matches!(&c.op, FrameOp::PushClip(_)))
        );
        let image = frame
            .commands()
            .iter()
            .find_map(|c| {
                if let FrameOp::Draw(FrameDraw::Fill {
                    brush: Brush::Image(image),
                    transform,
                    ..
                }) = &c.op
                {
                    Some((image, *transform))
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(image.0.image.resource, handle);
        assert!(
            image.1.as_coeffs()[1] != 0.,
            "parent rotation must survive image recording"
        );
        if let Some(before) = &stable_layouts {
            assert_eq!(
                &f.layouts(),
                before,
                "same size frame changes must not relayout"
            );
        } else {
            stable_layouts = Some(f.layouts());
        }
        if first.is_none() {
            first = Some(frame);
        } else {
            current = Some(frame);
        }
        assert!(
            pixels.usage().bytes <= 12,
            "only first retained frame plus current frame own pixels"
        );
    }
    let ResourceData::Yuv420(image) = first.as_ref().unwrap().resources().leases()[0].data() else {
        panic!()
    };
    assert_eq!(
        image.plane(0),
        [16; 4],
        "retained frame must remain unchanged"
    );
    drop(current);
    drop(first);
    drop(f);
    drop(registry);
    assert_eq!(pixels.usage().bytes, 0);
}

#[test]
fn subtree_invert_updates_cached_composition_without_relayout() {
    let mut f = Fixture::new(None);
    let first = f.record();
    f.accept(&first);
    let layouts = f.layouts();
    f.edit(f.root, |p| {
        p.invert = 0.75;
        p.opacity = 0.5;
    });
    let inverted = f.record();
    let groups: Vec<_> = inverted
        .plan()
        .layers
        .iter()
        .flat_map(|l| &l.groups)
        .filter(|g| g.invert != 0.)
        .collect();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].invert, 0.75);
    assert_eq!(
        f.layouts(),
        layouts,
        "pixel effects must keep layout and widget identity"
    );
    f.accept(&inverted);
    let cached = f.record();
    assert_eq!(
        cached
            .plan()
            .layers
            .iter()
            .flat_map(|l| &l.groups)
            .filter(|g| g.invert == 0.75)
            .count(),
        1
    );
    f.accept(&cached);
    f.edit(f.root, |p| {
        p.invert = 0.;
        p.opacity = 1.;
    });
    let cleared = f.record();
    assert!(
        cleared
            .plan()
            .layers
            .iter()
            .flat_map(|l| &l.groups)
            .all(|g| g.invert == 0.)
    );
    assert_eq!(f.layouts(), layouts);
}

mod virtual_scroll_cost;

struct TestFrame {
    source: RecordedSource,
    receipt: crate::session::RecordedProjection,
}
impl std::ops::Deref for TestFrame {
    type Target = RecordedSource;
    fn deref(&self) -> &Self::Target {
        &self.source
    }
}

mod natural_height;
