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
    resources::{ColorEncoding, ResourceSnapshot},
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
    render_at(story, assets, false)
}

fn render_at(story: CompiledStory, assets: &Path, mouse: bool) -> RenderFrame {
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
    if mouse {
        let (first, receipt) = driver
            .record(&mut bridge, root_id, FrameId(0))
            .unwrap()
            .unwrap();
        assert_eq!(
            bridge.resolve(PresentationNotice::new(
                first.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(FrameId(0))
            )),
            NoticeResult::Accepted
        );
        driver.presented(&bridge, &receipt).unwrap();
        use reel_ui::{core::*, dpi::PhysicalPosition};
        driver
            .handle_pointer(
                &mut bridge,
                PointerEvent::Move(PointerUpdate {
                    pointer: PointerInfo {
                        pointer_id: Some(PointerId::PRIMARY),
                        persistent_device_id: None,
                        pointer_type: PointerType::Mouse,
                    },
                    current: PointerState {
                        position: PhysicalPosition::new(50., 60.),
                        ..Default::default()
                    },
                    coalesced: vec![],
                    predicted: vec![],
                }),
            )
            .unwrap();
    }

    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 32,
            max_bytes: 32 << 20,
        },
    );
    let (source, receipt) = driver
        .record(&mut bridge, root_id, FrameId(1))
        .unwrap()
        .expect("settled frame");
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
            let image = lease.image_descriptor().expect("expected image metadata");
            let pixels = lease.read_pixels().expect("expected image pixels");
            assert_eq!(image.encoding, ColorEncoding::Srgb);
            blob_ids.push(pixels.blob().id());
            Some(ImageDraw {
                version: lease.version().clone(),
                transform: transform.as_coeffs(),
                dimensions: [image.width, image.height],
                pixels: pixels.bytes().to_vec(),
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
        let packaged = reel_player::distribution::decode_story(&artifact.bytes).unwrap();
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
                let expected: &Vec<ImageDraw> = expected;
                assert_eq!(actual.len(), expected.len());
                for (actual, expected) in actual.iter().zip(expected) {
                    assert_eq!(actual.version.key, expected.version.key);
                    assert_eq!(actual.transform, expected.transform);
                    assert_eq!(actual.dimensions, expected.dimensions);
                    assert!(
                        actual.pixels == expected.pixels,
                        "relocated resource pixels"
                    );
                }
            } else {
                expected = Some(actual);
            }
        }
    }
}

#[test]
fn cursor_source_and_asb_use_ordinary_image_resource_leases_after_owner_drop() {
    let source = r#"fn main() {stage.replace(stack(width:640,height:420) {
        cursor {image("assets/panels.png",width:70,height:70,anchor_x:0.5,anchor_y:0.5);image("assets/sun.jpg",x:80);};
    });}"#;
    let checked = reel_cli::check(source, "cursor-images.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    for story in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
    ] {
        let frame = render_at(story, &assets, true);
        let mut count = 0;
        for command in frame.commands() {
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
                    .unwrap();
                assert!(
                    !lease
                        .read_pixels()
                        .expect("cursor image lease")
                        .bytes()
                        .is_empty()
                );
                count += 1;
            }
        }
        assert_eq!(
            count, 2,
            "cursor images stay in the common frame with valid leases after its driver is dropped"
        );
    }
}

#[path = "support/author.rs"]
#[allow(dead_code)]
mod support;

