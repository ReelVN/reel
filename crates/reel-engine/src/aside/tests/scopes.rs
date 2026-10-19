use super::{
    models::{drive, prepared},
    *,
};
use reel_ui::stage::NodeKind;

fn restored(vm: &mut SessionVm, session: &Session) -> (Session, SessionVm) {
    use super::snapshot::{SessionSnapshot, SnapshotBudget};
    let bytecode = bc::encode(vm.vm.program().package(), bc::Limits::default()).unwrap();
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut image = SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
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
    restore.finish().unwrap()
}

#[test]
fn effect_scope_return_fault_and_task_cancel_retire_owned_work() {
    let code = r#"
    fn cancelled(work) {
        let mut caught = false;
        try { await work; } catch error { caught = true; }
        if !caught { throw "owned work survived scope"; }
    }
    fn opening() {
        let node = stack(); stage.replace(node);
        let handles = {motion:null, child:null, wait:null, explicit:null};
        let result = scope(body: |s| {
            handles.motion = s.run(tween(node, x:100, duration:10));
            handles.child = s.spawn(|| await completion());
            handles.wait = completion();
            handles.explicit = run(delay(10), owner:node);
            42
        });
        if result != 42 { throw "scope return"; }
        cancelled(handles.motion); cancelled(handles.child); cancelled(handles.wait);
        finish(handles.explicit);
        let original = {reason:"source error"};
        let mut caught = false;
        try { scope(body: || {
            handles.motion = run(tween(node, x:100, duration:10));
            throw original;
        }); } catch error { caught = error == original; }
        if !caught { throw "scope error identity"; }
        cancelled(handles.motion);
        let started = completion();
        let state = {cleanup:false};
        let worker = spawn(|| scope(body: || {
            handles.motion = run(tween(node, x:200, duration:10));
            handles.child = spawn(|| await completion());
            started.resolve();
            try { await completion(); } finally { state.cleanup = true; }
        }));
        await started; worker.cancel(); cancelled(worker);
        cancelled(handles.motion); cancelled(handles.child);
        if state.cleanup { throw "hard cancellation executed author finally"; }
        stage.replace(text("scopes completed"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("scopes completed".into()))
        );
        assert!(vm.scopes.live.is_empty());
        assert!(vm.tasks.is_empty());
        assert!(!vm.activities.has_active());
    }
}

#[test]
fn effect_scope_owner_retirement_preserves_outer_catch_and_finally() {
    let code = r#"
    fn opening() {
        let owner = stack(); stage.replace(owner);
        let state = {inner:false, outer:false, caught:0};
        let entered = completion();
        let remover = spawn(|| { await entered; owner.remove(); });
        try {
            scope(owner:owner, body: || {
                scope(body: || {
                    entered.resolve();
                    try { await completion(); }
                    catch error { state.inner = true; }
                    finally { state.inner = true; }
                });
                throw "cancelled body resumed";
            });
        } catch error {
            if error != "ScopeCancelled" { throw "scope cancellation"; }
            state.caught += 1;
        } finally { state.outer = true; }
        await remover;
        if state.inner || !state.outer || state.caught != 1 { throw "region unwind"; }
        stage.replace(text("outer continued"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("outer continued".into()))
        );
        assert!(vm.scopes.live.is_empty());
        assert!(vm.tasks.is_empty());
    }
}

