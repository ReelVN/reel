use super::*;
use crate::{
    aside_session::{AsideSessionUi, StoryProgress, StoryState},
    assets::AssetOptions,
};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    identity::{FrameId, SurfaceEpoch},
};
use reel_engine::aside::{SliceBudget, VmLimits};
use reel_ui::{
    bridge::{NoticeResult, PresentationNotice, PresentationOutcome},
    frame::{BuildLimits, FrameBuild, FrameDraw, FrameInfo, FrameOp, RenderFrame, StepBudget},
    peniko::Brush,
    resources::ResourceSnapshot,
    widgets::Image,
};
use std::{
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    time::Duration,
};

const STORY: &str = r#"
fn main() {
    let picture = image("assets/panels.png");
    stage.replace(stack() {
        picture;
        image("assets/panels.png", x: 180, width: 80);
        image("assets/panels.png", x: 280, height: 45);
        image("assets/panels.png", y: 110, width: 100, height: 100, fit: "contain");
        image("assets/panels.png", x: 120, y: 110, width: 100, height: 100, fit: "cover", opacity: 0.5);
        image("assets/panels.png", x: 240, y: 110, width: 100, height: 100, fit: "stretch");
    });
    narrator("A");
    picture.source = "assets/sun.jpg";
    picture.width = 80;
    narrator("B");
}
"#;

