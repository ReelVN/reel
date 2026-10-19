use super::{
    models::{drive, prepared},
    *,
};
use reel_ui::stage::NodeKind;

#[test]
fn owned_script_tasks_share_results_faults_and_cancellation_from_source_and_asb() {
    let code = r#"
    class Worker {
        let gate;
        let shared;
        fn init(self, gate, shared) { self.gate = gate; self.shared = shared; }
        fn work(self) { await self.gate; self.shared.ran += 1; self.shared }
    }
    fn opening() {
        let root = stack(); stage.replace(root);
        let data = {ran: 0, received: 0, cancelled_finally: false};
        let gate = completion();
        let worker = Worker(gate, data);
        let task = spawn(worker.work, owner: root);
        if data.ran != 0 { throw "spawn must return before running work"; }
        let waiter = spawn(|| { await task; throw "cancelled waiter resumed"; });
        let first = spawn(|| { if await task != data { throw "result identity"; } data.received += 1; });
        let second = spawn(|| { if await task != data { throw "shared result"; } data.received += 1; });
        if !waiter.cancel() || waiter.cancel() { throw "cancel request"; }
        let mut caught = 0;
        try { await waiter; } catch error { caught += 1; }
        gate.resolve();
        await first; await second;
        if await task != data || data.ran != 1 || data.received != 2 || task.cancel() { throw "terminal result"; }
        let error = {message: "original"};
        let failed = spawn(|| { throw error; });
        for i in 0..2 { try { await failed; } catch e { if e != error { throw "fault identity"; } caught += 1; } }
        let started = completion(); let blocked = completion();
        let waiting = spawn(|| {
            started.resolve();
            try { await blocked; } finally { data.cancelled_finally = true; }
        });
        await started;
        waiting.cancel();
        try { await waiting; } catch error { caught += 1; }
        if data.cancelled_finally { throw "hard cancellation ran script"; }
        blocked.resolve();
        let self_task = {value: null};
        self_task.value = spawn(|| { self_task.value.cancel(); throw "self cancel resumed"; });
        try { await self_task.value; } catch error { caught += 1; }
        let removed = spawn(|| { throw "retired owner started"; }, owner: root);
        root.remove();
        try { await removed; } catch error { caught += 1; }
        let forbidden = spawn(|| narrator("not the main story"));
        try { await forbidden; } catch error { caught += 1; }
        if caught != 7 { throw "missing task terminal"; }
        let owner = stack(); stage.replace(owner);
        let entered = completion();
        let child = {value: null};
        let parent = spawn(|| {
            child.value = spawn(|| { entered.resolve(); await completion(); });
        }, owner: owner);
        await parent; await entered;
        owner.visible = false;
        // Hidden nodes retain work, and a returned parent does not cancel children.
        if !child.value.cancel() { throw "child lifetime"; }
        try { await child.value; } catch error {}
        let ready = completion();
        let attached = spawn(|| { ready.resolve(); await completion(); }, owner: owner);
        await ready;
        owner.remove();
        try { await attached; throw "removed task returned"; } catch error {}
        stage.replace(text("tasks completed"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        collect(&mut vm);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("tasks completed".into()))
        );
        assert!(vm.tasks.is_empty());
        assert!(!vm.has_runnable_tasks());
        vm.revoke(&mut session).unwrap();
        while vm.cleanup_pending() {
            vm.cleanup_step(&mut session, 1, SliceBudget::default(), || 0)
                .unwrap();
        }
    }
}

#[test]
fn session_shutdown_releases_waiting_script_task_roots() {
    let code = r#"fn opening() {
        let started = completion();
        spawn(|| { started.resolve(); await completion(); });
        await started;
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, true);
        assert_eq!(vm.tasks.len(), 1);
        vm.revoke(&mut session).unwrap();
        for _ in 0..100_000 {
            if !vm.cleanup_pending() {
                break;
            }
            vm.cleanup_step(&mut session, 1, SliceBudget::default(), || 0)
                .unwrap();
        }
        assert!(!vm.cleanup_pending());
        assert!(vm.tasks.is_empty());
        collect(&mut vm);
        assert_eq!(vm.vm.heap().stats().roots, 0);
    }
}

