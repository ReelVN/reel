//! Real source/ASB author images through the public player and frame pipeline.

use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    identity::{FrameId, SurfaceEpoch},
};
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{SliceBudget, VmLimits, source::CompiledStory},
};
use reel_player::{
    aside_session::{AsideSessionUi, StoryProgress, StoryState},
    assets::AssetOptions,
};
use reel_ui::{
    app::{GeometryLimits, RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{
        BridgeLimits, NoticeResult, PresentationBridge, PresentationNotice, PresentationOutcome,
    },
    core::{DefaultProperties, Widget},
    dpi::PhysicalSize,
    frame::{BuildLimits, FrameBuild, FrameDraw, FrameInfo, FrameOp, RenderFrame, StepBudget},
    peniko::Brush,
    resources::{ColorEncoding, ResourceData, ResourceSnapshot},
};
use std::{
    path::Path,
    sync::{Arc, mpsc},
    time::Duration,
};

const SOURCE: &str = r#"
fn main() {
    stage.replace(stack() {
        image("assets/panels.png");
        image("assets/panels.png", x: 180, width: 80, anchor_x: 0.5, rotation: 90, scale_x: -2, scale_y: 2);
        image("assets/sun.jpg", x: 280);
    });
}
"#;

fn render(story: CompiledStory, assets: &Path) -> RenderFrame {
    let mut driver = AsideSessionUi::from_compiled(
        Session::new(SessionId(1), Epoch(1)).unwrap(),
        story,
        "en",
        VmLimits::default(),
    )
    .unwrap()
    .with_asset_options(AssetOptions {
        root: assets.into(),
        ..Default::default()
    });
    let (send, receive) = mpsc::channel();
    driver.set_resource_waker(Arc::new(move || {
        let _ = send.send(());
    }));
    for _ in 0..10_000 {
        match driver.tick(SliceBudget::default(), 256, || 0).unwrap() {
            StoryProgress::Finished => break,
            StoryProgress::Faulted(_) => panic!("{:?}", driver.fault().unwrap()),
            StoryProgress::HostFailed(_) => panic!("{:?}", driver.host_failure()),
            _ => {}
        }
    }
    assert_eq!(driver.state(), StoryState::Finished);
    let widget = driver.widget().prepare();
    let root_id = widget.id();
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
    driver.sync(&mut bridge, root_id).unwrap();
    while driver.ui().images_pending() {
        receive
            .recv_timeout(Duration::from_secs(10))
            .expect("image worker must wake its host");
        driver.sync(&mut bridge, root_id).unwrap();
    }
    assert_eq!(driver.ui().image_resources().len(), 2);
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 32,
            max_bytes: 32 << 20,
        },
    );
    let (source, receipt) = driver.record(&mut bridge, FrameId(1)).unwrap();
    let key = source.key();

    let resources =
        ResourceSnapshot::capture(driver.ui().image_resources().iter().cloned().map(Ok), &pool)
            .unwrap();
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
    let frame = build.finish().unwrap();
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            key,
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(1))
        )),
        NoticeResult::Accepted
    );
    driver.presented(&bridge, &receipt).unwrap();
    frame
}

#[derive(Debug, PartialEq)]
struct ImageDraw {
    version: reel_core::identity::ResourceVersion,
    transform: [f64; 6],
    dimensions: [u32; 2],
    pixels: Vec<u8>,
}

fn images(frame: &RenderFrame) -> Vec<ImageDraw> {
    let mut blob_ids = Vec::new();
    let draws: Vec<_> = frame
        .commands()
        .iter()
        .filter_map(|command| {
            let FrameOp::Draw(FrameDraw::Fill {
                transform,
                brush: Brush::Image(brush),
                ..
            }) = &command.op
            else {
                return None;
            };
            let lease = frame
                .resources()
                .leases()
                .iter()
                .find(|lease| lease.handle() == brush.image.resource)
                .expect("image handle must have a pixel lease");
            let ResourceData::Image { image, encoding } = lease.data() else {
                panic!("expected image pixels")
            };
            assert_eq!(*encoding, ColorEncoding::Srgb);
            blob_ids.push(image.data.id());
            Some(ImageDraw {
                version: lease.version().clone(),
                transform: transform.as_coeffs(),
                dimensions: [image.width, image.height],
                pixels: image.data.as_ref().to_vec(),
            })
        })
        .collect();
    assert_eq!(draws.len(), 3);
    assert_eq!(
        blob_ids[0], blob_ids[1],
        "duplicate sources must share decoded storage"
    );
    assert_ne!(blob_ids[0], blob_ids[2]);
    for (draw, file, dimensions, transform) in [
        (
            &draws[0],
            "assets/panels.png",
            [160, 90],
            [1., 0., 0., 1., 0., 0.],
        ),
        (
            &draws[1],
            "assets/panels.png",
            [160, 90],
            [0., -1., -1., 0., 180., 80.],
        ),
        (
            &draws[2],
            "assets/sun.jpg",
            [96, 96],
            [1., 0., 0., 1., 280., 0.],
        ),
    ] {
        assert_eq!(draw.version.key.0, file);
        assert_eq!(draw.dimensions, dimensions);
        assert!(
            draw.transform
                .iter()
                .zip(transform)
                .all(|(a, b)| (a - b).abs() < 1e-9),
            "transform={:?}, expected={transform:?}",
            draw.transform
        );
    }
    assert_eq!(&draws[0].pixels[..4], &[224, 64, 48, 255]);
    assert_eq!(&draws[0].pixels[80 * 4..81 * 4], &[32, 112, 224, 128]);
    let center = (48 * 96 + 48) * 4;
    assert!(
        draws[2].pixels[center..center + 4]
            .iter()
            .zip([255_i16, 200, 48, 255])
            .all(|(actual, expected)| (i16::from(*actual) - expected).abs() <= 8)
    );
    draws
}

#[test]
fn source_and_asb_share_real_image_frames_with_an_independent_asset_root() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .canonicalize()
        .unwrap();
    let mut expected = None;
    let relocated = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/reel-artifacts/relocated-author-assets");
    std::fs::create_dir_all(relocated.join("assets")).unwrap();
    for file in ["assets/panels.png", "assets/sun.jpg"] {
        std::fs::copy(assets.join(file), relocated.join(file)).unwrap();
    }
    // These diagnostic paths intentionally have no relation to the actual assets.
    for source_path in [
        "source-only/原稿.aside",
        "other-machine/stories/revised.aside",
    ] {
        let checked = reel_cli::check(SOURCE, source_path).unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        let packaged = reel_cli::decode_story(&artifact.bytes).unwrap();
        let program = packaged.program.package();
        assert_eq!(
            program.symbols[program.sources[0].path.index()],
            source_path
        );
        let direct = reel_cli::into_story(checked, "main::main").unwrap();
        for (story, root) in [(direct, &assets), (packaged, &relocated)] {
            let frame = render(story, root);
            let actual = images(&frame);
            if let Some(expected) = &expected {
                assert_eq!(&actual, expected);
            } else {
                expected = Some(actual);
            }
        }
    }
}
