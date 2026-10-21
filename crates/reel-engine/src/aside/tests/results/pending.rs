use super::*;

fn has_text(vm: &SessionVm, text: &str) -> bool {
    vm.stage()
        .nodes()
        .any(|n| n.kind == NodeKind::Text(text.into()))
}

#[test]
fn result_pending_registration_visits_existing_results_only_after_lifecycle_changes() {
    for encoded in [false, true] {
        for count in [64, 256, 1024] {
            let code = r#"fn opening() {
                let root=column(); stage.replace(root);
                let owner=root.add(stack());
                let requests=[];
                for i in 0..COUNT {requests.append(completion(owner:owner));}
                root.add(button("live",on_click:|| {
                    for request in requests {
                        if !request.pending {throw "live request ended";}
                    }
                }));
                root.add(button("remove",on_click:owner.remove));
                root.add(button("retired",on_click:|| {
                    for request in requests {
                        if request.pending {throw "owner did not cancel request";}
                    }
                    root.add(text("all cancelled"));
                }));
            }"#
            .replace("COUNT", &count.to_string());
            let (mut session, mut vm) = prepared(&code, encoded);
            drive(&mut vm, &mut session, false);
            let creation = vm.results.owned_inspections;
            eprintln!("completion registration: encoded={encoded} count={count} visits={creation}");
            assert_eq!(creation, 0, "registration only appends its own weak record");
            collect(&mut vm);
            drive(&mut vm, &mut session, false);
            let after_gc = vm.results.owned_inspections;
            assert_eq!(
                after_gc - creation,
                count,
                "GC completion visits each live result once"
            );
            click(&mut vm, &mut session, "live");
            assert_eq!(vm.results.owned_inspections, after_gc);
            click(&mut vm, &mut session, "remove");
            assert_eq!(vm.results.owned_inspections - after_gc, count);
            click(&mut vm, &mut session, "retired");
            assert!(has_text(&vm, "all cancelled"));
            assert!(
                !vm.results
                    .needs_work(vm.stage.scene.revision(), vm.stage.retirement_revision)
            );
        }
    }
}

#[test]
fn result_pending_publishes_terminals_and_owner_exit_without_await() {
    let code = r#"fn dynamic(value) {value} fn opening() {
        let root=column(); stage.replace(root);
        let owner=root.add(stack());
        let result=completion(owner:owner);
        root.add(text(derive(|| if result.pending {"pending"} else {"settled"})));
        root.add(button("finish",on_click: || {
            if !result.resolve(42) || result.pending || result.resolve(99) {
                throw "settlement did not publish";
            }
        },enabled:bind(result.pending)));
        root.add(button("remove",on_click:owner.remove));
        root.add(button("readonly",on_click: || {
            let mut rejected=false;
            try {dynamic(result).pending=false;} catch error {rejected=true;}
            if !rejected {throw "pending is writable";}
        }));
    }"#;
    for encoded in [false, true] {
        for finish in [false, true] {
            let (mut session, mut vm) = prepared(code, encoded);
            drive(&mut vm, &mut session, true);
            assert!(has_text(&vm, "pending"));
            click(&mut vm, &mut session, "readonly");
            click(
                &mut vm,
                &mut session,
                if finish { "finish" } else { "remove" },
            );
            assert!(has_text(&vm, "settled"));
            assert!(
                !vm.stage()
                    .node(button(&vm, "finish"))
                    .unwrap()
                    .props
                    .enabled
            );
        }
    }
}

