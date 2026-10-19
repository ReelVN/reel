//! Source/ASB geometry uses real UI layout and accepted CPU presentation.
#[allow(dead_code)]
#[path = "support/author.rs"]
mod support;
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
    let checked = reel_cli::check(source, "geometry.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for compiled in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_cli::decode_story(&artifact.bytes).unwrap(),
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
fn geometry_example_compiles_as_source_and_asb() {
    let checked = reel_cli::check(
        include_str!("../../../examples/author_geometry.aside"),
        "author_geometry.aside",
    )
    .unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    reel_cli::decode_story(&artifact.bytes).unwrap();
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
        reel_cli::decode_story(&artifact.bytes).unwrap(),
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
