use super::*;
use crate::assets::AssetOptions;
use reel_core::prepared_image::{PreparedImage, TileEncoding};
use reel_ui::{
    properties::ObjectFit,
    stage::{NodeKind, NodeProps, Stage},
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

#[cfg(feature = "window")]
#[path = "prepared_image_gpu_tests.rs"]
mod gpu_tests;

pub(super) fn fixture() -> (
    SessionUi,
    PresentationBridge,
    WidgetId,
    Stage,
    reel_ui::stage::NodeId,
    mpsc::Receiver<()>,
    PathBuf,
) {
    fixture_with_encoding(TileEncoding::Png)
}
fn fixture_with_encoding(
    encoding: TileEncoding,
) -> (
    SessionUi,
    PresentationBridge,
    WidgetId,
    Stage,
    reel_ui::stage::NodeId,
    mpsc::Receiver<()>,
    PathBuf,
) {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/reel-artifacts/prepared-image-runtime")
        .join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(&directory).unwrap();
    let source = if encoding == TileEncoding::ReelTexture {
        "long.png"
    } else {
        "long.reel-image"
    };
    let manifest = PreparedImage::new(40001, 33, 128, encoding).unwrap();
    fs::write(directory.join(source), manifest.encode()).unwrap();
    for level in 0..manifest.levels() {
        fs::create_dir_all(directory.join(format!("{source}.tiles/{level}"))).unwrap();
        let [width, height] = manifest.dimensions(level);
        for y in 0..height.div_ceil(manifest.tile_edge) {
            for x in 0..width.div_ceil(manifest.tile_edge) {
                let [w, h] = manifest.tile_dimensions(level, x, y).unwrap();
                let path = directory.join(manifest.tile_source(source, level, x, y));
                if encoding == TileEncoding::ReelTexture {
                    use reel_core::texture::*;
                    let descriptor = TextureDescriptor {
                        storage_layout: reel_core::texture::TextureStorageLayout::Linear,
                        mip_count: 1,
                        format: if x % 2 == 0 {
                            TextureFormat::Bc1Rgb
                        } else {
                            TextureFormat::Bc3Rgba
                        },
                        source_encoding: ColorEncoding::Srgb,
                        layout: TextureLayout {
                            logical_size: [w, h],
                            content_size: [w, h],
                            storage_size: [w.div_ceil(4) * 4, h.div_ceil(4) * 4],
                        },
                    };
                    let header =
                        TextureHeader::new(descriptor, descriptor.byte_len().unwrap()).unwrap();
                    let mut bytes = header.encode().to_vec();
                    bytes.resize(bytes.len() + descriptor.byte_len().unwrap() as usize, 0);
                    fs::write(path, bytes).unwrap();
                } else {
                    image::RgbaImage::from_fn(w, h, |tx, ty| {
                        image::Rgba([((x * 128 + tx) % 251) as u8, ty as u8, level as u8, 255])
                    })
                    .save(path)
                    .unwrap();
                }
            }
        }
    }
    let mut stage = Stage::default();
    let image = stage
        .create(
            NodeKind::Image {
                sampling: Default::default(),
                source: source.into(),
                fit: ObjectFit::Contain,
                slice: None,
                slice_scale: 1.,
                repeat: [false; 2],
                tile_scale: 1.,
                corner_colors: None,
                corners: None,
                alpha_layout: Default::default(),
                adjustment: Default::default(),
                slice_center: true,
                source_rect: None,
                origin: Some("long.aside:4:17".into()),
            },
            NodeProps::default(),
            vec![],
        )
        .unwrap();
    let root = stage
        .create(
            NodeKind::Stack,
            NodeProps {
                width: Some(640.),
                height: Some(420.),
                clip: true,
                ..Default::default()
            },
            vec![image],
        )
        .unwrap();
    stage.replace(root).unwrap();
    let mut ui = SessionUi::new(super::tests::session())
        .unwrap()
        .with_asset_options(AssetOptions {
            root: directory.clone(),
            resident_bytes: 230_000,
            metadata_bytes: 32 << 20,
            decode_bytes: 2 << 20,
            input_bytes: 1 << 20,
        });
    let (send, wake) = mpsc::channel();
    ui.set_resource_waker(Arc::new(move || {
        let _ = send.send(());
    }));
    ui.set_stage(&stage);
    let (bridge, widget) = super::tests::mount(&ui);
    (ui, bridge, widget, stage, image, wake, directory)
}

#[test]
fn bc_tiles_load_as_blocks_and_bind_each_visible_draw() {
    use reel_core::{
        budget::{BudgetCategory, BudgetPool, QueueLimits},
        identity::FrameId,
    };
    use reel_ui::{
        frame::{BuildLimits, FrameBuild, FrameDraw, FrameInfo, FrameOp, StepBudget},
        peniko::Brush,
        resources::ResourceSnapshot,
    };
    let (mut ui, mut bridge, widget, _, _, wake, _) =
        fixture_with_encoding(TileEncoding::ReelTexture);
    settle(&mut ui, &mut bridge, widget, &wake);
    assert!(ui.images.loaded.len() >= 2);
    let image = bridge.edit_widget(
        SessionUi::stage_widget(&bridge, widget).unwrap(),
        |mut stage| {
            StageWidget::image_widgets(&mut stage.downcast::<StageWidget>())
                .remove(0)
                .1
        },
    );
    let geometry = bridge.layout_geometry().unwrap();
    let record = geometry.get(image.widget).unwrap();
    let manifest = ui.images.manifests[&image.source];
    let ordinary = crate::images::tiles::visible_tiles(manifest, &image, &geometry).0;
    let mut effect_source = image;
    effect_source.effect_domain = Some((
        geometry.records()[record.parent.unwrap()].id,
        record
            .window_transform
            .transform_rect_bbox(record.size.to_rect()),
    ));
    let complete = crate::images::tiles::visible_tiles(manifest, &effect_source, &geometry).0;
    assert!(ordinary.tiles.len() < complete.tiles.len());
    assert_eq!(
        complete.tiles.len(),
        manifest.width.div_ceil(manifest.tile_edge) as usize,
        "an effect can sample source tiles beyond its parent's visible clip"
    );
    assert!(ui.images.loaded.values().all(|lease|matches!(lease.view(),reel_ui::resources::ResourceView::Texture{descriptor,..} if descriptor.format.is_compressed())));
    assert!(
        ui.images.pool.usage().bytes < 30_000,
        "only native BC blocks of visible tiles and neighbors are resident"
    );
    let storage = ui.images.pool.clone();
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 32,
            max_bytes: 8 << 20,
        },
    );
    let source = bridge.record(FrameId(1)).unwrap();
    let recorded: Vec<_> = source
        .plan()
        .layers
        .iter()
        .flat_map(|layer| layer.command_owners())
        .filter_map(|range| range.image_resource)
        .map(|b| b.resource)
        .collect();
    assert!(recorded.len() >= 2);
    let snapshot =
        ResourceSnapshot::capture(ui.image_resources().iter().cloned().map(Ok), &pool).unwrap();
    let mut build = FrameBuild::new(
        source,
        snapshot,
        FrameInfo {
            logical_size: [640., 420.],
            physical_size: [640, 420],
        },
        BuildLimits { max_bytes: 4 << 20 },
        &pool,
    )
    .unwrap();
    while !build
        .step(StepBudget {
            max_commands: 64,
            max_units: 65536,
            max_bytes: 1 << 20,
        })
        .unwrap()
        .complete
    {}
    let frame = build.finish().unwrap();
    let handles: Vec<_> = frame
        .commands()
        .iter()
        .filter_map(|c| match &c.op {
            FrameOp::Draw(FrameDraw::Fill {
                brush: Brush::Image(image),
                ..
            }) => Some(image.image.resource),
            _ => None,
        })
        .collect();
    assert_eq!(handles, recorded);
    assert!(handles.iter().all(|handle| {
        frame
            .resources()
            .leases()
            .iter()
            .any(|lease| lease.handle() == *handle)
    }));
    drop(ui);
    drop(bridge);
    assert!(storage.usage().bytes > 0);
    drop(frame);
    assert_eq!(storage.usage().bytes, 0);
    assert_eq!(pool.usage().bytes, 0);
}
fn settle(
    ui: &mut SessionUi,
    bridge: &mut PresentationBridge,
    widget: WidgetId,
    wake: &mpsc::Receiver<()>,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        ui.sync(bridge, widget).unwrap();
        if !ui.images_pending() && !ui.images.assets.needs_poll() {
            return;
        }
        assert!(Instant::now() < deadline, "tile readiness deadline");
        let _ = wake.recv_timeout(Duration::from_millis(10));
    }
}

