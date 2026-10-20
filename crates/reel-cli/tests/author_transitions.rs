//! Transition barriers and input ownership through the real accepted CPU stage.
#[path = "support/author.rs"]
#[allow(dead_code)]
mod support;
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{VmLimits, transitions::TransitionPhase},
};
use reel_player::aside_session::AsideSessionUi;
use reel_ui::bridge::{PresentationNotice, PresentationOutcome};
use support::*;

#[test]
fn transition_freezes_only_accepted_content_and_gates_its_target_from_source_and_asb() {
    let source = r##"
        fn main() {
            let state = model({enabled: true});
            let panel = stack(x: 10, y: 10, width: 120, height: 60,
                enabled: derive(|| state.enabled), background: "#ff0000") {
                button("", width: 100, height: 50,
                    on_click: |event| game.configure(title: "inside"));
            };
            stage.replace(stack(width: 640, height: 420) {
                panel;
                button("", x: 200, y: 10, width: 100, height: 50,
                    on_click: |event| game.configure(title: "outside"));
            });
            game.configure(title: "before");
            stage.transition(target: panel, duration: 0.2, change: || {
                panel.background = "#0000ff";
                game.configure(title: "changing");
            });
            if !panel.enabled || !state.enabled { throw "transition overwrote enabled"; }
            game.configure(title: "completed");
            await completion();
        }
    "##;
    let checked = reel_cli::check(source, "transitions.aside").unwrap();
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
        let (recording, receipt) = driver
            .record(&mut bridge, root, FrameId(1))
            .unwrap()
            .unwrap();
        driver.presented(&bridge, &receipt).unwrap();
        settle(&mut driver);
        assert_eq!(driver.adapter().game_settings().title, "before");
        bridge.resolve(PresentationNotice::new(
            recording.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Skipped,
        ));
        driver.presented(&bridge, &receipt).unwrap();
        settle(&mut driver);
        assert_eq!(driver.adapter().game_settings().title, "before");
        let mut frame = 1;
        for _ in 0..30 {
            frame += 1;
            present(&mut driver, &mut bridge, root, frame);
            settle(&mut driver);
            if driver
                .adapter()
                .transition_views()
                .any(|view| view.phase == TransitionPhase::Playing)
            {
                break;
            }
        }
        assert_eq!(driver.adapter().game_settings().title, "changing");
        assert!(
            driver
                .adapter()
                .transition_views()
                .any(|view| view.phase == TransitionPhase::Playing)
        );
        click_at(&mut driver, &mut bridge, 50., 30.);
        assert_eq!(driver.adapter().game_settings().title, "changing");
        click_at(&mut driver, &mut bridge, 250., 30.);
        assert_eq!(driver.adapter().game_settings().title, "outside");
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .all(|node| node.props.enabled)
        );
        while driver.advance_time(1., 64).unwrap().pending {}
        settle(&mut driver);
        assert_eq!(
            driver.adapter().game_settings().title,
            "outside",
            "time alone finished a transition"
        );
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        settle(&mut driver);
        assert_eq!(driver.adapter().game_settings().title, "completed");
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        click_at(&mut driver, &mut bridge, 50., 30.);
        assert_eq!(driver.adapter().game_settings().title, "inside");
        driver.cancel().unwrap();
    }
}
