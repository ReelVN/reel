use super::*;
use std::cell::Cell;

thread_local! { static CALLS: Cell<u32> = const { Cell::new(0) }; }

#[test]
fn read_only_leaf_registration_requires_pure_effects_and_nonzero_work() {
    for (effects, work) in [(Effects::ALL, 1), (Effects(0), 0)] {
        let mut package = fixtures::helper_native();
        let mut support = review_tests::echo_registry();
        package.signatures[0].effects = effects;
        support.natives[0].signature.effects = effects;
        support.natives[0].read_only_leaf = Some(ReadOnlyLeaf { work, invoke: leaf });
        assert!(matches!(
            Vm::load(
                verify(package, Limits::default()).unwrap(),
                support,
                VmLimits::default(),
                SessionEpoch(1),
            ),
            Err(VmError::Format(_))
        ));
    }
}

fn leaf(_: &Heap, args: &[Value]) -> Option<Value> {
    CALLS.set(CALLS.get() + 1);
    Some(Value::int(args[0].as_int()? + 1))
}

fn started(invoke: fn(&Heap, &[Value]) -> Option<Value>) -> (Vm, TaskId, RuntimeSupport) {
    CALLS.set(0);
    let mut package = fixtures::helper_native();
    let mut support = review_tests::echo_registry();
    package.signatures[0].effects = Effects(0);
    support.natives[0].signature.effects = Effects(0);
    support.natives[0].read_only_leaf = Some(ReadOnlyLeaf { work: 1, invoke });
    let mut vm = Vm::load(
        verify(package, Limits::default()).unwrap(),
        support.clone(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let task = vm
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    (vm, task, support)
}

fn prepared(invoke: fn(&Heap, &[Value]) -> Option<Value>) -> (Vm, TaskId, RuntimeSupport) {
    let (mut vm, task, support) = started(invoke);
    loop {
        if matches!(vm.active.frames.data.last(), Some(Frame::Native(n))
            if matches!(n.phase, NativePhase::Resume(NativeInput::Entry) | NativePhase::Issue(NativeInput::Entry)))
        {
            break;
        }
        let budget = SliceBudget {
            instructions: 1,
            work: vm.last_usage.required_work.max(1),
            allocation_bytes: vm.last_usage.required_allocation,
            ..Default::default()
        };
        assert!(matches!(
            vm.run_slice(task, budget, || 0).unwrap(),
            RunOutcome::BudgetYield(_)
        ));
    }
    (vm, task, support)
}

#[test]
fn direct_leaf_publishes_caller_before_clock_yield_and_snapshot_without_a_native_frame() {
    let mut roots = Vec::new();
    for legacy in [true, false] {
        let (mut vm, task, support) = started(leaf);
        vm.legacy_native_leaf_calls = legacy;
        let outcome = vm
            .run_slice(
                task,
                SliceBudget {
                    deadline_ns: Some(1),
                    ..Default::default()
                },
                || u64::from(CALLS.get() != 0),
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::BudgetYield(_)));
        assert_eq!(CALLS.get(), 1);
        assert_eq!(
            matches!(vm.active.frames.data.last(), Some(Frame::Native(_))),
            legacy
        );
        roots.push(vm.heap.stats().root_ranges);
        review_tests::collect(&mut vm);
        let (mut vm, task) = native_plan_tests::restore(vm, task, support);
        returned(&mut vm, task);
        assert_eq!(CALLS.get(), 1);
    }
    assert!(
        roots[1] < roots[0],
        "direct call omits registered native roots"
    );
}

fn returned(vm: &mut Vm, task: TaskId) {
    loop {
        match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::Returned(id) => {
                assert_eq!(vm.delivery_value(id).unwrap().as_int(), Some(42));
                vm.discard_delivery(id).unwrap();
                break;
            }
            other => panic!("leaf must finish without a host ticket: {other:?}"),
        }
    }
}

#[test]
fn read_only_leaf_obeys_work_and_restores_the_committed_result_once() {
    let (mut vm, task, support) = prepared(leaf);
    assert!(matches!(
        vm.run_slice(
            task,
            SliceBudget {
                work: 0,
                ..Default::default()
            },
            || 0
        )
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    assert_eq!(CALLS.get(), 0);
    let outcome = vm
        .run_slice(
            task,
            SliceBudget {
                work: 1,
                ..Default::default()
            },
            || 0,
        )
        .unwrap();
    assert!(matches!(outcome, RunOutcome::BudgetYield(_)));
    assert_eq!(CALLS.get(), 1);
    assert!(
        matches!(vm.active.frames.data.last(), Some(Frame::Native(n)) if matches!(n.phase, NativePhase::Commit(false)))
    );
    review_tests::collect(&mut vm);
    let (mut vm, task) = native_plan_tests::restore(vm, task, support);
    returned(&mut vm, task);
    assert_eq!(CALLS.get(), 1);
}

#[test]
fn read_only_leaf_keeps_entry_and_return_clock_boundaries() {
    let (mut vm, task, _) = prepared(leaf);
    let budget = SliceBudget {
        deadline_ns: Some(1),
        ..Default::default()
    };
    assert!(matches!(
        vm.run_slice(task, budget, || 1).unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    assert_eq!(CALLS.get(), 0);
    let mut clocks = 0;
    assert!(matches!(
        vm.run_slice(task, budget, || {
            let time = clocks;
            clocks += 1;
            time
        })
        .unwrap(),
        RunOutcome::BudgetYield(_)
    ));
    assert_eq!(CALLS.get(), 1);
    assert_eq!(clocks, 2);
    assert!(
        matches!(vm.active.frames.data.last(), Some(Frame::Native(n)) if matches!(n.phase, NativePhase::Commit(false)))
    );
    returned(&mut vm, task);
    assert_eq!(CALLS.get(), 1);
}

#[test]
fn rejected_leaf_input_issues_the_original_ticket_and_consumes_one_work() {
    let (mut vm, task, _) = prepared(|_, _| None);
    let RunOutcome::HostCall(ticket) = vm
        .run_slice(
            task,
            SliceBudget {
                work: 1,
                ..Default::default()
            },
            || 0,
        )
        .unwrap()
    else {
        panic!("ordinary handler")
    };
    assert_eq!(vm.last_usage.work, 1);
    assert_eq!(vm.host_arguments(ticket).unwrap()[0].as_int(), Some(41));
    vm.respond(ticket, HostResponse::Ready(Value::int(42)))
        .unwrap();
    returned(&mut vm, task);
}