#[test]
fn prepared_atlas_windows_select_and_paint_only_their_source_region() {
    use reel_core::{
        budget::{BudgetCategory, BudgetPool, QueueLimits},
        identity::FrameId,
    };
    use reel_ui::{
        frame::{BuildLimits, FrameBuild, FrameDraw, FrameInfo, FrameOp, StepBudget},
        imaging::record::Geometry,
        kurbo::{Point, Rect},
        peniko::Brush,
        resources::ResourceSnapshot,
    };
    let (mut ui, mut bridge, widget, mut stage, image, wake, _) = fixture();
    for (index, crop, columns) in [
        (0, Rect::new(128.5, 0.5, 385.5, 32.5), vec![1, 2, 3]),
        (1, Rect::new(1024.5, 0.5, 1281.5, 32.5), vec![8, 9, 10]),
        (2, Rect::new(-64., 0., -1., 33.), vec![0]),
        (3, Rect::new(40002., 0., 40066., 33.), vec![312]),
    ] {
        let old = stage.node(image).unwrap();
        let mut kind = old.kind.clone();
        let NodeKind::Image {
            source_rect, fit, ..
        } = &mut kind
        else {
            unreachable!()
        };
        *source_rect = Some(crop);
        *fit = ObjectFit::Stretch;
        let mut props = old.props.clone();
        props.width = Some(crop.width() * 2.);
        props.height = Some(crop.height() * 2.);
        stage.update(image, kind, props).unwrap();
        ui.set_stage(&stage);
        settle(&mut ui, &mut bridge, widget, &wake);
        let required = ui
            .images
            .tiles
            .iter()
            .filter(|(_, tile)| tile.required)
            .map(|(source, _)| source.as_str())
            .collect::<Vec<_>>();
        let mut expected = columns
            .iter()
            .map(|x| format!("long.reel-image.tiles/0/{x}-0.png"))
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(
            required,
            expected.iter().map(String::as_str).collect::<Vec<_>>()
        );
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 32,
                max_bytes: 8 << 20,
            },
        );
        let resources =
            ResourceSnapshot::capture(ui.image_resources().iter().cloned().map(Ok), &pool).unwrap();
        let mut build = FrameBuild::new(
            bridge.record(FrameId(index + 1)).unwrap(),
            resources,
            FrameInfo {
                logical_size: [640., 420.],
                physical_size: [640, 420],
            },
            BuildLimits { max_bytes: 4 << 20 },
            &pool,
        )
        .unwrap();
        while !build
            .step(StepBudget {
                max_commands: 64,
                max_units: 65536,
                max_bytes: 1 << 20,
            })
            .unwrap()
            .complete
        {}
        let frame = build.finish().unwrap();
        let draws = frame
            .commands()
            .iter()
            .filter_map(|command| match &command.op {
                FrameOp::Draw(FrameDraw::Fill {
                    transform,
                    brush: Brush::Image(image),
                    brush_transform: Some(sampling),
                    shape: Geometry::Rect(bounds),
                    ..
                }) => Some((*transform, image, *sampling, *bounds)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            draws.len(),
            columns.len(),
            "prefetch tiles never draw outside the crop"
        );
        let mut area = 0.;
        for (transform, image, sampling, bounds) in draws {
            let target = transform.transform_rect_bbox(bounds);
            assert!(target.x0 >= 0. && target.x1 <= crop.width() * 2.);
            assert!(target.y0 >= 0. && target.y1 <= crop.height() * 2.);
            area += target.area();
            let window = image
                .image
                .source_rect
                .expect("per-tile crop sampling window");
            assert_eq!(window, sampling.transform_rect_bbox(crop));
            assert_eq!(transform * crop.origin(), Point::ORIGIN);
        }
        assert_eq!(
            area,
            crop.area() * 4.,
            "tile geometry covers the complete crop, including padded image edges"
        );
    }
}
fn pan(ui: &mut SessionUi, stage: &mut Stage, image: reel_ui::stage::NodeId, x: f64, scale: f64) {
    let old = stage.node(image).unwrap();
    let mut props = old.props.clone();
    props.x = x;
    props.scale_x = scale;
    props.scale_y = scale;
    stage.update(image, old.kind.clone(), props).unwrap();
    ui.set_stage(stage);
}

