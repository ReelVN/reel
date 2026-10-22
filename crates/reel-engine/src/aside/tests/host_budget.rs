use super::*;
use aside_vm::snapshot::SnapshotBudget;

fn wide_source(body: &str) -> String {
    let mut source = String::from("fn wide(n) {");
    for i in 0..180 {
        source.push_str(&format!("let v{i} = n + {i};"));
    }
    source.push_str("let result = if n == 0 { 0 } else { wide(n - 1) }; result");
    for i in 0..180 {
        source.push_str(&format!(" + v{i}"));
    }
    source.push_str("} fn opening() {");
    source.push_str(body);
    source.push('}');
    source
}

fn restore(session: &Session, vm: &mut SessionVm) -> (Session, SessionVm) {
    let program = vm.vm.program().clone();
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 64 }).unwrap().complete {}
    let mut image =
        snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(42),
            epoch: Epoch(10),
        },
        crate::ReadProgress::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 64 }).unwrap().complete {}
    restore.finish().unwrap()
}

#[test]
fn host_budget_retries_real_window_minima_for_main_spawn_and_derive_source_asb() {
    for (kind, body) in [
        ("main", "wide(85);"),
        ("spawn", "let work = spawn(|| wide(85)); await work;"),
        ("derive", "stage.replace(text(derive(|| f\"{wide(85)}\")));"),
    ] {
        for encoded in [false, true] {
            let (mut session, mut vm) = models::prepared(&wide_source(body), encoded);
            let mut measured = false;
            let mut restored = false;
            let mut settled = false;
            for _ in 0..100_000 {
                match vm
                    .step(
                        &mut session,
                        SessionBudget::RetryMinimum(SliceBudget::default()),
                        || 0,
                    )
                    .unwrap()
                {
                    SessionStep::Vm(RunOutcome::BudgetYield(usage))
                        if !measured && usage.required_work > SliceBudget::default().work =>
                    {
                        if encoded && !restored {
                            // Scheduling hints are omitted from the snapshot.
                            // Restore can compact the pool; the pending call
                            // recalculates its current need instead of replaying a grant.
                            (session, vm) = restore(&session, &mut vm);
                            restored = true;
                            measured = true;
                            continue;
                        }
                        // Exact keeps rejecting this same operation, despite the hint.
                        let SessionStep::Vm(RunOutcome::BudgetYield(rejected)) = vm
                            .step(
                                &mut session,
                                SessionBudget::Exact(SliceBudget::default()),
                                || 0,
                            )
                            .unwrap()
                        else {
                            panic!("exact grant unexpectedly advanced");
                        };
                        assert_eq!(rejected.required_work, usage.required_work);
                        assert_eq!(rejected.instructions, 0);
                        // An expired deadline must neither execute nor erase the hint.
                        let SessionStep::Vm(RunOutcome::BudgetYield(expired)) = vm
                            .step(
                                &mut session,
                                SessionBudget::RetryMinimum(SliceBudget {
                                    deadline_ns: Some(1),
                                    ..Default::default()
                                }),
                                || 1,
                            )
                            .unwrap()
                        else {
                            panic!("expired grant advanced");
                        };
                        assert_eq!(expired, aside_vm::executor::SliceUsage::default());
                        let before = vm.vm.stats();
                        let start = std::time::Instant::now();
                        let progress = vm
                            .step(
                                &mut session,
                                SessionBudget::RetryMinimum(SliceBudget {
                                    instructions: 1,
                                    ..Default::default()
                                }),
                                || 0,
                            )
                            .unwrap();
                        let elapsed = start.elapsed();
                        let after = vm.vm.stats();
                        assert!(matches!(
                            progress,
                            SessionStep::Vm(RunOutcome::BudgetYield(_))
                        ));
                        assert!(after.slots.capacity > before.slots.capacity);
                        assert_eq!(after.usage.instructions, 1);
                        eprintln!(
                            "host_budget kind={kind} asb={encoded} required_work={} required_allocation={} slice_us={} capacity={}->{} charged={}->{} heap_peak={}->{}",
                            usage.required_work,
                            usage.required_allocation,
                            elapsed.as_micros(),
                            before.slots.capacity,
                            after.slots.capacity,
                            before.slots.charged_bytes,
                            after.slots.charged_bytes,
                            before.heap.peak_charged_bytes,
                            after.heap.peak_charged_bytes
                        );
                        measured = true;
                    }
                    SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                        vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(32))
                            .unwrap();
                    }
                    SessionStep::Vm(RunOutcome::Returned(_)) => {
                        vm.discard_delivery(&mut session).unwrap()
                    }
                    SessionStep::Vm(RunOutcome::Fault(id)) => {
                        panic!("{:?}", vm.vm.delivery_fault(id).unwrap())
                    }
                    SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => panic!("{error:?}"),
                    SessionStep::Idle => {
                        settled = true;
                        break;
                    }
                    _ => {}
                }
            }
            assert!(measured, "{kind} did not exercise a window minimum");
            assert!(settled, "{kind} did not finish");
            vm.revoke(&mut session).unwrap();
            for _ in 0..100_000 {
                if !vm.cleanup_pending() {
                    break;
                }
                vm.cleanup_step(
                    &mut session,
                    64,
                    SessionBudget::RetryMinimum(SliceBudget {
                        work: 0,
                        allocation_bytes: 0,
                        ..Default::default()
                    }),
                    || 0,
                )
                .unwrap();
            }
            assert!(!vm.cleanup_pending());
        }
    }
}