#[test]
fn task_fault_keeps_original_source_as_a_cause_after_gc_and_await() {
    let code = "fn broken() { throw \"original failure\"; }\nfn opening() { let task = spawn(broken); await task; }";
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        loop {
            match next(&mut vm, &mut session) {
                RunOutcome::HostCall(ticket) => {
                    vm.dispatch(&mut session, ticket, "en", 32).unwrap();
                }
                RunOutcome::Fault(delivery) => {
                    let info = vm.vm.delivery_fault(delivery).unwrap().unwrap();
                    assert_eq!(info.kind, FaultKind::Thrown);
                    let heap = vm.vm.heap();
                    let cause = heap
                        .fault_causes(info.id)
                        .unwrap()
                        .next(heap)
                        .unwrap()
                        .unwrap();
                    let original = heap.fault_info(cause.fault.unwrap()).unwrap();
                    let package = vm.vm.program().package();
                    assert_eq!(
                        package.symbols[package.functions[original.location.code.function.index()]
                            .name
                            .index()],
                        "broken"
                    );
                    vm.discard_delivery(&mut session).unwrap();
                    break;
                }
                _ => {}
            }
            collect(&mut vm);
        }
        assert!(vm.tasks.is_empty());
    }
}

#[test]
fn completed_awaits_yield_to_other_tasks_even_on_a_slow_host() {
    for encoded in [false, true] {
        for instructions in [1, 1000] {
            for wait in ["ready", "run(delay(0.004), owner: root)"] {
                let code = format!(
                    r#"fn opening() {{
                    let root = stack(); stage.replace(root);
                    let ready = completion(); ready.resolve();
                    spawn(|| {{ loop {{ await {wait}; root.x += 1; }} }}, owner: root);
                    await run(delay(2), owner: root);
                    if root.x == 0 {{ throw "worker never ran"; }}
                    root.remove(); stage.replace(text("continued"));
                }}"#
                );
                let (mut session, mut vm) = prepared(&code, encoded);
                let main = vm.task.unwrap();
                let mut ready_awaits = 0;
                let mut finished = false;
                for step in 0..2000 {
                    // Each tiny VM/native slice costs a full 16 ms host frame.
                    // Short activities therefore finish before their await is entered.
                    vm.advance_time(&session, step as f64 * 0.016, 64).unwrap();
                    match vm
                        .step(
                            &mut session,
                            SliceBudget {
                                instructions,
                                ..Default::default()
                            },
                            || 0,
                        )
                        .unwrap()
                    {
                        SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                            vm.dispatch(&mut session, ticket, "en", 32).unwrap();
                            if ticket.task != main
                                && ticket.await_activation.is_some()
                                && !vm.vm.wait_is_pending(ticket.wait_id())
                            {
                                ready_awaits += 1;
                            }
                        }
                        SessionStep::Vm(RunOutcome::Returned(_)) => {
                            vm.discard_delivery(&mut session).unwrap();
                            finished = true;
                            break;
                        }
                        SessionStep::Vm(RunOutcome::Fault(id)) => {
                            panic!("{:?}", vm.vm.delivery_fault(id))
                        }
                        SessionStep::Vm(RunOutcome::ResourceBlocked(e)) => panic!("{e:?}"),
                        _ => {}
                    }
                }
                assert!(
                    finished,
                    "ready await starved main: {wait}, instructions={instructions}"
                );
                assert!(
                    ready_awaits > 0,
                    "exercise a completed await, not only a pending wait"
                );
                drive(&mut vm, &mut session, false);
                assert!(
                    vm.stage()
                        .nodes()
                        .any(|n| n.kind == NodeKind::Text("continued".into()))
                );
                assert!(vm.tasks.is_empty(), "owner retirement cleans up the loop");
            }
        }
    }
}

#[test]
fn ordinary_wait_capable_calls_and_budget_slices_keep_the_sync_segment() {
    let code = r#"fn opening() {
        let state = {value: 0};
        let worker = spawn(|| { if state.value != 20 { throw "interrupted synchronous segment"; } state.value = 21; });
        for i in 0..20 { run(delay(0)); state.value += 1; }
        if state.value != 20 { throw "ordinary WAIT native yielded"; }
        await worker;
        if state.value != 21 { throw "await did not schedule worker"; }
        stage.replace(text("atomic"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("atomic".into()))
        );
    }
}
