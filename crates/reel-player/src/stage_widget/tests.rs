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
    fn record(&mut self) -> RecordedSource {
        self.ui.set_stage(&self.stage);
        self.ui.sync(&mut self.bridge, self.widget).unwrap();
        self.frame += 1;
        self.bridge.record(FrameId(self.frame)).unwrap()
    }
    fn accept(&mut self, source: &RecordedSource) {
        assert_eq!(
            self.bridge.resolve(PresentationNotice::new(
                source.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(source.key().frame_id())
            )),
            NoticeResult::Accepted
        );
        self.ui.presented(self.ui.projection_stamp());
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
            source,
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
        mixed,
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
    assert_eq!(renderer.usage().targets, 0);
}