#[test]
fn host_budget_keeps_each_rotating_callback_minimum_and_cleans_cancelled_tasks() {
    let code = "fn opening() { for i in 0..4 { spawn(|| { let marker = 7; await completion(); marker }); } }";
    let (mut session, mut vm) = models::prepared(code, false);
    loop {
        match next(&mut vm, &mut session) {
            RunOutcome::HostCall(ticket) => {
                vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(32))
                    .unwrap();
            }
            RunOutcome::Returned(_) => {
                vm.discard_delivery(&mut session).unwrap();
                break;
            }
            outcome => panic!("{outcome:?}"),
        }
    }
    assert_eq!(vm.tasks.len(), 4);
    let small = SliceBudget {
        instructions: 1,
        work: 0,
        allocation_bytes: 0,
        deadline_ns: None,
    };
    // Exercise task-local hints with deliberate scheduler rotation between
    // retries; production still keeps each synchronous script segment together.
    for index in 1..=4 {
        vm.schedule_cursor = index;
        let SessionStep::Vm(RunOutcome::BudgetYield(usage)) = vm
            .step(&mut session, SessionBudget::Exact(small), || 0)
            .unwrap()
        else {
            panic!("zero grant advanced");
        };
        assert!(usage.required_work > 0 || usage.required_allocation > 0);
    }
    for index in 1..=4 {
        vm.schedule_cursor = index;
        vm.step(&mut session, SessionBudget::RetryMinimum(small), || 0)
            .unwrap();
        let usage = vm.vm.stats().usage;
        assert!(
            usage.work != 0 || usage.allocation_bytes != 0,
            "callback {index} lost its minimum"
        );
        // A completed native boundary clears the grant. Resuming with zero work
        // must request a new minimum, even though this task received one before.
        let ticket = loop {
            vm.schedule_cursor = index;
            if let SessionStep::Vm(RunOutcome::HostCall(ticket)) = vm
                .step(
                    &mut session,
                    SessionBudget::RetryMinimum(SliceBudget::default()),
                    || 0,
                )
                .unwrap()
            {
                break ticket;
            }
        };
        vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(32))
            .unwrap();
        loop {
            vm.schedule_cursor = index;
            if let SessionStep::Vm(RunOutcome::BudgetYield(usage)) = vm
                .step(&mut session, SessionBudget::RetryMinimum(small), || 0)
                .unwrap()
            {
                assert_eq!(usage.work, 0);
                assert_eq!(usage.allocation_bytes, 0);
                break;
            }
        }
    }
    vm.revoke(&mut session).unwrap();
    let mut denied = false;
    for _ in 0..100_000 {
        if !vm.cleanup_pending() {
            break;
        }
        vm.cleanup_step(&mut session, 64, SessionBudget::RetryMinimum(small), || 0)
            .unwrap();
        denied |= vm.vm.stats().usage.required_work != 0;
    }
    assert!(
        denied,
        "cancellation did not exercise its hidden BudgetYield"
    );
    assert!(!vm.cleanup_pending());
    assert!(vm.tasks.is_empty());
    collect(&mut vm);
    assert_eq!(vm.vm.heap().stats().roots, 0);
}

#[test]
fn host_budget_retries_hidden_cleanup_minima_for_main_and_derived_tasks() {
    for body in [
        "wide(85);",
        "stage.replace(text(derive(|| f\"{wide(85)}\")));",
    ] {
        let (mut session, mut vm) = models::prepared(&wide_source(body), true);
        let mut blocked = false;
        for _ in 0..100_000 {
            match vm
                .step(
                    &mut session,
                    SessionBudget::Exact(SliceBudget::default()),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::BudgetYield(usage))
                    if usage.required_work > SliceBudget::default().work =>
                {
                    blocked = true;
                    break;
                }
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(32))
                        .unwrap();
                }
                SessionStep::Vm(RunOutcome::Returned(_)) => {
                    vm.discard_delivery(&mut session).unwrap()
                }
                SessionStep::Vm(RunOutcome::Fault(id)) => {
                    panic!("{:?}", vm.vm.delivery_fault(id).unwrap())
                }
                _ => {}
            }
        }
        assert!(blocked);
        (session, vm) = restore(&session, &mut vm);
        vm.revoke(&mut session).unwrap();
        let mut denied = false;
        for _ in 0..100_000 {
            if !vm.cleanup_pending() {
                break;
            }
            vm.cleanup_step(
                &mut session,
                64,
                SessionBudget::RetryMinimum(SliceBudget {
                    instructions: 1,
                    work: 0,
                    allocation_bytes: 0,
                    deadline_ns: None,
                }),
                || 0,
            )
            .unwrap();
            denied |= vm.vm.stats().usage.required_work != 0;
        }
        assert!(denied);
        assert!(!vm.cleanup_pending());
        collect(&mut vm);
        assert_eq!(vm.vm.heap().stats().roots, 0);
    }
}
