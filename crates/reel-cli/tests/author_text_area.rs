//! Source and actual ASB use the same editor, accepted input and model commits.
#[path = "support/author.rs"]
#[allow(dead_code)]
mod support;
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{VmLimits, source::compile_story},
};
use reel_player::aside_session::AsideSessionUi;
use reel_ui::{core::*, stage::NodeKind};
use support::*;

const SOURCE: &str = r#"
fn main() {
    let notes = model({body: "first\nsecond", changes: 0, submits: 0});
    let editor = text_area(value: bind(notes.body), width: 240, height: 80,
        on_change: |event| {
            if event.value != notes.body || event.target.value != notes.body { throw "commit order"; }
            notes.changes += 1;
        }, on_submit: || { notes.submits += 1; });
    let mut rejected = 0;
    try { let _ = text_area(value: derive(|| notes.body)); } catch e { rejected += 1; }
    try { editor.value = 1; } catch e { rejected += 1; }
    try { let _ = text_input(value: "two\nlines"); } catch e { rejected += 1; }
    if rejected != 3 { throw "input contract"; }
    stage.replace(column() {
        editor;
        text(derive(|| f"{notes.changes}:{notes.submits}"));
        button("Replace", on_click: || { notes.body = "author\nreplacement"; });
    });
    narrator("Editor input must leave this dialogue waiting.");
}
"#;

fn drivers(source: &str) -> Vec<AsideSessionUi> {
    let checked = reel_cli::check(source, "text_area.aside").unwrap();
    let bytes = reel_cli::build(&checked, Some("main::main")).unwrap().bytes;
    [
        compile_story(source, "main", 1, "main").unwrap(),
        reel_cli::decode_story(&bytes).unwrap(),
    ]
    .into_iter()
    .map(|story| {
        let mut d = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut d);
        d
    })
    .collect()
}
fn key(key: keyboard::Key, modifiers: keyboard::Modifiers) -> TextEvent {
    TextEvent::Keyboard(keyboard::KeyboardEvent {
        state: keyboard::KeyState::Down,
        key,
        code: keyboard::Code::Unidentified,
        location: keyboard::Location::Standard,
        modifiers,
        repeat: false,
        is_composing: false,
    })
}
fn state(d: &AsideSessionUi) -> (String, String) {
    let nodes = d.adapter().stage();
    let value = nodes
        .nodes()
        .find_map(|node| match &node.kind {
            NodeKind::TextInput {
                value,
                multiline: true,
            } => Some(value.to_string()),
            _ => None,
        })
        .unwrap();
    let counts = nodes
        .nodes()
        .find_map(|node| match &node.kind {
            NodeKind::Text(value) => Some(value.to_string()),
            _ => None,
        })
        .unwrap();
    (value, counts)
}

