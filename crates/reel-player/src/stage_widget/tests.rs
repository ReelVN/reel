use super::*;
mod animation_cost;
mod cursor;
mod flow;
mod input_focus;
mod prepared_textures;
mod radio_navigation;
mod retained;
mod scroll_alignment;
mod scroll_bounce;
mod scroll_restore;
mod styles;
mod virtual_images;
mod wheel;
use crate::session::SessionUi;
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
                focus_on_pointer: true,
                focusable: true,
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
                overscroll: Default::default(),
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

impl Fixture {
    fn new(font: Option<reel_ui::peniko::Blob<u8>>) -> Self {
        Self::with_canvas(font, None)
    }
    fn with_canvas(font: Option<reel_ui::peniko::Blob<u8>>, canvas: Option<[u32; 2]>) -> Self {
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
        let widget = if let Some(size) = canvas {
            reel_ui::core::NewWidget::new(reel_ui::widgets::Canvas::new(widget, size)).erased()
        } else {
            widget.erased()
        };
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
    fn paint_work(&self) -> [usize; 3] {
        fn walk(widget: WidgetRef<'_, dyn Widget>, work: &mut [usize; 3]) {
            if let Some(scene) = widget.downcast::<SceneWidget>() {
                for (total, count) in work.iter_mut().zip(scene.paint_work) {
                    *total += count;
                }
            }
            for child in widget.children() {
                walk(child, work);
            }
        }
        let mut work = [0; 3];
        walk(
            self.bridge.root().get_widget(self.widget).unwrap(),
            &mut work,
        );
        work
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
    f.stage
        .update(f.root, NodeKind::Column(Default::default()), props)
        .unwrap();
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
    f.stage
        .update(f.root, NodeKind::Column(Default::default()), props)
        .unwrap();
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
                overscroll: Default::default(),
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
    assert_eq!(scene.scroll.content(), Size::new(60., 97.));
    assert_eq!(
        scrolled.geometry().get(ids[&f.blue].0).unwrap().size.width,
        60.
    );
    assert_eq!(
        scrolled.geometry().get(ids[&tail].0).unwrap().size.width,
        40.
    );
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
                group: None,
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
                    overscroll: Default::default(),
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
                overscroll: Default::default(),
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
                    focus_on_pointer: true,
                    focusable: true,
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
    use reel_ui::stage::particles::ParticleNode;
    let mut stage = Stage::default();
    let particle = stage
        .create(
            NodeKind::Particles(ParticleNode {
                source: "assets/panels.png".into(),
                origin: Some("snow.aside:7:3".into()),
                seed: 42,
                size_scale: 1.,
                time: 0.,
                batches: Default::default(),
            }),
            NodeProps::default(),
            vec![],
        )
        .unwrap();
    let background = stage
        .create(
            NodeKind::Image {
                sampling: Default::default(),
                source_rect: None,
                source: "background.reel-image".into(),
                origin: Some("snow.aside:2:3".into()),
                fit: Default::default(),
                slice: None,
                slice_scale: 1.,
                repeat: [false; 2],
                tile_scale: 1.,
                corner_colors: None,
                alpha_layout: Default::default(),
                adjustment: Default::default(),
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
        PreparedImage::new(
            4096,
            2048,
            512,
            reel_core::prepared_image::TileEncoding::Png,
        )
        .unwrap(),
    );
    images.request_stage(&view);
    while !images.poll().unwrap() {
        receive
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
    }
    view.resolve_metadata(&images).unwrap();
    let root = view.root.as_ref().unwrap();
    assert_eq!(
        root.children[0].images.tiled_size,
        Some(Size::new(4096., 2048.))
    );
    let sprite = root.children[1].images.image_resource.as_ref().unwrap();
    assert_eq!(sprite.image_size(), Some([160, 90]));
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

    let original_kind = stage.node(background).unwrap().kind.clone();
    let original_props = stage.node(background).unwrap().props.clone();
    let mut region = original_kind.clone();
    if let NodeKind::Image { source_rect, .. } = &mut region {
        *source_rect = Some(Rect::new(0., 0., 64., 64.));
    }
    stage
        .update(background, region, original_props.clone())
        .unwrap();
    view = StageView::update(&stage, Some(&view));
    assert!(
        view.image_requests(EffectShaderFormat::Wgsl).is_ok(),
        "prepared atlas regions share image geometry"
    );
    stage
        .update(background, original_kind.clone(), original_props.clone())
        .unwrap();
    view = StageView::update(&stage, Some(&view));

    let manifest = images.manifests["background.reel-image"];
    images.manifests.insert("atlas.png".into(), manifest);
    for (index, expected) in ["alpha_layout", "nine-slice", "repetition", "masks"]
        .into_iter()
        .enumerate()
    {
        let mut kind = original_kind.clone();
        let mut props = original_props.clone();
        let NodeKind::Image {
            source,
            alpha_layout,
            slice,
            repeat,
            ..
        } = &mut kind
        else {
            unreachable!()
        };
        *source = "atlas.png".into();
        match index {
            0 => *alpha_layout = reel_ui::frame::ImageAlphaLayout::TopBottom,
            1 => *slice = Some(Insets::uniform(1.)),
            2 => *repeat = [true; 2],
            _ => {
                props.mask = Some(reel_ui::stage::MaskProgram::image("atlas.png".into()));
                props.appearance_origin = Some("snow.aside:11:5".into());
            }
        }
        stage.update(background, kind, props).unwrap();
        let mut invalid = StageView::update(&stage, Some(&view));
        assert!(
            invalid.image_requests(EffectShaderFormat::Wgsl).is_ok(),
            "filename extensions do not classify images"
        );
        images.request_stage(&invalid);
        assert!(images.poll().unwrap());
        let error = invalid.resolve_metadata(&images).unwrap_err();
        assert!(
            error.contains(expected) && error.contains("snow.aside:"),
            "{error}"
        );
    }

    stage
        .update(background, original_kind, original_props)
        .unwrap();
    let mut kind = stage.node(particle).unwrap().kind.clone();
    let NodeKind::Particles(batch) = &mut kind else {
        unreachable!()
    };
    batch.source = "atlas.png".into();
    let props = stage.node(particle).unwrap().props.clone();
    stage.update(particle, kind, props).unwrap();
    view = StageView::update(&stage, Some(&view));
    images
        .manifests
        .insert("background.reel-image".into(), manifest);
    images.manifests.insert("atlas.png".into(), manifest);
    images.request_stage(&view);
    assert!(images.poll().unwrap());
    let error = view.resolve_metadata(&images).unwrap_err();
    assert!(error.contains("snow.aside:7:3"), "{error}");
    assert!(
        error.contains("whole PNG, JPEG or .reel-texture sprite"),
        "{error}"
    );
    assert_eq!(
        images.loads, 1,
        "known unsupported manifests are rejected before tile IO"
    );
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
    let kind = |source, source_rect| NodeKind::Image {
        sampling: Default::default(),
        source_rect,
        source,
        origin: None,
        fit: Default::default(),
        slice: None,
        slice_scale: 1.,
        slice_center: true,
        repeat: [false; 2],
        tile_scale: 1.,
        corner_colors: None,
        alpha_layout: Default::default(),
        adjustment: Default::default(),
    };
    let mut f = Fixture::new(None);
    f.stage
        .update(
            f.root,
            NodeKind::Button {
                label: None,
                listening: true,
                focus_on_pointer: true,
                focusable: true,
            },
            f.stage.node(f.root).unwrap().props.clone(),
        )
        .unwrap();
    let props = f.stage.node(f.red).unwrap().props.clone();
    let blue = f.stage.node(f.blue).unwrap().props.clone();
    f.stage.update(f.blue, NodeKind::Stack, blue).unwrap();
    f.stage
        .update(f.red, kind(ImageSource::empty(), None), props.clone())
        .unwrap();
    let source = f.record();
    assert!(!f.ui.images_pending());
    assert!(
        StageView::capture(&f.stage)
            .image_requests(EffectShaderFormat::Wgsl)
            .unwrap()
            .required()
            .next()
            .is_none()
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
            .update(
                f.red,
                kind(
                    ImageSource::resource(lease),
                    Some(Rect::new(
                        f64::from(index % 2),
                        0.,
                        f64::from(index % 2) + 1.,
                        1.,
                    )),
                ),
                props.clone(),
            )
            .unwrap();
        assert!(
            StageView::capture(&f.stage)
                .image_requests(EffectShaderFormat::Wgsl)
                .unwrap()
                .required()
                .next()
                .is_none()
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
        let reel_ui::resources::ResourceView::Yuv420(image) = snapshot.leases()[0].view() else {
            panic!("planar image expected")
        };
        assert_eq!(image.plane(0).as_ptr(), pointer);
        let root_geometry = source.geometry().get(f.layouts()[&f.root].0).unwrap();
        let button_center = root_geometry.window_transform * root_geometry.size.to_rect().center();
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
                    brush_transform,
                    ..
                }) = &c.op
                {
                    Some((image, *transform, *brush_transform))
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(image.0.image.resource, handle);
        assert_eq!(
            image.2,
            Some(Affine::translate((f64::from(index % 2), 0.))),
            "source pixels retain their original 1:1 mapping"
        );
        assert_eq!(
            image.0.image.source_rect,
            Some(Rect::new(
                f64::from(index % 2),
                0.,
                f64::from(index % 2) + 1.,
                1.
            ))
        );
        assert_eq!(
            f.ui.image_resources().len(),
            1,
            "selecting pixels preserves one YUV resource"
        );
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
        if index == 0 {
            use reel_ui::core::{
                PointerButton, PointerButtonEvent, PointerEvent, PointerId, PointerInfo,
                PointerState, PointerType,
            };
            let pointer = |down| {
                let event = PointerButtonEvent {
                    pointer: PointerInfo {
                        pointer_id: Some(PointerId::PRIMARY),
                        persistent_device_id: None,
                        pointer_type: PointerType::Mouse,
                    },
                    state: PointerState {
                        position: reel_ui::dpi::PhysicalPosition::new(
                            button_center.x,
                            button_center.y,
                        ),
                        ..Default::default()
                    },
                    button: Some(PointerButton::Primary),
                };
                if down {
                    PointerEvent::Down(event)
                } else {
                    PointerEvent::Up(event)
                }
            };
            f.ui.handle_pointer(&mut f.bridge, pointer(true)).unwrap();
            let before = f.layouts();
            let mut kind = f.stage.node(f.red).unwrap().kind.clone();
            let NodeKind::Image { source_rect, .. } = &mut kind else {
                unreachable!()
            };
            *source_rect = Some(Rect::new(1., 0., 2., 1.));
            f.stage.update(f.red, kind, props.clone()).unwrap();
            let selection = f.record();
            f.accept(&selection);
            let clicks =
                f.ui.handle_pointer(&mut f.bridge, pointer(false))
                    .unwrap()
                    .events;
            assert!(
                clicks
                    .into_iter()
                    .any(|event| matches!(event,NodeEvent::Click(click) if click.node==f.root)),
                "source window animation preserves the button's accepted pointer gesture"
            );
            assert_eq!(
                f.layouts(),
                before,
                "moving the window only invalidates paint"
            );
            assert_eq!(
                f.ui.image_resources()[0].handle(),
                handle,
                "a frame selection retains the original YUV lease"
            );
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
    let reel_ui::resources::ResourceView::Yuv420(image) =
        first.as_ref().unwrap().resources().leases()[0].view()
    else {
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

mod font_weight;
mod natural_height;
mod text_spacing;

mod text_layout;
mod virtual_list;

mod composition;