fn fixture() -> (AsideSessionUi, PresentationBridge, WidgetId, Receiver<()>) {
    fixture_with_assets(crate::assets::Assets::directory(AssetOptions {
        root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
        ..Default::default()
    }))
}
fn fixture_with_assets(
    assets: crate::assets::Assets,
) -> (AsideSessionUi, PresentationBridge, WidgetId, Receiver<()>) {
    fixture_with_source(STORY, assets)
}
fn fixture_with_source(
    source: &str,
    assets: crate::assets::Assets,
) -> (AsideSessionUi, PresentationBridge, WidgetId, Receiver<()>) {
    let mut driver = AsideSessionUi::from_source(
        super::tests::session(),
        source,
        "main",
        1,
        "main",
        "en",
        VmLimits::default(),
    )
    .unwrap()
    .with_assets(assets);
    let (send, receive) = mpsc::channel();
    driver.set_resource_waker(Arc::new(move || {
        let _ = send.send(());
    }));
    let (bridge, root) = super::tests::mount(driver.ui());
    (driver, bridge, root, receive)
}
fn drive(driver: &mut AsideSessionUi) {
    for _ in 0..10000 {
        match driver.tick(SliceBudget::default(), 256, || 0).unwrap() {
            StoryProgress::Interaction(_) | StoryProgress::Finished => return,
            StoryProgress::Faulted(_) => panic!("{:?}", driver.fault().unwrap()),
            StoryProgress::HostFailed(_) => panic!("{:?}", driver.host_failure()),
            _ => {}
        }
    }
    panic!("story failed to yield")
}
fn settle(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    wake: &Receiver<()>,
) {
    driver.sync(bridge, root).unwrap();
    while driver.ui().images_pending() {
        wake.recv_timeout(Duration::from_secs(10))
            .expect("background completion must wake host");
        driver.sync(bridge, root).unwrap();
    }
}
fn frame(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    id: u64,
) -> RenderFrame {
    frame_with_info(
        driver,
        bridge,
        root,
        id,
        FrameInfo {
            logical_size: [640., 420.],
            physical_size: [640, 420],
        },
    )
}
fn frame_with_info(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    id: u64,
    info: FrameInfo,
) -> RenderFrame {
    frame_with_receipt(driver, bridge, root, id, info).0
}
fn frame_with_receipt(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    id: u64,
    info: FrameInfo,
) -> (RenderFrame, RecordedProjection) {
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 32,
            max_bytes: 32 << 20,
        },
    );
    let (source, receipt) = driver
        .record(bridge, root, FrameId(id))
        .unwrap()
        .expect("settled frame");
    let key = source.key();

    let resources =
        ResourceSnapshot::capture(driver.ui().image_resources().iter().cloned().map(Ok), &pool)
            .unwrap();
    let mut build = FrameBuild::new(
        source,
        resources,
        info,
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
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            key,
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(id))
        )),
        NoticeResult::Accepted
    );
    driver.presented(bridge, &receipt).unwrap();
    (frame, receipt)
}
fn image_ids(bridge: &PresentationBridge, root: WidgetId) -> Vec<WidgetId> {
    fn walk(widget: WidgetRef<'_, dyn Widget>, ids: &mut Vec<WidgetId>) {
        if widget.downcast::<Image>().is_some() {
            ids.push(widget.id());
        }
        for child in widget.children() {
            walk(child, ids);
        }
    }
    let mut ids = Vec::new();
    walk(bridge.root().get_widget(root).unwrap(), &mut ids);
    ids
}
fn advance(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    id: &mut u64,
) {
    for _ in 0..2 {
        let widget = bridge
            .root()
            .get_widget(root)
            .unwrap()
            .downcast::<SessionWidget>()
            .unwrap();
        let point = widget.dialogue_rect.center() - Vec2::new(0., widget.scroll.get());
        for kind in ["move", "down", "up"] {
            driver
                .handle_pointer(bridge, super::tests::mouse(kind, (point.x, point.y)))
                .unwrap();
        }
        driver.sync(bridge, root).unwrap();
        *id += 1;
        frame(driver, bridge, root, *id);
    }
}
fn image_transforms(frame: &RenderFrame) -> Vec<[f64; 6]> {
    frame
        .commands()
        .iter()
        .filter_map(|command| {
            if let FrameOp::Draw(FrameDraw::Fill {
                transform,
                brush: Brush::Image(_),
                ..
            }) = &command.op
            {
                Some(transform.as_coeffs())
            } else {
                None
            }
        })
        .collect()
}
fn image_pixel_ids(frame: &RenderFrame) -> Vec<u64> {
    frame
        .commands()
        .iter()
        .filter_map(|command| {
            if let FrameOp::Draw(FrameDraw::Fill {
                brush: Brush::Image(image),
                ..
            }) = &command.op
            {
                let lease = frame
                    .resources()
                    .leases()
                    .iter()
                    .find(|lease| lease.handle() == image.image.resource)
                    .expect("image handle must resolve in this frame's snapshot");
                let reel_ui::resources::ResourceData::Image { image, .. } = lease.data() else {
                    panic!("image handle resolved to another resource kind")
                };
                Some(image.data.id())
            } else {
                None
            }
        })
        .collect()
}
#[test]
fn author_image_pipeline_preserves_atomic_scene_identity_and_final_frame() {
    image_pipeline(fixture());
    image_pipeline(fixture_with_assets(crate::assets::tests::memory_assets()));
}