mod author_cursor {
    //! The actual example runs from source and ASB; mouse motion never writes VM coordinates.
    use super::support;
    use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};
    use reel_player::aside_session::AsideSessionUi;
    use reel_ui::{core::*, dpi::PhysicalPosition, stage::NodeKind};
    use support::*;

    #[test]
    fn cursor_example_source_and_asb_animate_retire_and_move_without_vm_allocation() {
        let checked = reel_cli::check(
            include_str!("../../../examples/author_cursor/src/main.aside"),
            "author_cursor.aside",
        )
        .unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        for compiled in [
            reel_cli::into_story(checked, "main::main").unwrap(),
            reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
        ] {
            let mut d = AsideSessionUi::from_compiled(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                compiled,
                "en",
                VmLimits::default(),
            )
            .unwrap();
            settle(&mut d);
            let (mut b, root) = bridge(&d);
            present(&mut d, &mut b, root, 1);
            assert_eq!(
                d.adapter()
                    .stage()
                    .nodes()
                    .filter(|n| n.kind == NodeKind::Cursor)
                    .count(),
                2
            );
            assert!(d.activities_need_time());
            let heap = d.adapter().vm().heap().stats();
            let revision = d.adapter().stage().revision();
            let started = std::time::Instant::now();
            for frame in 2..202 {
                d.handle_pointer(
                    &mut b,
                    PointerEvent::Move(PointerUpdate {
                        pointer: PointerInfo {
                            pointer_id: Some(PointerId::PRIMARY),
                            persistent_device_id: None,
                            pointer_type: PointerType::Mouse,
                        },
                        current: PointerState {
                            position: PhysicalPosition::new(80. + (frame % 50) as f64, 120.),
                            ..Default::default()
                        },
                        coalesced: vec![],
                        predicted: vec![],
                    }),
                )
                .unwrap();
                present(&mut d, &mut b, root, frame);
            }
            let after = d.adapter().vm().heap().stats();
            assert_eq!(after.chunk_allocations, heap.chunk_allocations);
            assert_eq!(after.charged_bytes, heap.charged_bytes);
            assert_eq!(d.adapter().stage().revision(), revision);
            eprintln!(
                "cursor 200 mouse/present cycles: {:?}; VM chunks delta=0, VM bytes delta=0, stage revision delta=0",
                started.elapsed()
            );
            while d.advance_time(0.15, 16).unwrap().pending {}
            settle(&mut d);
            let pointer = d
                .adapter()
                .stage()
                .nodes()
                .find(|n| n.kind == NodeKind::Cursor && !n.children.is_empty())
                .unwrap();
            let mark = d.adapter().stage().node(pointer.children[0]).unwrap();
            assert!(
                (mark.props.opacity - 0.675).abs() < 0.001,
                "ordinary cursor subtree animation"
            );
            present(&mut d, &mut b, root, 202);
            // The second button owns ordinary input outside the cursor's panel.
            click_at(&mut d, &mut b, 250., 260.);
            present(&mut d, &mut b, root, 203);
            assert_eq!(
                d.adapter()
                    .stage()
                    .nodes()
                    .filter(|n| n.kind == NodeKind::Cursor)
                    .count(),
                0
            );
            assert!(
                !d.activities_need_time(),
                "retirement cancels the cursor's ordinary activity"
            );
            d.cancel().unwrap();
        }
    }
}

