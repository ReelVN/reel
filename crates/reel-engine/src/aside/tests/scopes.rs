use super::{
    models::{drive, prepared},
    *,
};
use reel_ui::stage::NodeKind;

#[test]
fn scope_nodes_share_mount_admission_and_retire_only_registered_subtrees() {
    let code = r#"
    fn retired(node) {
        try { node.x=1; } catch error { return true; }
        false
    }
    fn opening() {
        let root=stack(); stage.replace(root);
        let permanent=root.add(text("permanent"));
        let holders={outer:null,inner:null,failed:null};
        scope(body: |s| {
            holders.outer=s.add(root,stack(),index:0);
            let ordinary=root.add(text("ordinary add"));
            scope(body: |inner| {
                holders.inner=inner.add(holders.outer,text("nested"));
            });
            if !retired(holders.inner) || retired(holders.outer) { throw "nested ownership"; }
            let mut rejected=0;
            try { s.add(root,permanent); } catch error { rejected+=1; }
            holders.failed=text("failed mount");
            try { s.add(root,holders.failed,index:100); } catch error { rejected+=1; }
            if rejected!=2 { throw "invalid mount succeeded"; }
            root.add(holders.failed);
            root.move(holders.outer,index:3);
            s.add(holders.outer,text("full subtree"));
        });
        if !retired(holders.outer) || retired(permanent) || retired(holders.failed) {
            throw "scope changed ordinary ownership";
        }
        let original={reason:"body error"}; let mut caught=false;
        try { scope(body: |s| {
            holders.outer=s.add(root,text("fault page")); throw original;
        }); } catch error { caught=error==original; }
        if !caught || !retired(holders.outer) { throw "fault cleanup"; }
        scope(body: |s| {
            let page=s.add(root,stack()); s.add(page,text("early removal")); page.remove();
        });
        scope(body: |s| {
            s.add(root,text("old stage")); stage.replace(stack() { text("new stage"); });
        });
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        assert!(vm.scopes.live.is_empty());
        assert_eq!(vm.stage().nodes().count(), 2);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("new stage".into()))
        );
    }
}

#[test]
fn scope_nodes_restore_without_rebuilding_and_cancel_node_owned_work() {
    let code = r#"fn opening() {
        let root=stack(); stage.replace(root);
        let entered=completion();
        let state={created:0,page:null,child:null,motion:null,presented:null};
        let job=spawn(|| scope(body: |s| {
            state.created+=1;
            state.page=s.add(root,stack() { text("temporary page"); });
            state.child=spawn(|| await completion(),owner:state.page);
            state.motion=run(tween(state.page,x:100,duration:10),owner:state.page);
            state.presented=stage.presented(owner:state.page);
            entered.resolve();
            await completion();
        }));
        await entered; narrator("save page");
        job.cancel();
        let mut cancelled=0;
        for work in [job,state.child,state.motion,state.presented] {
            try { await work; } catch error { cancelled+=1; }
        }
        if cancelled!=4 || state.created!=1 { throw "restored cleanup replayed work"; }
        root.add(text("page retired"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        let (mut session, mut vm) = restored(&mut vm, &session);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("temporary page".into()))
        );
        let interaction = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
                .unwrap();
        }
        drive(&mut vm, &mut session, true);
        assert!(vm.scopes.live.is_empty());
        assert!(vm.tasks.is_empty());
        assert!(!vm.activities.has_active());
        assert_eq!(vm.stage().nodes().count(), 2);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("page retired".into()))
        );
    }
}

#[test]
fn scope_nodes_retirement_reuses_parent_edges_without_vm_chunk_allocation() {
    let code = r#"fn opening() {
        let root=stack(); stage.replace(root);
        root.add(text("before"));
        scope(body: |s| {
            let mut i=0;
            while i<256 { s.add(root,stack()); i+=1; }
            root.add(text("after"));
            narrator("retire");
        });
    }"#;
    let (mut session, mut vm) = prepared(code, true);
    drive(&mut vm, &mut session, false);
    let interaction = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
            .unwrap();
    }
    let mut measured = false;
    for _ in 0..1000 {
        match vm.step(&mut session, SliceBudget::default(), || 0).unwrap() {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                let package = vm.vm.program().package();
                let closing = package.symbols
                    [package.natives[ticket.native.index()].symbol.index()]
                    == "reel::scope"
                    && ticket.input == NativeInput::CallbackReturned;
                let before = vm.vm.heap().stats().chunk_allocations;
                vm.dispatch(&mut session, ticket, "en", 32).unwrap();
                if closing {
                    assert_eq!(
                        vm.vm.heap().stats().chunk_allocations,
                        before,
                        "retirement allocated replacement child arrays"
                    );
                    measured = true;
                    break;
                }
            }
            SessionStep::Vm(RunOutcome::BudgetYield(_)) | SessionStep::Progress => {}
            other => panic!("scope did not return: {other:?}"),
        }
    }
    assert!(measured);
    drive(&mut vm, &mut session, true);
    let stage = vm.stage();
    let children = &stage.node(stage.root().unwrap()).unwrap().children;
    assert_eq!(children.len(), 2);
    assert_eq!(
        stage.node(children[0]).unwrap().kind,
        NodeKind::Text("before".into())
    );
    assert_eq!(
        stage.node(children[1]).unwrap().kind,
        NodeKind::Text("after".into())
    );
}

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
        let snow=particles("flake.png"); stage.replace(snow);
        let handles={particles:null,clock:null,presented:null,geometry:null};
        scope(body: || {
            handles.particles=emit(snow,burst:0,rate:10,lifetime:1);
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
