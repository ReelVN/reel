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
        lifetime: 5.,
        seed: 42,
        velocity: [-20., 60.],
        spread: [30., 20.],
        acceleration: [10., -12.],
        size: 24.,
        size_spread: 8.,
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
    view.root.as_mut().unwrap().image = Some(ImageData {
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
    let node = &mut view.root.as_mut().unwrap().node;
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
    view.root.as_mut().unwrap().node.kind = NodeKind::Particles(ParticleBatch {
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
        let node = &mut view.root.as_mut().unwrap().node;
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
                    lifetime: 2.,
                    seed: 42,
                    velocity: [0., 20.],
                    spread: [0., 0.],
                    acceleration: [0., 0.],
                    size: 8.,
                    size_spread: 0.,
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
    assert_eq!(root.children[0].tiled_size, Some(Size::new(4096., 2048.)));
    let sprite = root.children[1].image.as_ref().unwrap();
    assert_eq!([sprite.width, sprite.height], [160, 90]);
    assert_eq!(view.resources.len(), 1);
    assert_eq!(
        view.resources[0].handle(),
        images.loaded["assets/panels.png"].handle()
    );
    let widget = SceneWidget::new(root);
    assert!(widget.image.is_none());
    assert_eq!(
        images.loads, 1,
        "metadata path reuses the one decoded sprite"
    );

    let NodeKind::Particles(batch) = &mut view.root.as_mut().unwrap().children[1].node.kind else {
        unreachable!()
    };
    Arc::make_mut(&mut batch.config).source = "background.reel-image".into();
    images.request_stage(&view);
    let error = images.poll().unwrap_err();
    assert!(error.contains("snow.aside:7:3"), "{error}");
    assert!(error.contains("whole PNG or JPEG sprite"), "{error}");
    assert_eq!(images.loads, 1, "unsupported request is rejected before IO");
}