mod author_geometry {
    //! Source/ASB geometry uses real UI layout and accepted CPU presentation.
    #[allow(dead_code)]
    use super::support;
    use reel_core::identity::{FrameId, SurfaceEpoch};
    use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};
    use reel_player::aside_session::AsideSessionUi;
    use reel_ui::bridge::{PresentationNotice, PresentationOutcome};
    use reel_ui::stage::NodeKind;
    use support::*;
    fn done(driver: &AsideSessionUi) -> bool {
        driver
            .adapter()
            .stage()
            .nodes()
            .any(|n| n.kind == NodeKind::Text("done".into()))
    }
    fn run(source: &str) {
        run_with_font(source, None);
    }
    fn run_with_font(source: &str, font: Option<reel_ui::peniko::Blob<u8>>) {
        let checked = reel_cli::check(source, "geometry.aside").unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        for compiled in [
            reel_cli::into_story(checked, "main::main").unwrap(),
            reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
        ] {
            let mut driver = AsideSessionUi::from_compiled(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                compiled,
                "en",
                VmLimits::default(),
            )
            .unwrap();
            settle(&mut driver);
            let (mut bridge, root) = bridge_with_font(&driver, font.clone());
            let (first_frame, (rejected, stale)) = (1..30)
                .find_map(|frame| {
                    driver.sync(&mut bridge, root).unwrap();
                    let candidate = driver.record(&mut bridge, root, FrameId(frame)).unwrap();
                    if candidate.is_none() {
                        assert_eq!(bridge.pending_count(), 0);
                        assert!(!done(&driver), "preparation cannot complete geometry");
                        assert!(driver.has_runnable_tasks());
                        settle(&mut driver);
                    }
                    candidate.map(|candidate| (frame, candidate))
                })
                .expect("layout preparation must produce a candidate");

            driver.presented(&bridge, &stale).unwrap();
            settle(&mut driver);
            assert!(!done(&driver), "candidate is not accepted");
            bridge.resolve(PresentationNotice::new(
                rejected.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Skipped,
            ));
            driver.presented(&bridge, &stale).unwrap();
            settle(&mut driver);
            assert!(!done(&driver), "skipped candidate is not accepted");
            for frame in first_frame + 1..first_frame + 30 {
                present(&mut driver, &mut bridge, root, frame);
                settle(&mut driver);
                // Old receipts cannot change the accepted-frame confirmation.
                driver.presented(&bridge, &stale).unwrap();
                if done(&driver) {
                    break;
                }
            }
            assert!(done(&driver));
            driver.cancel().unwrap();
        }
    }
    #[test]
    #[ignore = "requires REEL_SESSION_FONT; CPU layout only"]
    fn text_endpoint_follows_layout_reveal_padding_and_reference_transforms() {
        let font =
            std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("configured font")).unwrap();
        run_with_font(
            r#"
    fn near(a,b){if a-b>0.01 || b-a>0.01 {throw f"endpoint expected {b}, got {a}";}}
    fn main(){
        let body=text("MMMM",x:30,y:40,width:200,height:120,padding:10,font_size:20,line_height:1.5,text_wrap:false);
        let empty=text("");let container=stack();
        let root=stack(width:640,height:420){body;empty;container;};stage.replace(root);
        let start=await stage.geometry(body,relative_to:body);
        if start.text_end.x<=10{throw "missing glyph advance";}
        near(start.text_end.y,40);
        body.reveal=0;
        let unrevealed=await stage.geometry(body,relative_to:body);
        near(unrevealed.text_end.x,start.text_end.x);near(unrevealed.text_end.y,start.text_end.y);
        body.value="MMMM\nM";
        let multiline=await stage.geometry(body,relative_to:body);
        near(multiline.text_end.x,10+(start.text_end.x-10)/4);near(multiline.text_end.y,70);
        body.value="MMMM\n";
        let trailing=await stage.geometry(body,relative_to:body);
        near(trailing.text_end.x,10);near(trailing.text_end.y,70);
        body.value="M";body.text_align="end";
        let aligned=await stage.geometry(body,relative_to:body);near(aligned.text_end.x,190);
        body.text_align="start";body.value="M M M M M M";body.width=45;body.text_wrap=true;
        let wrapped=await stage.geometry(body,relative_to:body);
        if wrapped.text_end.y<=40 || wrapped.text_end.x>35 {throw "endpoint ignores wrapping";}
        body.width=200;body.value=rich("[ruby=long annotation]M[/ruby]");
        let ruby=await stage.geometry(body,relative_to:body);
        if ruby.text_end.x<=10+(start.text_end.x-10)/4 || ruby.text_end.y<=40 {throw "endpoint ignores ruby";}
        body.rotation=90;body.scale_x=-2;
        let transformed=await stage.geometry(body);
        near(transformed.text_end.x,transformed.transform.a*ruby.text_end.x+transformed.transform.c*ruby.text_end.y+transformed.transform.tx);
        near(transformed.text_end.y,transformed.transform.b*ruby.text_end.x+transformed.transform.d*ruby.text_end.y+transformed.transform.ty);
        if (await stage.geometry(empty)).text_end!=null || (await stage.geometry(container)).text_end!=null {throw "non-text endpoint";}
        stage.replace(text("done"));narrator("done");
    }"#,
            Some(reel_ui::peniko::Blob::from(font)),
        );
    }
    #[test]
    fn geometry_source_and_asb_cover_sibling_overlay_camera_scroll_rotation_and_mirroring() {
        run(r#"
    fn near(a,b){if a-b>0.001 || b-a>0.001 {throw f"geometry expected {b}, got {a}";}}
    fn main(){
        let target=stack(x:30,y:40,width:40,height:20,rotation:90,scale_x:-1);
        let view=camera(x:40,y:20,width:200,height:160,camera_x:100,camera_y:80,zoom:2){target};
        let overlay=stack(x:10,y:30,width:300,height:200,scale_x:2,scale_y:2,visible:false);
        let child=stack(x:10,y:150,width:40,height:20);
        let viewport=scroll(x:20,y:30,width:120,height:80,scroll_y:100){stack(width:120,height:400){child}};
        let root=stack(width:640,height:420){view;viewport;overlay;};stage.replace(root);
        await stage.presented();
        overlay.visible=true;overlay.opacity=0;
        let g=await stage.geometry(target,relative_to:overlay,owner:root);
        near(g.width,40);near(g.height,20);near(g.transform.a,0);near(g.transform.b,-1);
        near(g.transform.c,-1);near(g.transform.d,0);near(g.transform.tx,-5);near(g.transform.ty,-5);
        near(g.bounds.x,-25);near(g.bounds.y,-45);near(g.bounds.width,20);near(g.bounds.height,40);
        let local=await stage.geometry(target,relative_to:view);
        near(local.bounds.x,-80);near(local.bounds.y,-80);
        view.camera_rotation=90;
        let turned=await stage.geometry(target,relative_to:view);
        near(turned.transform.a,-2);near(turned.transform.b,0);
        near(turned.transform.c,0);near(turned.transform.d,2);
        near(turned.transform.tx,20);near(turned.transform.ty,220);
        let scrolled=await stage.geometry(child);
        near(scrolled.bounds.x,30);near(scrolled.bounds.y,80);
        let origin=await stage.geometry(child,relative_to:viewport);
        near(origin.bounds.x,10);near(origin.bounds.y,50);
        overlay.opacity=1;stage.replace(text("done"));narrator("done");
    }"#);
    }
    #[test]
    fn geometry_hidden_or_singular_nodes_reject_after_acceptance() {
        run(r#"fn unavailable(target,relative){let mut caught=false;
        try{await stage.geometry(target,relative_to:relative);}catch error{caught=error.code=="GeometryUnavailable";}
        if !caught{throw "unavailable geometry succeeded";}}
    fn main(){let target=stack(width:40,height:20);let overlay=stack(width:100,height:100,visible:false);
        let hidden_child=stack(width:20,height:20);let hidden_parent=stack(visible:false){hidden_child};
        let root=stack{target;overlay;hidden_parent};stage.replace(root);
        unavailable(hidden_child,target);
        unavailable(target,overlay);
        overlay.visible=true;overlay.scale_x=0;unavailable(target,overlay);
        overlay.scale_x=1;target.visible=false;unavailable(target,overlay);
        target.visible=true;overlay.opacity=0;let g=await stage.geometry(target,relative_to:overlay);
        if g.width!=40{throw "transparent nodes have geometry";}stage.replace(text("done"));narrator("done");
    }"#);
    }
    #[test]
    fn geometry_after_virtual_scroll_to_returns_the_accepted_row() {
        run(
            r#"fn main(){let data=model({items:[]});let mut i=0;while i<1000{data.items.append(i);i+=1;}
        let rows=list(||data.items,key:|x|x,virtual:true,build:|row|stack(width:300,height:20+row.value%3*20));
        let overlay=stack(width:640,height:420,opacity:0);
        let viewport=scroll(width:320,height:120){rows};stage.replace(stack{viewport;overlay});
        let row=await rows.scroll_to(980,align:"center");
        let g=await stage.geometry(row,relative_to:overlay);
        if g.width!=320 || g.height!=60 || g.bounds.y<0 || g.bounds.y+g.height>120 {throw "row is not positioned";}
        stage.replace(text("done"));narrator("done");}"#,
        );
    }

    #[test]
    fn geometry_old_surface_candidate_cannot_complete_a_request() {
        let source = r#"fn main(){let target=stack(width:20,height:30);stage.replace(target);
        let g=await stage.geometry(target);if g.width!=20{throw "geometry";}
        stage.replace(text("done"));narrator("done");}"#;
        let checked = reel_cli::check(source, "surface.aside").unwrap();
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            reel_cli::into_story(checked, "main::main").unwrap(),
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let (mut bridge, root) = bridge(&driver);
        driver.sync(&mut bridge, root).unwrap();
        let (source, stale) = driver
            .record(&mut bridge, root, FrameId(1))
            .unwrap()
            .expect("settled frame");

        assert!(bridge.replace_surface(SurfaceEpoch(2)));
        bridge.resolve(PresentationNotice::new(
            source.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(1)),
        ));
        driver.presented(&bridge, &stale).unwrap();
        settle(&mut driver);
        assert!(!done(&driver));
        let (current, receipt) = driver
            .record(&mut bridge, root, FrameId(2))
            .unwrap()
            .expect("settled frame");

        bridge.resolve(PresentationNotice::new(
            current.key(),
            SurfaceEpoch(2),
            PresentationOutcome::Accepted(FrameId(2)),
        ));
        driver.presented(&bridge, &receipt).unwrap();
        settle(&mut driver);
        assert!(done(&driver));
    }
    #[test]
    fn geometry_old_receipt_and_unsynced_model_cannot_stamp_old_layout_as_a_new_request() {
        let source = r#"fn main(){let target=stack(width:20,height:30);stage.replace(target);
        await stage.presented();target.width=40;
        let geometry=await stage.geometry(target);
        if geometry.width!=40 {throw "new request received old layout";}
        stage.replace(text("done"));narrator("done");}"#;
        let checked = reel_cli::check(source, "receipt.aside").unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        for compiled in [
            reel_cli::into_story(checked, "main::main").unwrap(),
            reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
        ] {
            let mut driver = AsideSessionUi::from_compiled(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                compiled,
                "en",
                VmLimits::default(),
            )
            .unwrap();
            settle(&mut driver);
            let (mut bridge, root) = bridge(&driver);
            driver.sync(&mut bridge, root).unwrap();
            let (old, receipt) = driver
                .record(&mut bridge, root, FrameId(1))
                .unwrap()
                .expect("settled frame");
            bridge.resolve(PresentationNotice::new(
                old.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(FrameId(1)),
            ));
            driver.presented(&bridge, &receipt).unwrap();
            settle(&mut driver);
            assert!(driver.adapter().geometry_queries().next().is_some());
            // Keep both the accepted old source and its receipt alive across request 2.
            driver.presented(&bridge, &receipt).unwrap();
            settle(&mut driver);
            assert!(!done(&driver));
            // A recording before sync contains the old widget tree and its old watermark.
            let (unsynced, unsynced_receipt) = driver
                .record(&mut bridge, root, FrameId(2))
                .unwrap()
                .expect("settled frame");
            bridge.resolve(PresentationNotice::new(
                unsynced.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(FrameId(2)),
            ));
            driver.presented(&bridge, &unsynced_receipt).unwrap();
            settle(&mut driver);
            assert!(!done(&driver));
            driver.sync(&mut bridge, root).unwrap();
            driver.presented(&bridge, &receipt).unwrap();
            driver.presented(&bridge, &unsynced_receipt).unwrap();
            settle(&mut driver);
            assert!(!done(&driver));
            present(&mut driver, &mut bridge, root, 3);
            settle(&mut driver);
            assert!(done(&driver));
        }
    }
}
