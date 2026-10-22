use super::*;
use aside_vm::snapshot::SnapshotBudget;
use reel_ui::stage::NodeKind;

#[test]
fn auto_condition_binding_replacement_and_snapshot_preserve_reading_policy() {
    let code = r#"
        let state = model({playing: true});
        let reading = game.reading();
        fn dynamic(value) {value}
        fn opening() {
            if !reading.auto_ready { throw "default condition"; }
            game.set_auto(true, delay: 2, condition: derive(|| !state.playing));
            let mut errors = 0;
            try { game.set_auto(false, delay: 4, condition: dynamic(5)); } catch e { errors += 1; }
            try { game.set_auto(false, delay: 4, condition: bind(reading.delay)); } catch e { errors += 1; }
            try { reading.auto_ready = true; } catch e { errors += 1; }
            if errors != 3 || !reading.auto || reading.delay != 2 { throw "invalid gate mutated policy"; }
            game.set_auto();
            stage.replace(column() {
                button("toggle", on_click: |e| { state.playing = !state.playing; });
                button("detach", on_click: |e| { game.set_auto(condition: true); });
            });
            narrator("voice line");
        }
    "#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        assert!(session.reading().automatic());
        assert!(!session.reading().auto_ready());
        let bytecode = bc::encode(vm.vm.program().package(), bc::Limits::default()).unwrap();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let bytes = capture.finish().unwrap().encode().unwrap();
        let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
        let program = bc::verify(
            bc::decode(&bytecode, bc::Limits::default()).unwrap(),
            bc::Limits::default(),
        )
        .unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            program,
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(41),
                epoch: Epoch(10),
            },
            crate::ReadProgress::default(),
            None,
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        (session, vm) = restore.finish().unwrap();
        models::drive(&mut vm, &mut session, false);
        assert!(!session.reading().auto_ready());
        let revision = session.reading().revision();
        let click = |vm: &mut SessionVm, session: &mut Session, label: &str| {
            let id = vm
                .stage()
                .nodes()
                .find(|n| matches!(&n.kind, NodeKind::Button {label: Some(s), ..} if s == label))
                .unwrap()
                .id;
            vm.click(session, id).unwrap();
            models::drive(vm, session, false);
        };
        click(&mut vm, &mut session, "toggle");
        assert!(session.reading().auto_ready());
        assert_eq!(session.reading().revision(), revision);
        click(&mut vm, &mut session, "toggle");
        assert!(!session.reading().auto_ready());
        click(&mut vm, &mut session, "detach");
        assert!(session.reading().auto_ready());
        click(&mut vm, &mut session, "toggle");
        click(&mut vm, &mut session, "toggle");
        assert!(
            session.reading().auto_ready(),
            "old dependency must be detached"
        );
        assert!(session.reading().automatic());
        assert_eq!(session.reading().delay(), std::time::Duration::from_secs(2));
    }
}
