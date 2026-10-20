use super::*;
use aside_vm::snapshot::SnapshotBudget;
use reel_ui::stage::NodeKind;

#[test]
fn text_spacing_public_example_updates_from_source_and_asb() {
    let code = include_str!("../../../../../examples/author_text_spacing.aside")
        .replace("fn main()", "fn opening()");
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(&code, encoded);
        models::drive(&mut vm, &mut session, true);
        let title = vm
            .stage()
            .nodes()
            .find(
                |node| matches!(&node.kind, NodeKind::Text(text) if *text == "CHAPTER ONE".into()),
            )
            .unwrap()
            .id;
        let tighter = vm
            .stage()
            .nodes()
            .find(|node| matches!(&node.kind, NodeKind::Button { label: Some(label), .. } if label == "Tighter"))
            .unwrap()
            .id;
        assert_eq!(vm.stage().node(title).unwrap().props.letter_spacing, 2.);
        assert!(vm.click(&mut session, tighter).unwrap());
        models::drive(&mut vm, &mut session, true);
        assert_eq!(vm.stage().node(title).unwrap().props.letter_spacing, 1.5);
    }
}

#[test]
fn text_spacing_validates_bindings_and_restores_source_asb() {
    let code = r#"
    use std::math::{sqrt, exp};
    fn opening() {
        let state = model({letter: -2, word: 6});
        let defaulted = text("default");
        if defaulted.letter_spacing != 0 || defaulted.word_spacing != 0 { throw "defaults"; }
        let direct = text("direct", letter_spacing: -2, word_spacing: 6);
        let mut rejected = 0;
        for invalid in [null, true, "wide", 1e300, -1e300, sqrt(-1), exp(1000)] {
            try { direct.letter_spacing = invalid; } catch e { rejected += 1; }
            try { direct.word_spacing = invalid; } catch e { rejected += 1; }
            try { let _ = text("invalid", letter_spacing: invalid); } catch e { rejected += 1; }
            try { let _ = text("invalid", word_spacing: invalid); } catch e { rejected += 1; }
        }
        let bound = text("bound", letter_spacing: bind(state.letter), word_spacing: bind(state.word));
        let derived = text("derived", letter_spacing: derive(|| state.letter), word_spacing: derive(|| state.word));
        try { state.letter = 1e300; } catch e { rejected += 1; }
        try { state.word = null; } catch e { rejected += 1; }
        if rejected != 30 || state.letter != -2 || state.word != 6 ||
            direct.letter_spacing != -2 || direct.word_spacing != 6 { throw "validation changed values"; }
        stage.replace(column() {
            defaulted; direct; bound; derived;
            text_input(value: "A A", letter_spacing: bind(state.letter), word_spacing: bind(state.word));
            button("change", letter_spacing: derive(|| state.letter), word_spacing: derive(|| state.word),
                on_click: |event| {
                    state.letter = if state.letter == -2 { 3.25 } else { -2 };
                    state.word = if state.word == 6 { -4.5 } else { 6 };
                    direct.letter_spacing = state.letter;
                    direct.word_spacing = state.word;
                });
        });
        narrator("ready");
    }
    "#;
    let assert_spacing = |vm: &SessionVm, expected: (f64, f64)| {
        for node in vm.stage().nodes() {
            let expected = match &node.kind {
                NodeKind::Text(text) if *text == "default".into() => (0., 0.),
                NodeKind::Text(_) | NodeKind::TextInput { .. } | NodeKind::Button { .. } => {
                    expected
                }
                _ => continue,
            };
            assert_eq!(
                (node.props.letter_spacing, node.props.word_spacing),
                expected,
                "{:?}",
                node.kind
            );
        }
    };
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        assert_spacing(&vm, (-2., 6.));
        let program = vm.vm.program().clone();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let bytes = capture.finish().unwrap().encode().unwrap();
        let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            program,
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(41),
                epoch: Epoch(20),
            },
            crate::ReadProgress::default(),
            None,
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut restored_session, mut restored) = restore.finish().unwrap();
        for (session, vm) in [
            (&mut session, &mut vm),
            (&mut restored_session, &mut restored),
        ] {
            models::drive(vm, session, true);
            assert_spacing(vm, (-2., 6.));
            let button = vm
                .stage()
                .nodes()
                .find(|node| matches!(&node.kind, NodeKind::Button { .. }))
                .unwrap()
                .id;
            for expected in [(3.25, -4.5), (-2., 6.)] {
                assert!(vm.click(session, button).unwrap());
                models::drive(vm, session, true);
                assert_spacing(vm, expected);
            }
        }
    }
}
