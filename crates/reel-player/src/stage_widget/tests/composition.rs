//! Scene transforms, compositing and camera geometry.
use super::*;

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
            NodeKind::Row(Default::default()),
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
fn particle_bounds_follow_each_captured_extent_after_resize_and_append() {
    use reel_ui::{
        app::GeometryRecord,
        peniko::{Blob, ImageAlphaType, ImageFormat},
        stage::particles::{
            ParticleBatches, ParticleConfig, ParticleEmission, ParticleFrame, ParticleNode,
        },
    };
    let config = ParticleConfig {
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
        size_curve: None,
        angle: 0.,
        angle_spread: 0.,
        radial_speed: 0.,
        radial_spread: 0.,
        direction: 0.,
        direction_spread: 180.,
        damping: 0.,
        opacity: [1., 1.],
        spin: 90.,
        spin_spread: 40.,
    };
    let mut frame = ParticleFrame {
        origin: [0., 0.],
        elapsed: 1.,
        duration: None,
        extent: [1000., 700.],
    };
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 32,
            max_bytes: 1 << 20,
        },
    );
    let make_node = |frames: &[ParticleFrame]| ParticleNode {
        source: "sprite.png".into(),
        origin: None,
        seed: 42,
        size_scale: 1.,
        time: 0.,
        batches: ParticleBatches::try_new(
            &pool,
            frames.len(),
            frames
                .iter()
                .enumerate()
                .map(|(i, frame)| ParticleEmission {
                    sequence: i as u64,
                    config: config.clone(),
                    frame: *frame,
                    started: 0.,
                    paused: false,
                }),
        )
        .unwrap(),
    };
    let mut stage = Stage::default();
    let id = stage
        .create(
            NodeKind::Particles(make_node(&[frame])),
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
    let mut registry = reel_ui::resources::ResourceRegistry::new(1, 65536).unwrap();
    let handle = registry
        .register(
            reel_core::identity::ResourceVersion {
                key: reel_core::identity::AssetKey("sprite.png".into()),
                version: reel_core::identity::ContentVersion([1; 32]),
            },
            ResourceData::Image {
                image: ImageData {
                    data: Blob::new(Arc::new(vec![255_u8; 2 * 4 * 4])),
                    format: ImageFormat::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width: 2,
                    height: 4,
                },
                encoding: reel_ui::resources::ColorEncoding::Srgb,
            },
            pool.reserve(32).unwrap(),
        )
        .unwrap();
    Arc::make_mut(view.root.as_mut().unwrap())
        .images
        .image_resource = Some(registry.acquire(handle).unwrap());
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
        let NodeKind::Particles(expected) = &view.root.as_ref().unwrap().node.kind else {
            unreachable!()
        };
        let batches: Vec<_> = source
            .plan()
            .layers
            .iter()
            .flat_map(|layer| layer.command_owners())
            .filter_map(|range| range.sprites.as_ref())
            .map(|batch| batch.as_ref().unwrap())
            .collect();
        assert_eq!(
            batches.len(),
            1,
            "one ordered sprite record per particle node"
        );
        let instances = batches[0].as_slice();
        assert_eq!(
            instances.len(),
            expected.samples().count(),
            "record includes every overlapping batch"
        );
        for (actual, expected) in instances.iter().zip(expected.samples()) {
            assert_eq!(actual.position, expected.position);
            assert_eq!(actual.size, expected.size);
            assert_eq!(actual.rotation, expected.rotation.to_radians());
            assert_eq!(actual.alpha, expected.opacity as f32);
        }
        let root = bridge.root().get_widget(widget).unwrap();
        let children = root.children();
        let particles = children[0].downcast::<SceneWidget>().unwrap();
        (
            source.geometry().get(particles.id()).unwrap().clone(),
            particles.layouts,
        )
    }
    fn assert_covers_sprites(geometry: &GeometryRecord, batch: &ParticleNode) {
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
    assert_covers_sprites(&small, &make_node(&[frame]));

    // An appended emission captures a different extent; both batches retain
    // their original birth rectangle, even after the node is resized.
    let first_frame = frame;
    frame.extent = [1400., 1000.];
    frame.origin = [-200., 300.];
    Arc::make_mut(&mut Arc::make_mut(view.root.as_mut().unwrap()).node).kind =
        NodeKind::Particles(make_node(&[first_frame, frame]));
    let (restarted, restarted_layouts) = record(&mut bridge, widget_id, &view, 3);
    assert!(restarted_layouts > resized_layouts);
    assert!(restarted.bounding_box.x1 > small.bounding_box.x1);
    assert!(restarted.bounding_box.y1 > small.bounding_box.y1);
    assert_covers_sprites(&restarted, &make_node(&[frame]));

    let node = Arc::make_mut(&mut Arc::make_mut(view.root.as_mut().unwrap()).node);
    let NodeKind::Particles(batch) = &mut node.kind else {
        unreachable!()
    };
    batch.size_scale = 20.;
    let enlarged = batch.clone();
    let (geometry, scaled_layouts) = record(&mut bridge, widget_id, &view, 4);
    assert!(scaled_layouts > restarted_layouts);
    assert_covers_sprites(&geometry, &enlarged);

    for step in 5..20 {
        let node = Arc::make_mut(&mut Arc::make_mut(view.root.as_mut().unwrap()).node);
        node.props.rotation = -17.;
        node.props.scale_x = 1.5;
        let NodeKind::Particles(batch) = &mut node.kind else {
            unreachable!()
        };
        batch.time += 0.1;
        let batch = batch.clone();
        let (geometry, layouts) = record(&mut bridge, widget_id, &view, step);
        assert_eq!(
            layouts, scaled_layouts,
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
                art: Default::default(),
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
                art: Default::default(),
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
                wheel: false,
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
    let hover = |f: &mut Fixture, x, y| {
        f.ui.handle_pointer(
            &mut f.bridge,
            cursor::pointer(PointerType::Mouse, (x, y), None),
        )
        .unwrap()
        .events
        .into_iter()
        .find_map(|event| match event {
            NodeEvent::Pointer(event) if event.phase == PointerPhase::Moved => Some(event),
            _ => None,
        })
    };
    let focused = f.bridge.root().focused_widget();
    let event = hover(&mut f, 200., 160.).expect("camera hover input");
    assert_eq!(event.node, f.red);
    assert!(!event.dragging);
    assert!((event.position - Point::new(10., 10.)).hypot() < 1e-9);
    assert_eq!(f.bridge.root().focused_widget(), focused);
    assert!(
        hover(&mut f, 200., -40.).is_none(),
        "hover respects the viewport clip"
    );
    assert!(
        !f.ui
            .handle_pointer(&mut f.bridge, pointer(false, 200., 160.))
            .unwrap()
            .events
            .iter()
            .any(|event| matches!(event,
                NodeEvent::Pointer(event) if event.phase == PointerPhase::Released)),
        "hover does not begin a drag"
    );
    let event = press(&mut f, 200., 160.).expect("camera input");
    assert_eq!(event.node, f.red);
    assert!(event.dragging);
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
    let event = hover(&mut f, 200., 160.).expect("old accepted camera hover");
    assert!((event.position - Point::new(10., 10.)).hypot() < 1e-9);
    let event = press(&mut f, 200., 160.).expect("old accepted camera input");
    assert!(
        (event.position - Point::new(10., 10.)).hypot() < 1e-9,
        "unaccepted camera motion cannot alter input coordinates"
    );
    f.accept(&pending);
    let event = hover(&mut f, 200., 160.).expect("new accepted camera hover");
    assert!((event.position - Point::new(30., 10.)).hypot() < 1e-9);
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
        .flat_map(|l| l.groups())
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
            .flat_map(|l| l.groups())
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
            .flat_map(|l| l.groups())
            .all(|g| g.invert == 0.)
    );
    assert_eq!(f.layouts(), layouts);
}