#[test]
fn fixed_rasters_wait_for_camera_entry_and_preserve_natural_size_and_accepted_frame() {
    let source = r#"
fn main() {
    let view = camera(width: 300, height: 150, camera_x: 150, camera_y: 75) {
        image("assets/panels.png", x: 2000);
        image("assets/sun.jpg", x: 1000, width: 96, height: 96);
        image("missing-offscreen.png", x: 4000, width: 96, height: 96);
    };
    stage.replace(view);
    narrator("A");
    view.camera_x = 1048;
    view.camera_rotation = 15;
    narrator("B");
}
"#;
    let (mut driver, mut bridge, root, wake) =
        fixture_with_source(source, crate::assets::tests::memory_assets());
    drive(&mut driver);
    settle(&mut driver, &mut bridge, root, &wake);
    assert_eq!(driver.ui().images.loads, 1);
    assert_eq!(driver.ui().images.pool.usage().bytes, 160 * 90 * 4);
    assert!(driver.ui().images.is_required("assets/panels.png"));
    assert!(!driver.ui().images.is_required("assets/sun.jpg"));
    assert!(!driver.ui().images.is_required("missing-offscreen.png"));
    let mut id = 1;
    let first = frame(&mut driver, &mut bridge, root, id);
    let retained = image_pixel_ids(&first);
    advance(&mut driver, &mut bridge, root, &mut id);
    drive(&mut driver);
    driver.sync(&mut bridge, root).unwrap();
    assert!(driver.ui().images_pending());
    assert_eq!(driver.ui().images.loads, 2);
    assert!(
        driver
            .record(&mut bridge, root, FrameId(id + 1))
            .unwrap()
            .is_none()
    );
    assert_eq!(image_pixel_ids(&first), retained);
    settle(&mut driver, &mut bridge, root, &wake);
    let _ = frame(&mut driver, &mut bridge, root, id + 1);
    assert!(driver.ui().images.is_required("assets/sun.jpg"));
    assert!(!driver.ui().images.is_required("missing-offscreen.png"));
    for _ in 0..4 {
        driver.sync(&mut bridge, root).unwrap();
    }
    assert_eq!(driver.ui().images.loads, 2);
}

#[test]
fn native_scroll_loads_a_newly_exposed_raster_before_accepting_the_view() {
    use reel_ui::{
        core::{
            PointerId, PointerInfo, PointerScrollEvent, PointerState, PointerType, ScrollDelta,
        },
        dpi::PhysicalPosition,
    };
    let source = r#"
fn main() {
    stage.replace(scroll(width: 300, height: 150) {
        stack(width: 300, height: 1400) {
            image("assets/panels.png", width: 160, height: 90);
            image("assets/sun.jpg", y: 1000, width: 96, height: 96);
        };
    });
    narrator("A");
}
"#;
    let (mut driver, mut bridge, root, wake) =
        fixture_with_source(source, crate::assets::tests::memory_assets());
    drive(&mut driver);
    settle(&mut driver, &mut bridge, root, &wake);
    let _first = frame(&mut driver, &mut bridge, root, 1);
    assert_eq!(driver.ui().images.loads, 1);
    driver
        .handle_pointer(
            &mut bridge,
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
                delta: ScrollDelta::PixelDelta(PhysicalPosition::new(0., 1000.)),
            }),
        )
        .unwrap();
    driver.sync(&mut bridge, root).unwrap();
    assert!(driver.ui().images_pending());
    assert!(
        driver
            .record(&mut bridge, root, FrameId(2))
            .unwrap()
            .is_none()
    );
    settle(&mut driver, &mut bridge, root, &wake);
    assert_eq!(driver.ui().images.loads, 2);
    assert!(driver.ui().images.is_required("assets/sun.jpg"));
    assert!(!driver.ui().images.is_required("assets/panels.png"));
    let _ = frame(&mut driver, &mut bridge, root, 2);
}