#[test]
fn huge_image_cold_warm_pan_zoom_and_retirement_use_visible_residency() {
    let (mut ui, mut bridge, widget, mut stage, image, wake, _) = fixture();
    let started = Instant::now();
    settle(&mut ui, &mut bridge, widget, &wake);
    let cold = ui.images.loads;
    assert!(
        cold <= 9,
        "manifest + visible tiles + one neighbor; got {cold}"
    );
    assert!(ui.images.loaded.len() <= 8);
    assert!(!ui.image_resources().is_empty());
    {
        let requests = (ui.images.stage_requests, ui.images.tile_requests);
        let resources = ui.stage.as_ref().unwrap().resources.clone();
        for index in 0..100 {
            let node = stage.node(image).unwrap();
            let mut props = node.props.clone();
            props.opacity = f64::from(index) / 99.;
            props.color = [index as u8, 128, 255, 255];
            let mut kind = node.kind.clone();
            let NodeKind::Image { sampling, .. } = &mut kind else {
                unreachable!()
            };
            *sampling = if index % 2 == 0 {
                reel_ui::peniko::ImageQuality::Low
            } else {
                reel_ui::peniko::ImageQuality::Medium
            };
            stage.update(image, kind, props).unwrap();
            ui.set_stage(&stage);
            ui.sync(&mut bridge, widget).unwrap();
            assert!(!ui.images_pending());
            assert_eq!(
                (ui.images.stage_requests, ui.images.tile_requests),
                requests
            );
            assert!(Arc::ptr_eq(
                &ui.stage.as_ref().unwrap().resources,
                &resources
            ));
        }
        ui.clear_presented();
        settle(&mut ui, &mut bridge, widget, &wake);
        assert!(ui.images.stage_requests > requests.0);
        assert!(ui.images.tile_requests > requests.1);
        assert!(Arc::ptr_eq(
            &ui.stage.as_ref().unwrap().resources,
            &resources
        ));
        eprintln!(
            "paint_updates=100 additional_stage_requests=0 additional_tile_requests=0 shared_leases=true"
        );
    }
    let first = ui.image_resources()[0].clone();
    let bytes = ui.images.pool.usage().bytes;
    assert!(
        bytes < 150_000,
        "entire 40001x33 source is over 5 MB: {bytes}"
    );
    {
        use reel_core::{
            budget::{BudgetCategory, BudgetPool, QueueLimits},
            identity::FrameId,
        };
        use reel_ui::{
            frame::{BuildLimits, FrameBuild, FrameInfo, StepBudget},
            resources::ResourceSnapshot,
        };
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 32,
                max_bytes: 8 << 20,
            },
        );
        let source = bridge.record(FrameId(1)).unwrap();
        let resources =
            ResourceSnapshot::capture(ui.image_resources().iter().cloned().map(Ok), &pool).unwrap();
        let mut build = FrameBuild::new(
            source,
            resources,
            FrameInfo {
                logical_size: [640., 420.],
                physical_size: [640, 420],
            },
            BuildLimits { max_bytes: 4 << 20 },
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
        let clips = frame
            .commands()
            .iter()
            .filter(|command| matches!(command.op, reel_ui::frame::FrameOp::PushClip(_)))
            .count();
        assert_eq!(clips, 3, "session, stage and image clips, never per tile");
        assert_eq!(
            frame.resources().leases().len(),
            ui.image_resources().len(),
            "every painted tile has a frozen existing resource lease"
        );
    }
    super::tests::accept(&mut ui, &mut bridge, widget, 2);
    pan(&mut ui, &mut stage, image, -32., 1.);
    settle(&mut ui, &mut bridge, widget, &wake);
    assert!(
        ui.images.loads <= cold + 1,
        "warm visible tiles must be reused; only new ring edge can load"
    );
    let warm = ui.images.loads;
    pan(&mut ui, &mut stage, image, -20_000., 1.);
    ui.sync(&mut bridge, widget).unwrap();
    assert!(ui.images_pending());
    assert!(
        bridge.accepted_key().is_some(),
        "metadata layout preserves accepted input view"
    );
    // Supersede cold demand before its tile completes.
    pan(&mut ui, &mut stage, image, -30_000., 1.);
    settle(&mut ui, &mut bridge, widget, &wake);
    assert!(
        ui.images
            .loaded
            .keys()
            .all(|key| !key.contains("/0/0-") && !key.contains("/0/156-"))
    );
    assert!(
        !first.is_retired(),
        "the retained tile remains shared after this view releases it"
    );
    pan(&mut ui, &mut stage, image, -10_000., 0.25);
    settle(&mut ui, &mut bridge, widget, &wake);
    assert!(
        ui.images.loaded.keys().all(|key| key.contains(".tiles/2/")),
        "zoom selects pyramid level"
    );
    let peak = ui.images.pool.usage().peak_bytes;
    assert!(
        peak <= 230_000,
        "accepted + candidate + prefetch remain bounded: {peak}"
    );
    eprintln!(
        "prepared_image original=40001x33 tile=128 cold_requests={cold} warm_requests={warm} final_requests={} resident={} peak_cpu={} elapsed={:?}",
        ui.images.loads,
        ui.images.pool.usage().bytes,
        peak,
        started.elapsed()
    );
    drop(first);
    ui.close_resources();
    bridge.edit_widget(widget, |mut widget| {
        let mut widget = widget.downcast::<SessionWidget>();
        SessionWidget::sync(&mut widget, &ui);
    });
    drop(bridge);
    assert_eq!(ui.images.pool.usage().bytes, 0);
}

