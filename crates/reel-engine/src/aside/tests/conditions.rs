use super::*;
use reel_ui::stage::NodeKind;

fn click(vm: &mut SessionVm, session: &mut Session, label: &str) {
    let id = vm
        .stage()
        .nodes()
        .find(|n| matches!(&n.kind, NodeKind::Button { label: Some(s), .. } if s == label))
        .unwrap()
        .id;
    vm.click(session, id).unwrap();
    models::drive(vm, session, false);
}
fn has_text(vm: &SessionVm, label: &str) -> bool {
    vm.stage()
        .nodes()
        .any(|n| n.kind == NodeKind::Text(label.into()))
}

#[test]
fn when_true_and_conditional_dependencies_resolve_without_input_consumption() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"fn opening(){
            let state=model({branch:false,a:false,b:false});
            stage.replace(column(){
                button("a",on_click:||{state.a=true;});
                button("branch",on_click:||{state.branch=true;});
                button("b",on_click:||{state.b=true;});
            });
            if !await when(||true) {throw "immediate";}
            let done=when(||if state.branch {state.a} else {state.b});
            if !done.pending {throw "premature";}
            if !await done {throw "result";}
            if done.pending || !await done {throw "repeat";}
            stage.replace(text("complete"));narrator("still requires input");
        }"#,
            encoded,
        );
        models::drive(&mut vm, &mut session, false);
        click(&mut vm, &mut session, "a");
        assert!(!has_text(&vm, "complete"));
        click(&mut vm, &mut session, "branch");
        assert!(has_text(&vm, "complete"));
        assert!(matches!(
            session.view().interaction,
            Some(crate::InteractionView::Dialogue(_))
        ));
    }
}

#[test]
fn when_cancel_and_owner_retirement_cancel_once() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"fn opening(){
            let owner=stack();stage.replace(owner);
            let done=when(||false,owner:owner);
            stage.replace(text("new"));
            let mut retired=false;try {await done;} catch error {retired=true;}
            if !retired {throw "owner remained";}
            if done.pending || done.cancel() {throw "cancel repeated";}
            let other=when(||false);if !other.cancel(){throw "cancel";}
            let mut cancelled=false;try {await other;} catch error {cancelled=true;}
            if !cancelled {throw "cancel remained";}
            stage.replace(text("cancelled"));
        }"#,
            encoded,
        );
        models::drive(&mut vm, &mut session, false);
        assert!(has_text(&vm, "cancelled"));
    }
}

#[test]
fn when_restores_pending_dependency_and_keeps_predicate_fault_value() {
    use aside_vm::snapshot::SnapshotBudget;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"fn opening(){
            let state=model({ready:false});
            stage.replace(button("ready",on_click:||{state.ready=true;}));
            let result=when(||state.ready);
            if !await result {throw "restored result";}
            let mut caught=false;
            let failure=when(||{throw "predicate origin";});
            try {await failure;} catch error {
                if error!="predicate origin" {throw "wrong failure";}
                caught=true;
            }
            if !caught {throw "missing predicate fault";}
            stage.replace(text("restored"));
        }"#,
            encoded,
        );
        models::drive(&mut vm, &mut session, false);
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let mut image =
            snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap())
                .unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            vm.vm.program().clone(),
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(41),
                epoch: Epoch(100),
            },
            Default::default(),
            None,
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut restored, mut resumed) = restore.finish().unwrap();
        click(&mut resumed, &mut restored, "ready");
        assert!(has_text(&resumed, "restored"));
        assert!(!has_text(&vm, "restored"));
    }
}

#[test]
fn when_invalid_initial_and_changed_predicates_fail_the_same_result() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"fn opening(){
            let initial=when(||12);
            let mut caught=false;
            try {await initial;} catch error {
                if f"{error}"!="when predicate must return bool" {throw "wrong initial failure";}
                caught=true;
            }
            if !caught || initial.pending {throw "initial remains pending";}
            let state=model({invalid:false});
            stage.replace(button("invalidate",on_click:||{state.invalid=true;}));
            let changed=when(||if state.invalid {12} else {false});
            caught=false;
            try {await changed;} catch error {
                if f"{error}"!="when predicate must return bool" {throw "wrong changed failure";}
                caught=true;
            }
            if !caught || changed.pending || changed.cancel() {throw "changed remains pending";}
            stage.replace(text("invalid settled"));
        }"#,
            encoded,
        );
        models::drive(&mut vm, &mut session, false);
        click(&mut vm, &mut session, "invalidate");
        assert!(has_text(&vm, "invalid settled"));
    }
}
