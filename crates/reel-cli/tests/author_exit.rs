//! Author shutdown uses the same cleanup path in source and encoded projects.
#[path = "support/author.rs"]
#[allow(dead_code)]
mod support;

use reel_engine::{
    Epoch, OwnerLifecycle, Session, SessionId,
    aside::{SliceBudget, VmLimits, source::compile_story},
};
use reel_player::{aside_session::AsideSessionUi, story_runtime::StoryRuntime};

#[test]
fn source_and_asb_exit_from_story_or_waiting_callback_retire_the_session() {
    for callback in [false, true] {
        let source = if callback {
            r#"fn main() {
                stage.replace(button("Exit", width: 200, height: 80, on_click: |event| {
                    await run(delay(0.5));
                    game.exit();
                    throw "callback continued after exit";
                }));
                await completion();
                throw "main continued after exit";
            }"#
        } else {
            r#"fn main() {
                run(repeat(delay(60)));
                game.exit();
                throw "main continued after exit";
            }"#
        };
        let checked = reel_cli::check(source, "main.aside").unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        for story in [
            compile_story(source, "main", 1, "main").unwrap(),
            reel_cli::decode_story(&artifact.bytes).unwrap(),
        ] {
            let mut driver = AsideSessionUi::from_compiled(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                story,
                "en",
                VmLimits::default(),
            )
            .unwrap();
            let owner = driver.ui().session().root_owner();
            if callback {
                support::settle(&mut driver);
                let (mut bridge, root) = support::bridge(&driver);
                support::present(&mut driver, &mut bridge, root, 1);
                support::click_at(&mut driver, &mut bridge, 50., 40.);
                assert!(!driver.adapter().exit_requested());
                driver.advance_time(1., 64).unwrap();
            }
            let mut runtime = StoryRuntime::new(driver);
            for _ in 0..10_000 {
                runtime
                    .tick(
                        SliceBudget {
                            instructions: 3,
                            ..Default::default()
                        },
                        || 1_000_000_000,
                    )
                    .unwrap();
                if runtime.close_complete() {
                    break;
                }
            }
            assert!(runtime.driver().adapter().exit_requested());
            assert!(runtime.close_complete());
            assert!(!runtime.needs_tick());
            assert!(runtime.driver().adapter().task().is_none());
            assert_eq!(
                runtime.driver().ui().session().owner_lifecycle(owner),
                Some(OwnerLifecycle::Retired)
            );
            assert!(runtime.driver().fault().unwrap().is_none());
        }
    }
}