#[test]
fn text_area_source_asb_edit_submit_preedit_cancel_and_navigation() {
    use keyboard::{Key, Modifiers, NamedKey};
    let command = if cfg!(target_os = "macos") {
        Modifiers::META
    } else {
        Modifiers::CONTROL
    };
    for mut d in drivers(SOURCE) {
        let font = include_bytes!("../../reel-player/tests/fixtures/font-weight/400.ttf").to_vec();
        let (mut bridge, root) = bridge_with_font(&d, Some(font.into()));
        present(&mut d, &mut bridge, root, 1);
        let interaction = d.ui().session().view().interaction.unwrap().id();
        let mut frame = 1;
        let mut send =
            |d: &mut AsideSessionUi, bridge: &mut reel_ui::bridge::PresentationBridge, e| {
                d.handle_text(bridge, e).unwrap();
                settle(d);
                frame += 1;
                present(d, bridge, root, frame);
                assert_eq!(
                    d.ui().session().view().interaction.unwrap().id(),
                    interaction
                );
            };
        send(
            &mut d,
            &mut bridge,
            key(Key::Named(NamedKey::Tab), Modifiers::empty()),
        );
        let focus = bridge.root().focused_widget();
        send(
            &mut d,
            &mut bridge,
            key(Key::Character("a".into()), command),
        );
        send(
            &mut d,
            &mut bridge,
            TextEvent::ClipboardPaste("one\r\ntwo\rthree".into()),
        );
        assert_eq!(state(&d), ("one\ntwo\nthree".into(), "1:0".into()));
        send(
            &mut d,
            &mut bridge,
            key(Key::Named(NamedKey::Enter), Modifiers::empty()),
        );
        send(
            &mut d,
            &mut bridge,
            TextEvent::Ime(Ime::Commit("four".into())),
        );
        assert_eq!(state(&d), ("one\ntwo\nthree\nfour".into(), "3:0".into()));
        assert_eq!(
            bridge.root().focused_widget(),
            focus,
            "value echo keeps editor identity"
        );
        send(
            &mut d,
            &mut bridge,
            key(Key::Named(NamedKey::Enter), command),
        );
        assert_eq!(state(&d).1, "3:1");

        // Home/End operate on the current visual line; selection crosses line breaks.
        send(
            &mut d,
            &mut bridge,
            key(Key::Named(NamedKey::Home), Modifiers::empty()),
        );
        send(
            &mut d,
            &mut bridge,
            key(Key::Named(NamedKey::End), Modifiers::SHIFT),
        );
        send(
            &mut d,
            &mut bridge,
            TextEvent::Ime(Ime::Preedit("pending".into(), Some((7, 7)))),
        );
        send(
            &mut d,
            &mut bridge,
            key(Key::Named(NamedKey::Enter), Modifiers::empty()),
        );
        assert_eq!(state(&d), ("one\ntwo\nthree\nfour".into(), "3:1".into()));
        send(
            &mut d,
            &mut bridge,
            TextEvent::Ime(Ime::Preedit("".into(), None)),
        );
        send(
            &mut d,
            &mut bridge,
            TextEvent::Ime(Ime::Commit("last".into())),
        );
        assert_eq!(state(&d).0, "one\ntwo\nthree\nlast");

        send(
            &mut d,
            &mut bridge,
            key(Key::Named(NamedKey::Home), command),
        );
        send(
            &mut d,
            &mut bridge,
            key(Key::Named(NamedKey::ArrowDown), Modifiers::empty()),
        );
        send(
            &mut d,
            &mut bridge,
            key(Key::Named(NamedKey::End), Modifiers::SHIFT),
        );
        send(&mut d, &mut bridge, TextEvent::ClipboardPaste("TWO".into()));
        assert_eq!(state(&d).0, "one\nTWO\nthree\nlast");
        send(
            &mut d,
            &mut bridge,
            key(Key::Named(NamedKey::ArrowUp), Modifiers::empty()),
        );
        send(
            &mut d,
            &mut bridge,
            key(Key::Named(NamedKey::Home), Modifiers::SHIFT),
        );
        send(&mut d, &mut bridge, TextEvent::ClipboardPaste("ONE".into()));
        assert_eq!(state(&d).0, "ONE\nTWO\nthree\nlast");

        // Losing focus cancels an unfinished composition; Tab still reaches another control.
        send(
            &mut d,
            &mut bridge,
            TextEvent::Ime(Ime::Preedit("discard".into(), None)),
        );
        send(&mut d, &mut bridge, TextEvent::WindowFocusChange(false));
        send(&mut d, &mut bridge, TextEvent::WindowFocusChange(true));
        assert_eq!(state(&d), ("ONE\nTWO\nthree\nlast".into(), "6:1".into()));
        send(
            &mut d,
            &mut bridge,
            key(Key::Named(NamedKey::Tab), Modifiers::empty()),
        );
        assert_ne!(bridge.root().focused_widget(), focus);
        send(
            &mut d,
            &mut bridge,
            key(Key::Named(NamedKey::Enter), Modifiers::empty()),
        );
        let mut released = key(Key::Named(NamedKey::Enter), Modifiers::empty());
        if let TextEvent::Keyboard(event) = &mut released {
            event.state = keyboard::KeyState::Up;
        }
        send(&mut d, &mut bridge, released);
        assert_eq!(state(&d), ("author\nreplacement".into(), "6:1".into()));
        d.cancel().unwrap();
    }
}

#[test]
fn text_area_public_example_compiles_and_retains_business_state() {
    for mut d in drivers(include_str!("../../../examples/author_text_area.aside")) {
        let font = include_bytes!("../../reel-player/tests/fixtures/font-weight/400.ttf").to_vec();
        let (mut bridge, root) = bridge_with_font(&d, Some(font.into()));
        present(&mut d, &mut bridge, root, 1);
        click_at(&mut d, &mut bridge, 80., 325.);
        settle(&mut d);
        assert_eq!(state(&d).0, "The door opens.\nWhat happens next?");
        assert!(
            d.adapter()
                .stage()
                .nodes()
                .any(|n| matches!(&n.kind, NodeKind::Text(t) if *t == "Saved".into()))
        );
        d.cancel().unwrap();
    }
}