#[test]
fn effect_shader_admission_reuses_io_and_leases_for_parameter_changes() {
    use reel_ui::{
        resources::{EffectShaderDescriptor, EffectShaderFormat, ResourceData},
        stage::{NodeKind, NodeProps, Stage, effect::*},
    };
    let program = EffectProgram::new(
        vec![EffectParameter {
            name: "coverage".into(),
            ty: EffectType::Float,
            default: None,
        }],
        vec![EffectNode {
            ty: EffectType::Float,
            op: EffectOp::Parameter(0),
        }],
        EffectNodeId(0),
    )
    .unwrap();
    let definition = [17; 32];
    let format = EffectShaderFormat::Wgsl;
    let source = format.asset_path(definition);
    let raw = b"@fragment fn main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }";
    let descriptor = EffectShaderDescriptor::new(
        definition,
        format,
        vec![EffectType::Float],
        EffectType::Float,
        raw,
    )
    .unwrap();
    let directory = std::env::temp_dir().join(format!(
        "reel-effect-admission-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = directory.join(&source);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, descriptor.encode_payload(raw).unwrap()).unwrap();
    let mut stage = Stage::default();
    let effect = |value: f32| {
        EffectInstance::new(
            definition,
            program.clone(),
            vec![EffectValue::Float(value.to_bits())],
        )
        .unwrap()
    };
    let node = stage
        .create(
            NodeKind::Stack,
            NodeProps {
                width: Some(100.),
                height: Some(100.),
                effect: Some(effect(0.5)),
                clip: true,
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    stage.replace(node).unwrap();
    let mut ui = SessionUi::new(super::tests::session())
        .unwrap()
        .with_asset_options(AssetOptions {
            root: directory.clone(),
            ..Default::default()
        });
    let (send, wake) = mpsc::channel();
    ui.set_resource_waker(Arc::new(move || {
        let _ = send.send(());
    }));
    ui.set_stage(&stage);
    let (mut bridge, root) = super::tests::mount(&ui);
    ui.sync(&mut bridge, root).unwrap();
    assert!(ui.images_block_presentation());
    while ui.images_pending() {
        wake.recv_timeout(Duration::from_secs(10)).unwrap();
        ui.sync(&mut bridge, root).unwrap();
    }
    assert_eq!(ui.images.loads, 1);
    let lease = &ui.image_resources()[0];
    let handle = lease.handle();
    assert!(matches!(lease.data(), ResourceData::EffectShader { .. }));
    let retained = bridge.record(FrameId(1)).unwrap();
    let layer = retained
        .plan()
        .layers
        .iter()
        .find(|layer| layer.groups.iter().any(|group| group.effect.is_some()))
        .unwrap();
    let group = layer
        .groups
        .iter()
        .find(|group| group.effect.is_some())
        .unwrap();
    let reel_ui::app::VisualLayerKind::Scene(scene) = &layer.kind else {
        panic!()
    };
    assert!(group.command > 0);
    assert!(
        matches!(
            scene.commands()[group.command - 1],
            reel_ui::imaging::record::Command::PushClip(_)
        ),
        "the node's clip must wrap the effect result, not cut its source"
    );
    let mut props = stage.node(node).unwrap().props.clone();
    props.effect = Some(effect(0.75));
    stage.update(node, NodeKind::Stack, props).unwrap();
    ui.set_stage(&stage);
    ui.sync(&mut bridge, root).unwrap();
    assert!(!ui.images_pending());
    assert_eq!(ui.images.loads, 1);
    assert_eq!(ui.image_resources()[0].handle(), handle);
    drop(retained);
    drop(bridge);
    drop(ui);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
fn image_pipeline(
    (mut driver, mut bridge, root, wake): (
        AsideSessionUi,
        PresentationBridge,
        WidgetId,
        Receiver<()>,
    ),
) {
    drive(&mut driver);
    settle(&mut driver, &mut bridge, root, &wake);
    let mut id = 1;
    let (first, first_receipt) = frame_with_receipt(
        &mut driver,
        &mut bridge,
        root,
        id,
        FrameInfo {
            logical_size: [640., 420.],
            physical_size: [640, 420],
        },
    );
    let ids = image_ids(&bridge, root);
    assert_eq!(ids.len(), 6);
    assert_eq!(driver.ui().images.loads, 1);
    assert_eq!(driver.ui().image_resources().len(), 1);
    let resident = driver.ui().images.pool.clone();
    assert_eq!(resident.usage().bytes, 160 * 90 * 4);
    let pixel_ids = image_pixel_ids(&first);
    assert_eq!(pixel_ids.len(), 6);
    assert!(
        pixel_ids.iter().all(|id| *id == pixel_ids[0]),
        "every image node must share the decoded blob"
    );
    let transforms = image_transforms(&first);
    let expected = [
        [1., 0., 0., 1., 0., 0.],
        [0.5, 0., 0., 0.5, 180., 0.],
        [0.5, 0., 0., 0.5, 280., 0.],
        [0.625, 0., 0., 0.625, 0., 131.875],
        [
            100. / 90.,
            0.,
            0.,
            100. / 90.,
            120. - (160. * 100. / 90. - 100.) / 2.,
            110.,
        ],
        [0.625, 0., 0., 100. / 90., 240., 110.],
    ];
    assert_eq!(transforms.len(), expected.len());
    for (actual, expected) in transforms.iter().zip(expected) {
        for (actual, expected) in actual.iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-9, "actual={transforms:?}");
        }
    }
    eprintln!("image transforms={transforms:?}");
    assert!(first.commands().iter().any(
        |command| matches!(&command.op, FrameOp::PushGroup(group) if group.composite.alpha == 0.5)
    ));
    assert!(
        first
            .commands()
            .iter()
            .filter(|command| matches!(&command.op, FrameOp::PushClip(_)))
            .count()
            >= 6
    );
    advance(&mut driver, &mut bridge, root, &mut id);
    drive(&mut driver);
    driver.sync(&mut bridge, root).unwrap();
    assert!(driver.ui().images_pending());
    assert_eq!(image_ids(&bridge, root), ids);
    assert_eq!(driver.ui().image_resources().len(), 1);
    assert!(
        driver
            .record(&mut bridge, root, FrameId(id + 1))
            .unwrap()
            .is_none(),
        "pending replacement retains the submitted frame"
    );
    assert_eq!(image_transforms(&first), transforms);
    assert_eq!(image_pixel_ids(&first), pixel_ids);
    driver.presented(&bridge, &first_receipt).unwrap();
    assert!(
        driver.ui().binding.borrow().accepted.is_none(),
        "presenting the retained scene must not re-enable story input"
    );
    settle(&mut driver, &mut bridge, root, &wake);
    assert_eq!(image_ids(&bridge, root), ids);
    assert_eq!(driver.ui().images.loads, 2);
    assert_eq!(driver.ui().image_resources().len(), 2);
    assert_eq!(resident.usage().bytes, (160 * 90 + 96 * 96) * 4);
    id += 2;
    let changed = frame(&mut driver, &mut bridge, root, id);
    assert_ne!(image_transforms(&changed), transforms);
    let changed_pixels = image_pixel_ids(&changed);
    assert_ne!(changed_pixels[0], pixel_ids[0]);
    assert_eq!(&changed_pixels[1..], &pixel_ids[1..]);
    assert_eq!(
        image_transforms(&changed)[0],
        [80. / 96., 0., 0., 80. / 96., 0., 0.]
    );
    advance(&mut driver, &mut bridge, root, &mut id);
    drive(&mut driver);
    assert_eq!(driver.state(), StoryState::Finished);
    driver.sync(&mut bridge, root).unwrap();
    let final_frame = frame(&mut driver, &mut bridge, root, id + 1);
    assert_eq!(image_ids(&bridge, root), ids);
    assert_eq!(image_transforms(&changed), image_transforms(&final_frame));
    assert_eq!(image_pixel_ids(&final_frame), changed_pixels);
    for _ in 0..4 {
        driver.sync(&mut bridge, root).unwrap();
    }
    assert_eq!(
        driver.ui().images.loads,
        2,
        "idle synchronization must not start more decodes"
    );
    assert!(!driver.ui().images_pending());
    #[cfg(feature = "window")]
    assert!(!driver.ui().images_need_poll());
    eprintln!(
        "decodes={}, shared nodes={}, resident={:?}",
        driver.ui().images.loads,
        ids.len(),
        resident.usage()
    );
    driver.cancel().unwrap();
    assert!(
        final_frame
            .resources()
            .leases()
            .iter()
            .all(|lease| !lease.is_retired())
    );
    driver.sync(&mut bridge, root).unwrap();
    assert!(driver.ui().image_resources().is_empty());
    assert!(image_ids(&bridge, root).is_empty());
    let closed_frame = frame(&mut driver, &mut bridge, root, id + 2);
    assert!(image_transforms(&closed_frame).is_empty());
    assert_eq!(driver.state(), StoryState::Finished);
    drop(driver);
    drop(bridge);
    drop(first);
    drop(changed);
    drop(final_frame);
    assert_eq!(resident.usage().bytes, 0);
    eprintln!(
        "after close and accepted-frame release: resident={:?}",
        resident.usage()
    );
}

#[test]
fn cancelled_story_sync_clears_published_images_and_keeps_only_submitted_leases() {
    for replacement_pending in [false, true] {
        let (mut driver, mut bridge, root, wake) = fixture();
        drive(&mut driver);
        settle(&mut driver, &mut bridge, root, &wake);
        let mut id = 1;
        let submitted = frame(&mut driver, &mut bridge, root, id);
        let resident = driver.ui().images.pool.clone();
        if replacement_pending {
            advance(&mut driver, &mut bridge, root, &mut id);
            drive(&mut driver);
            driver.sync(&mut bridge, root).unwrap();
            assert!(driver.ui().images_pending());
        }
        driver.cancel().unwrap();
        driver.sync(&mut bridge, root).unwrap();
        assert_eq!(driver.state(), StoryState::Cancelled);
        assert!(!driver.ui().images_pending());
        assert!(driver.ui().image_resources().is_empty());
        assert!(image_ids(&bridge, root).is_empty());
        let widget = bridge
            .root()
            .get_widget(root)
            .unwrap()
            .downcast::<SessionWidget>()
            .unwrap();
        assert_eq!(
            widget.projection.status,
            reel_engine::SessionStatus::Cancelled
        );
        assert!(widget.projection.lines.is_empty());
        assert!(widget.projection.choices.is_empty());
        assert!(
            widget.preview.is_none(),
            "cancellation must not inject diagnostic UI"
        );
        assert!(widget.lines.is_empty() && widget.choices.is_empty());
        let cancelled = frame(&mut driver, &mut bridge, root, id + 1);
        assert!(image_transforms(&cancelled).is_empty());
        assert!(
            submitted
                .resources()
                .leases()
                .iter()
                .all(|lease| !lease.is_retired())
        );
        assert!(resident.usage().bytes >= 160 * 90 * 4);
        drop(submitted);
        while resident.usage().bytes != 0 {
            wake.recv_timeout(Duration::from_secs(10))
                .expect("cancelled decoder must release its remaining storage");
        }
        // The driver and widget tree remain alive: zero usage proves their old
        // image aliases were released, rather than merely hiding pending state.
        assert_eq!(resident.usage().bytes, 0);
        driver.sync(&mut bridge, root).unwrap();
        assert!(!driver.ui().images_pending());
        assert!(driver.ui().image_resources().is_empty());
    }
}

#[cfg(feature = "window")]
#[test]
#[ignore = "requires actual graphics adapter; run explicitly"]
fn author_image_gpu_fit_alpha_and_clip_pixels() {
    use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer};
    let (mut driver, mut bridge, root, wake) = fixture();
    drive(&mut driver);
    settle(&mut driver, &mut bridge, root, &wake);
    let first_frame = frame(&mut driver, &mut bridge, root, 1);
    let instance = wgpu::Instance::default();
    let limits = GpuLimits::default();
    let gpu =
        pollster::block_on(Gpu::request(&instance, None, DeviceGeneration(1), limits)).unwrap();
    eprintln!("adapter={:?}", gpu.adapter.get_info());
    let mut renderer = Renderer::new(gpu);
    let prepared = renderer.prepare(&first_frame).unwrap();
    eprintln!(
        "frame commands={}, groups={}, clips={}, prepared={:?}",
        first_frame.commands().len(),
        first_frame
            .commands()
            .iter()
            .filter(|command| matches!(command.op, FrameOp::PushGroup(_)))
            .count(),
        first_frame
            .commands()
            .iter()
            .filter(|command| matches!(command.op, FrameOp::PushClip(_)))
            .count(),
        renderer.usage()
    );
    let pixels = renderer.readback(prepared).unwrap();
    fn linear(byte: u8) -> f32 {
        let c = f32::from(byte) / 255.;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    let red = [linear(224), linear(64), linear(48), 1.];
    let alpha = 128. / 255.;
    let blue = [
        linear(32) * alpha,
        linear(112) * alpha,
        linear(224) * alpha,
        alpha,
    ];
    // SessionWidget paints its opaque background beneath the author stage.
    let background = [linear(35), linear(54), linear(74), 1.];
    let over_background = |pixel: [f32; 4]| -> [f32; 4] {
        std::array::from_fn(|channel| pixel[channel] + background[channel] * (1. - pixel[3]))
    };
    for (x, y, expected) in [
        (20, 20, red),
        (120, 20, blue),
        (170, 20, [0.; 4]),
        (190, 20, red),
        (250, 20, blue),
        (190, 55, [0.; 4]),
        (290, 20, red),
        (350, 20, blue),
        (290, 55, [0.; 4]),
        (20, 130, [0.; 4]),
        (20, 150, red),
        (80, 150, blue),
        (20, 195, [0.; 4]),
        (110, 150, [0.; 4]),
        (140, 150, red.map(|c| c * 0.5)),
        (200, 150, blue.map(|c| c * 0.5)),
        (230, 150, [0.; 4]),
        (140, 215, [0.; 4]),
        (260, 130, red),
        (320, 130, blue),
        (350, 150, [0.; 4]),
    ] {
        let expected = over_background(expected);
        let actual = pixels[y * 640 + x];
        for channel in 0..4 {
            assert!(
                (actual[channel] - expected[channel]).abs() < 0.006,
                "({x},{y}) actual={actual:?} expected={expected:?}"
            );
        }
    }
    assert_eq!(
        renderer.usage().targets,
        0,
        "readback completion releases all targets"
    );
    // Exercise the same author's source edit through the live widget projection,
    // not a renderer-only replacement of the PNG brush.
    let mut id = 1;
    advance(&mut driver, &mut bridge, root, &mut id);
    drive(&mut driver);
    settle(&mut driver, &mut bridge, root, &wake);
    let changed_frame = frame(&mut driver, &mut bridge, root, id + 1);
    let changed_pixels = renderer
        .readback(renderer.prepare(&changed_frame).unwrap())
        .unwrap();
    let jpeg_background = [linear(20), linear(48), linear(88), 1.];
    let jpeg_center = [linear(255), linear(200), linear(48), 1.];
    for (x, y, expected, tolerance) in [
        (5, 5, jpeg_background, 0.025),
        (40, 40, jpeg_center, 0.025),
        (20, 85, background, 0.006),
        (120, 20, background, 0.006),
        (190, 20, red, 0.006),
        (250, 20, over_background(blue), 0.006),
    ] {
        let actual = changed_pixels[y * 640 + x];
        for channel in 0..4 {
            assert!(
                (actual[channel] - expected[channel]).abs() < tolerance,
                "changed ({x},{y}) actual={actual:?} expected={expected:?}"
            );
        }
    }
    assert_eq!(driver.ui().images.loads, 2);
    assert_eq!(renderer.usage().targets, 0);
    let window_frame = frame_with_info(
        &mut driver,
        &mut bridge,
        root,
        id + 2,
        FrameInfo {
            logical_size: [640., 420.],
            physical_size: [960, 540],
        },
    );
    let prepared = renderer
        .prepare(&window_frame)
        .expect("the default window extent must fit default GPU budgets");
    eprintln!("default window 960x540 prepared={:?}", renderer.usage());
    let window_pixels = renderer.readback(prepared).unwrap();
    assert_eq!(window_pixels.len(), 960 * 540);
    assert_eq!(renderer.usage().targets, 0);
}