#[test]
fn missing_visible_tile_keeps_constructor_source_diagnostic() {
    let (mut ui, mut bridge, widget, _, _, wake, directory) = fixture();
    fs::remove_file(directory.join("long.reel-image.tiles/0/0-0.png")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match ui.sync(&mut bridge, widget) {
            Err(error) => {
                let error = error.to_string();
                assert!(error.contains("long.aside:4:17"), "{error}");
                assert!(error.contains("0-0.png"), "{error}");
                break;
            }
            Ok(()) => {
                assert!(Instant::now() < deadline);
                let _ = wake.recv_timeout(Duration::from_millis(10));
            }
        }
    }
    assert!(bridge.accepted_key().is_none());
}

#[test]
fn two_dimensional_shared_sources_use_composed_ancestor_rotation_zoom_and_hidden_clips() {
    use reel_core::{budget::QueueLimits, test_support::MemoryAssetIo};
    let assets = crate::assets::Assets::with_adapters(AssetOptions::default(), |wake| {
        let limits = QueueLimits {
            max_requests: 32,
            max_bytes: 16 << 20,
        };
        (
            Box::new(MemoryAssetIo::new(limits, limits, wake.clone())),
            Box::new(crate::assets::ImageExecutor::new(limits, wake)),
        )
    });
    let mut stage = Stage::default();
    let mut create_image = |x, y| {
        stage
            .create(
                NodeKind::Image {
                    sampling: Default::default(),
                    source: "world.reel-image".into(),
                    fit: ObjectFit::Contain,
                    slice: None,
                    slice_scale: 1.,
                    repeat: [false; 2],
                    tile_scale: 1.,
                    corner_colors: None,
                    corners: None,
                    alpha_layout: Default::default(),
                    adjustment: Default::default(),
                    slice_center: true,
                    source_rect: None,
                    origin: Some("world.aside:8:5".into()),
                },
                NodeProps {
                    x,
                    y,
                    ..Default::default()
                },
                vec![],
            )
            .unwrap()
    };
    let near = create_image(-10_000., -9_000.);
    let far = create_image(-20_000., -20_000.);
    let root = stage
        .create(
            NodeKind::Stack,
            NodeProps {
                width: Some(640.),
                height: Some(420.),
                clip: true,
                ..Default::default()
            },
            vec![near, far],
        )
        .unwrap();
    stage.replace(root).unwrap();
    let mut ui = SessionUi::new(super::tests::session())
        .unwrap()
        .with_assets(assets);
    ui.set_stage(&stage);
    ui.images.manifests.insert(
        "world.reel-image".into(),
        PreparedImage::new(
            40001,
            30001,
            128,
            reel_core::prepared_image::TileEncoding::Png,
        )
        .unwrap(),
    );
    let (mut bridge, widget) = super::tests::mount(&ui);
    ui.sync(&mut bridge, widget).unwrap();
    let tiles = &ui.images.tiles;
    assert!(tiles["world.reel-image.tiles/0/78-70.png"].required);
    assert!(tiles["world.reel-image.tiles/0/161-159.png"].required);
    assert!(
        tiles.len() < 120,
        "two disjoint views must not load their bounding union: {}",
        tiles.len()
    );
    assert!(ui.images_pending());
    assert!(
        bridge.accepted_key().is_none(),
        "metadata candidate never accepts presentation"
    );
    let old = stage.node(root).unwrap();
    let mut props = old.props.clone();
    props.rotation = 30.;
    props.scale_x = 0.5;
    props.scale_y = 0.5;
    stage.update(root, old.kind.clone(), props).unwrap();
    ui.set_stage(&stage);
    ui.sync(&mut bridge, widget).unwrap();
    assert!(!ui.images.tiles.is_empty());
    assert!(
        ui.images.tiles.keys().all(|key| key.contains(".tiles/1/")),
        "composed ancestor zoom selects mip"
    );
    assert!(
        ui.images.tiles.len() < 120,
        "rotation clips conservative demand to the viewport"
    );
    ui.images
        .manifests
        .get_mut("world.reel-image")
        .unwrap()
        .first_level = 2;
    ui.set_stage(&stage);
    ui.sync(&mut bridge, widget).unwrap();
    assert!(
        !ui.images.tiles.is_empty() && ui.images.tiles.keys().all(|key| key.contains(".tiles/2/")),
        "runtime demand never selects levels omitted by resource preparation"
    );
    for id in [near, far] {
        let old = stage.node(id).unwrap();
        let mut props = old.props.clone();
        props.visible = false;
        stage.update(id, old.kind.clone(), props).unwrap();
    }
    ui.set_stage(&stage);
    ui.sync(&mut bridge, widget).unwrap();
    assert!(ui.images.tiles.is_empty());
    assert!(!ui.images_pending());
    let old = stage.node(root).unwrap();
    let mut props = old.props.clone();
    props.width = Some(0.);
    props.height = Some(0.);
    stage.update(root, old.kind.clone(), props).unwrap();
    ui.set_stage(&stage);
    ui.sync(&mut bridge, widget).unwrap();
    assert!(ui.images.tiles.is_empty());
}
