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
        include_str!("../../../examples/author_scope.aside"),
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
