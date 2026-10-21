//! The creator example starts and retires one effect through presented UI input.
#[path = "support/author.rs"]
#[allow(dead_code)]
mod support;
use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};
use reel_player::aside_session::AsideSessionUi;
use reel_ui::{input::GamepadButton, stage::NodeKind};
use support::*;

#[test]
fn scope_example_start_stop_retires_child_work_from_source_and_asb() {
    let checked = reel_cli::check(
        include_str!("../../../examples/author_scope/src/main.aside"),
        "author_scope.aside",
    )
    .unwrap();
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
        present(&mut driver, &mut bridge, root, 1);
        gamepad_press(&mut driver, &mut bridge, GamepadButton::DpadDown);
        gamepad_press(&mut driver, &mut bridge, GamepadButton::South);
        present(&mut driver, &mut bridge, root, 2);
        settle(&mut driver);
        while driver.advance_time(0.21, 16).unwrap().pending {}
        settle(&mut driver);
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("Scope ticks: 1".into()))
        );
        assert!(driver.activities_need_time());
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("Scoped effect is running".into()))
        );
        present(&mut driver, &mut bridge, root, 3);
        gamepad_press(&mut driver, &mut bridge, GamepadButton::DpadRight);
        gamepad_press(&mut driver, &mut bridge, GamepadButton::South);
        settle(&mut driver);
        assert!(!driver.activities_need_time());
        assert!(
            !driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("Scoped effect is running".into()))
        );
        let panel = driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| n.props.width == Some(220.))
            .unwrap();
        assert_eq!(
            panel.props.opacity, 0.6,
            "Stop exposes the latest read-only derived base value"
        );
        while driver.advance_time(2., 16).unwrap().pending {}
        settle(&mut driver);
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("Scope ticks: 1".into()))
        );
        driver.cancel().unwrap();
    }
}

#[test]
fn scope_offset_example_keeps_base_motion_during_shake_stop_and_completion() {
    let checked = reel_cli::check(
        include_str!("../../../examples/author_offsets/src/main.aside"),
        "author_offsets.aside",
    )
    .unwrap();
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
        while driver.advance_time(1., 64).unwrap().pending {}
        settle(&mut driver);
        present(&mut driver, &mut bridge, root, 1);
        let position = |driver: &AsideSessionUi| {
            let panel = driver
                .adapter()
                .stage()
                .nodes()
                .find(|n| {
                    matches!(&n.kind,
                NodeKind::Button{label:Some(text),..} if text=="Catch me")
                })
                .unwrap();
            (panel.props.x, panel.props.y)
        };
        assert_eq!(position(&driver), (120., 100.));
        click_at(&mut driver, &mut bridge, 60., 40.);
        assert_eq!(position(&driver), (120., 97.));
        while driver.advance_time(1.025, 64).unwrap().pending {}
        settle(&mut driver);
        let (x, y) = position(&driver);
        assert!((x - 125.).abs() < 0.001);
        assert_eq!(y, 97.);
        present(&mut driver, &mut bridge, root, 2);
        click_at(&mut driver, &mut bridge, 180., 40.);
        assert!((position(&driver).0 - 122.).abs() < 0.001);
        assert_eq!(position(&driver).1, 100.);
        assert!(driver.activities_need_time());
        while driver.advance_time(2., 64).unwrap().pending {}
        settle(&mut driver);
        present(&mut driver, &mut bridge, root, 3);
        click_at(&mut driver, &mut bridge, 60., 40.);
        while driver.advance_time(2.8, 64).unwrap().pending {}
        settle(&mut driver);
        assert!((position(&driver).0 - 264.).abs() < 0.001);
        assert_eq!(position(&driver).1, 100.);
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("Base movement continues".into()))
        );
        driver.cancel().unwrap();
    }
}

#[test]
fn scope_offset_parent_coordinates_share_accepted_geometry_and_pointer_hit_testing() {
    let source = r#"fn near(a,b){if a-b>0.001 || b-a>0.001 {throw f"geometry {a} != {b}";}}
    fn main(){
        game.configure(width:640,height:420);
        let data=model({hits:0,phase:"waiting"});let done=completion();
        let target=button("",x:10,y:20,width:40,height:20,on_click:||{data.hits+=1;});
        let parent=stack(x:100,y:50,width:100,height:100,rotation:90,scale_x:2,scale_y:2){target};
        stage.replace(stack(width:640,height:420){
            parent;button("Reset",x:400,y:20,width:100,height:40,on_click:||done.resolve());
            text(derive(||f"hits:{data.hits}"),x:300,y:220);
            text(derive(||data.phase),x:300,y:250);
        });
        scope(body:|s|{
            s.offset(target,x:20,y:-15);
            let g=await stage.geometry(target,relative_to:parent);
            near(g.transform.tx,30);near(g.transform.ty,5);near(g.width,40);near(g.height,20);
            if target.x!=10 || target.y!=20 {throw "changed base";}
            data.phase="offset";await done;
        });
        let g=await stage.geometry(target,relative_to:parent);
        near(g.transform.tx,10);near(g.transform.ty,20);data.phase="base";
    }"#;
    let checked = reel_cli::check(source, "offset_geometry.aside").unwrap();
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
        let has = |driver: &AsideSessionUi, text: &str| {
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text(text.into()))
        };
        for frame in 1..12 {
            present(&mut driver, &mut bridge, root, frame);
            settle(&mut driver);
            if has(&driver, "offset") {
                break;
            }
        }
        assert!(has(&driver, "offset"));
        click_at(&mut driver, &mut bridge, 80., 180.);
        assert!(has(&driver, "hits:1"));
        click_at(&mut driver, &mut bridge, 450., 40.);
        for frame in 12..24 {
            present(&mut driver, &mut bridge, root, frame);
            settle(&mut driver);
            if has(&driver, "base") {
                break;
            }
        }
        assert!(has(&driver, "base"));
        click_at(&mut driver, &mut bridge, 80., 180.);
        assert!(has(&driver, "hits:1"));
        click_at(&mut driver, &mut bridge, 40., 100.);
        assert!(has(&driver, "hits:2"));
        driver.cancel().unwrap();
    }
}