#[test]
fn result_pending_keeps_shared_producer_and_reports_task_faults() {
    let code = r#"fn opening() {
        let gate=completion();
        let waiter=spawn(|| await gate);
        if !waiter.pending {throw "task started settled";}
        waiter.cancel();
        try {await waiter;} catch error {}
        if waiter.pending || !gate.pending {throw "waiter cancelled shared producer";}
        gate.reject("reason");
        if gate.pending {throw "rejection not published";}
        try {await gate;} catch error {if error!="reason" {throw error;}}
        let failed=spawn(|| {throw "task reason";});
        try {await failed;} catch error {if error!="task reason" {throw error;}}
        if failed.pending {throw "fault not published";}
        let done=spawn(|| 7);
        if await done!=7 || done.pending {throw "return not published";}
        let cancelled=completion(); cancelled.cancel();
        if cancelled.pending {throw "explicit cancellation not published";}
        stage.replace(text("done"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        assert!(has_text(&vm, "done"));
    }
}

#[test]
fn result_pending_restores_scoped_modal_and_reopens_after_caller_cancel() {
    use aside_vm::snapshot::SnapshotBudget;
    let code = r#"
    fn ask(root,state) {
        if state.request!=null && state.request.pending {return null;}
        scope(owner:root,body:|s| {
            let result=completion();
            state.request=result;
            s.add(root,button("answer",on_click:|| result.resolve(42)));
            await result
        })
    }
    fn opening() {
        let root=column(); stage.replace(root);
        let state=model({request:null,job:null,answered:0});
        root.add(text(derive(|| if state.request!=null && state.request.pending {
            "asking"
        } else {"idle"})));
        root.add(button("open",on_click:|| {
            state.job=spawn(|| {
                if ask(root,state)==42 {state.answered+=1;}
            });
        }));
        root.add(button("cancel",on_click:|| state.job.cancel()));
        root.add(text(derive(|| f"answered {state.answered}")));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        click(&mut vm, &mut session, "open");
        assert!(has_text(&vm, "asking"));
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
                epoch: Epoch(19),
            },
            crate::ReadProgress::default(),
            None,
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut session, mut vm) = restore.finish().unwrap();
        drive(&mut vm, &mut session, false);
        assert!(has_text(&vm, "asking"));
        click(&mut vm, &mut session, "cancel");
        assert!(has_text(&vm, "idle"));
        assert!(!vm.stage().nodes().any(
            |n| matches!(&n.kind, NodeKind::Button {label:Some(label),..} if label=="answer")
        ));
        click(&mut vm, &mut session, "open");
        assert!(has_text(&vm, "asking"));
        click(&mut vm, &mut session, "answer");
        assert!(has_text(&vm, "idle"));
        assert!(has_text(&vm, "answered 1"));
        assert!(vm.scopes.live.is_empty());
    }
}

#[test]
fn result_pending_weak_registration_releases_unobserved_results_and_idle_work() {
    let code = r#"fn opening() {
        let root=stack(); stage.replace(root);
        for i in 0..2048 {completion(owner:stack());}
        root.add(button("change",on_click:|| {root.x+=1;}));
    }"#;
    let (mut session, mut vm) = prepared(code, true);
    drive(&mut vm, &mut session, false);
    let before = vm.vm.heap().stats().roots;
    vm.begin_collection().unwrap();
    // Interleave owner maintenance with small marking slices. Inspecting the
    // weak registrations must not shade and resurrect abandoned results.
    while vm.vm.heap().phase() != aside_vm::GcPhase::Idle {
        vm.results.owners_changed();
        drive(&mut vm, &mut session, false);
        vm.gc_step(GcBudget {
            units: 64,
            ..Default::default()
        })
        .unwrap();
    }
    drive(&mut vm, &mut session, false);
    collect(&mut vm);
    let after = vm.vm.heap().stats().roots;
    assert_eq!(before - after, 2048, "weak registrations retire after GC");
    for revision in 0..1000 {
        assert!(
            !vm.results
                .needs_work(revision, vm.stage.retirement_revision)
        );
    }
    click(&mut vm, &mut session, "change");
    assert!(
        !vm.results
            .needs_work(vm.stage.scene.revision(), vm.stage.retirement_revision)
    );
    eprintln!("2048 discarded completions: roots {before} -> {after}; idle work false");
}