#[test]
fn effect_scope_restore_waiting_or_during_callback_cleanup() {
    let code = r#"fn opening() {
        let owner = stack(); stage.replace(owner);
        let started = {value:0}; let child = {value:null};
        try { scope(owner:owner, body: || {
            started.value += 1;
            run(repeat(delay(1)));
            child.value = spawn(|| await completion());
            await completion();
        }); } catch error { if error != "ScopeCancelled" { throw error; } }
        let mut caught = false;
        try { await child.value; } catch error { caught = true; }
        if started.value != 1 || !caught { throw "scope restore replayed work"; }
        stage.replace(text("restored scope"));
    }"#;
    for cancel_before_capture in [false, true] {
        let (mut session, mut vm) = prepared(code, true);
        drive(&mut vm, &mut session, false);
        assert_eq!(vm.scopes.live.len(), 1);
        if cancel_before_capture {
            let scope = &vm.scopes.live[0];
            let task = scope.task;
            let wait = vm.main_wait.unwrap();
            assert!(vm.vm.cancel_host_callback(scope.task, scope.frame).unwrap());
            assert!(!vm.vm.wait_is_pending(wait));
            assert!(matches!(
                vm.vm
                    .complete(wait, aside_vm::executor::Completion::Ready(Value::NULL)),
                Err(VmError::InvalidWait)
            ));
            let RunOutcome::BudgetYield(usage) = vm
                .vm
                .run_slice(
                    task,
                    SliceBudget {
                        work: 1,
                        instructions: 1,
                        ..Default::default()
                    },
                    || 0,
                )
                .unwrap()
            else {
                panic!("callback cleanup must respect its slice budget");
            };
            assert!(usage.work <= 1);
        }
        let (mut session, mut vm) = restored(&mut vm, &session);
        if !cancel_before_capture {
            let scope = &vm.scopes.live[0];
            assert!(vm.vm.cancel_host_callback(scope.task, scope.frame).unwrap());
        }
        drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("restored scope".into()))
        );
        assert!(vm.scopes.live.is_empty());
        assert!(vm.tasks.is_empty());
        assert!(!vm.activities.has_active());
    }
}

#[test]
fn effect_scope_retains_caught_error_outside_the_callback() {
    let code = r#"
    fn interrupted() {
        let owner=stack(); stage.replace(owner);
        let entered=completion();
        spawn(|| { await entered; owner.remove(); });
        try { scope(owner:owner, body: || { entered.resolve(); await completion(); }); }
        catch error { if error!="ScopeCancelled" { throw error; } }
    }
    fn opening() {
        let original={reason:"outer error"};
        let mut caught=false;
        try {
            try { throw original; }
            catch error { interrupted(); throw; }
        } catch error { caught=error==original; }
        if !caught { throw "outer unwind was lost"; }
        stage.replace(text("outer unwind retained"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("outer unwind retained".into()))
        );
        assert!(vm.scopes.live.is_empty());
    }
}

#[test]
fn effect_scope_cancels_story_wait_and_allows_following_dialogue() {
    for wait in [
        "narrator(\"inside\")",
        "choose([{key:\"one\",label:\"inside\"}])",
    ] {
        let code = format!(
            r#"fn opening() {{
            let owner=stack(); stage.replace(owner);
            spawn(|| {{ await run(delay(1)); owner.remove(); }});
            try {{ scope(owner:owner, body: || {{ {wait}; throw "old input resumed"; }}); }}
            catch error {{ if error != "ScopeCancelled" {{ throw error; }} }}
            narrator("after scope");
        }}"#
        );
        let (mut session, mut vm) = prepared(&code, true);
        drive(&mut vm, &mut session, false);
        let old = session.view().interaction.unwrap().id();
        let (mut session, mut vm) = restored(&mut vm, &session);
        super::activities::advance_clock(&mut vm, &session, 2., 1);
        drive(&mut vm, &mut session, false);
        let interaction = session.view().interaction.unwrap();
        assert!(
            matches!(interaction, crate::InteractionView::Dialogue(d) if d.text() == "after scope")
        );
        assert_ne!(interaction.id(), old);
        assert!(vm.scopes.live.is_empty());
    }
}

#[test]
fn effect_scope_cancels_particles_clock_and_presentation_requests() {
    let code = r#"fn opening() {
        let snow=particles("flake.png",rate:10,lifetime:1); stage.replace(snow);
        let handles={particles:null,clock:null,presented:null,geometry:null};
        scope(body: || {
            handles.particles=emit(snow);
            handles.clock=clock(interval:1);
            handles.presented=stage.presented();
            handles.geometry=stage.geometry(snow);
        });
        let mut cancelled=0;
        for result in [handles.particles,handles.clock,handles.presented,handles.geometry] {
            try { await result; } catch error { cancelled+=1; }
        }
        if cancelled!=4 { throw "scope producer survived"; }
        stage.replace(text("producers cancelled"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        assert!(!vm.particles.has_active());
        assert!(!vm.activities.has_active());
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("producers cancelled".into()))
        );
    }
}
