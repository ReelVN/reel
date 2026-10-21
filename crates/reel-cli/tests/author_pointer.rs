//! Source and actual ASB exercise the example through accepted UI input.
//! Host completion is covered by the engine contract checks; this has no native window.
#[path = "support/author.rs"]
#[allow(dead_code)]
mod support;
use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};
use reel_player::aside_session::AsideSessionUi;
use reel_ui::stage::NodeKind;
use support::*;

#[test]
fn pointer_example_source_and_asb_handle_refusal_and_destination_input() {
    let checked = reel_cli::check(
        include_str!("../../../examples/author_pointer/src/main.aside"),
        "author_pointer.aside",
    )
    .unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_cli::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        click_at(&mut driver, &mut bridge, 30., 110.);
        assert!(driver.adapter().stage().nodes().any(|node| node.kind
            == NodeKind::Text(
                "Pointer movement is unavailable for the current input or host.".into()
            )));
        present(&mut driver, &mut bridge, root, 2);
        click_at(&mut driver, &mut bridge, 440., 120.);
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("Destination clicked.".into()))
        );
        driver.cancel().unwrap();
    }
}
