//! One source/ASB workflow covers aliasing, transactions and real control input.
#[path = "support/author.rs"]
mod support;
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{VmLimits, source::compile_story},
};
use reel_player::aside_session::AsideSessionUi;
use reel_ui::{core::PointerType, stage::NodeKind};
use support::*;

const SOURCE: &str = r##"
use reel::prelude::*;
use reel::prelude::bind as link;
fn main() {
    let source = slider(value: 40, max: 100, width: 220, height: 40);
    let peer = slider(value: bind(source.value), max: 80, y: 60, width: 220, height: 40);
    let fill = stack(width: bind(peer.value), height: 20, y: 120, background: "#00ff00");
    let enabled = toggle("Enabled", value: true, y: 160, width: 220, height: 40);
    let panel = text("panel", visible: bind(enabled.value), y: 220, height: 20);
    let root = stack(width: 640, height: 400, children: [source, peer, fill, enabled, panel]);
    stage.replace(root);
    let mut receivers = 0;
    let mut keys = 0;
    fn receiver() { receivers += 1; source }
    fn key() { keys += 1; "value" }
    let reference = link(receiver()[key()]);
    peer.value = reference;
    fill.width = 60;
    if source.value != 60 || peer.value != 60 || receivers != 1 || keys != 1 { throw "location or write-through"; }
    let mut failures = 0;
    try { source.value = 90; } catch e { failures += 1; }
    try { source.value = bind(peer.value); } catch e { failures += 1; }
    try { peer.value = bind(enabled.value); } catch e { failures += 1; }
    if failures != 3 || source.value != 60 || peer.value != 60 || fill.width != 60 { throw "atomic failure"; }
    if unbind(bind(peer.value)) != 60 { throw "unbind result"; }
    source.value = 70;
    if peer.value != 60 || fill.width != 60 { throw "unbind chain"; }
    peer.value = reference;
    source.value = 40;
    root.add(button("Retire source", y: 280, width: 220, height: 40, on_click: |event| {
        let old = peer.value;
        source.remove();
        if peer.value != old || fill.width != old { throw "retirement freeze"; }
        let mut rejected = false;
        try { peer.value = reference; } catch e { rejected = true; }
        if !rejected { throw "retired reference"; }
        peer.value = 25;
        if fill.width != 25 { throw "surviving chain"; }
    }));
    narrator("Bindings keep this scene synchronized without callbacks.");
}
"##;

#[test]
fn stable_bindings_drive_controls_from_source_and_asb() {
    let checked = reel_cli::check(SOURCE, "bindings.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        compile_story(SOURCE, "main", 1, "main").unwrap(),
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
        click_at(&mut driver, &mut bridge, 110., 80.);
        let values: Vec<_> = driver
            .adapter()
            .stage()
            .nodes()
            .filter_map(|n| match n.kind {
                NodeKind::Slider { value, .. } => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(values, [40., 40.]);
        // A changed peer updates the source and a third, non-control node even
        // with no on_change callback and no runnable story task.
        present(&mut driver, &mut bridge, root, 2);
        click_with_pointer(&mut driver, &mut bridge, 210., 80., PointerType::Touch);
        let stage = driver.adapter().stage();
        assert!(
            stage
                .nodes()
                .filter_map(|n| match n.kind {
                    NodeKind::Slider { value, .. } => Some(value),
                    _ => None,
                })
                .all(|v| v == 80.)
        );
        assert!(
            stage
                .nodes()
                .any(|n| n.props.y == 120. && n.props.width == Some(80.))
        );
        present(&mut driver, &mut bridge, root, 3);
        // The source slider permits 100, but its connected peer permits only 80.
        // A rejected user proposal must not fail the session or remain painted.
        click_at(&mut driver, &mut bridge, 210., 20.);
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .filter_map(|n| match n.kind {
                    NodeKind::Slider { value, .. } => Some(value),
                    _ => None,
                })
                .all(|v| v == 80.)
        );
        present(&mut driver, &mut bridge, root, 4);
        click_at(&mut driver, &mut bridge, 30., 180.);
        assert!(
            !driver
                .adapter()
                .stage()
                .nodes()
                .find(|n| n.kind == NodeKind::Text("panel".into()))
                .unwrap()
                .props
                .visible
        );
        present(&mut driver, &mut bridge, root, 5);
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .find(|n| n.kind == NodeKind::Text("panel".into()))
                .unwrap()
                .props
                .visible
        );
        present(&mut driver, &mut bridge, root, 6);
        click_at(&mut driver, &mut bridge, 30., 300.);
        driver.sync(&mut bridge, root).unwrap();
        assert_eq!(
            driver
                .adapter()
                .stage()
                .nodes()
                .filter(|n| matches!(n.kind, NodeKind::Slider { .. }))
                .count(),
            1
        );
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.props.y == 120. && n.props.width == Some(25.))
        );
        assert!(driver.ui().session().view().interaction.is_some());
    }
}

#[test]
fn shared_property_example_initializes_from_source_and_asb() {
    let source = include_str!("../../../examples/author_bindings.aside");
    let checked = reel_cli::check(source, "author_bindings.aside").unwrap();
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
        settle(&mut driver);
        assert_eq!(
            driver
                .adapter()
                .stage()
                .nodes()
                .filter(|n| matches!(n.kind, NodeKind::Slider { value: 100., .. }))
                .count(),
            2
        );
        assert!(driver.ui().session().view().interaction.is_some());
    }
}
