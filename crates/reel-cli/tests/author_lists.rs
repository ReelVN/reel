//! Real source/ASB, accepted geometry and portable input for keyed author UI.
#[path = "support/author.rs"]
mod support;
use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};
use reel_player::aside_session::AsideSessionUi;
use reel_ui::{input::GamepadButton, stage::NodeKind};
use support::*;

#[test]
fn source_and_asb_reorder_preserves_focus_and_reads_the_replacement_record() {
    let source = r#"
    fn main() {
        game.configure(default_dialogue: false);
        let data = model({items: [{id: "a", name: "A"}, {id: "b", name: "B"}], clicked: 0, latest: ""});
        stage.replace(column(width: 600, height: 400) {
            list(|| data.items, key: |item| item.id, build: |row| {
                button(label: derive(|| row.value.name), width: 180, height: 50, on_click: |e| {
                    data.latest = row.value.name;
                    data.clicked += 1;
                    if data.clicked == 1 { data.items = [{id: "b", name: "Bee"}, {id: "a", name: "Ay"}, {id: "c", name: "C"}]; }
                    else if data.clicked == 2 { data.items.remove(row.index); }
                })
            });
            text(value: derive(|| f"{data.clicked}:{data.latest}"));
        });
        narrator("waiting");
    }"#;
    let checked = reel_cli::check(source, "list.aside").unwrap();
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
        let id = driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| matches!(&n.kind, NodeKind::Button {label: Some(s), ..} if &**s == "A"))
            .unwrap()
            .id;
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        click_at(&mut driver, &mut bridge, 30., 25.);
        let focused = bridge.root().focused_widget().unwrap();
        present(&mut driver, &mut bridge, root, 2);
        assert_eq!(bridge.root().focused_widget(), Some(focused));
        assert!(
            matches!(&driver.adapter().stage().node(id).unwrap().kind, NodeKind::Button { label: Some(s), .. } if &**s == "Ay")
        );
        gamepad_press(&mut driver, &mut bridge, GamepadButton::South);
        present(&mut driver, &mut bridge, root, 3);
        assert!(driver.adapter().stage().node(id).is_none());
        assert_ne!(bridge.root().focused_widget(), Some(focused));
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("2:Ay".into()))
        );
        assert!(driver.ui().session().view().interaction.is_some());
        driver.cancel().unwrap();
    }
}

#[test]
fn keyed_creator_examples_initialize_from_source_and_asb() {
    for (name, source) in [
        (
            "author_list.aside",
            include_str!("../../../examples/author_list.aside"),
        ),
        (
            "author_narrative.aside",
            include_str!("../../../examples/author_narrative.aside"),
        ),
        (
            "author_reading.aside",
            include_str!("../../../examples/author_reading.aside"),
        ),
    ] {
        let checked = reel_cli::check(source, name).unwrap();
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
            assert!(driver.adapter().stage().root().is_some(), "{name}");
            driver.cancel().unwrap();
        }
    }
}
