use super::*;
use crate::frame_slots::counts::SlotState;
use crate::{executor::*, snapshot::*};

fn vm() -> Vm {
    Vm::load(
        verify(fixtures::helper_native(), Limits::default()).unwrap(),
        crate::executor::review_tests::echo_registry(),
        VmLimits::default(),
        SessionEpoch(42),
    )
    .unwrap()
}

#[test]
fn counts_reconcile_script_native_and_sliced_work() {
    let mut vm = vm();
    let initial = vm.execution_counts();
    eprintln!(
        "execution-counts-layout snapshot={} state={} slots={}",
        size_of::<ExecutionCounts>(),
        size_of::<State>(),
        size_of::<SlotState>()
    );
    assert_eq!(initial.slots, SlotCounts::default());
    assert_eq!(initial.script_entries, 0);
    let task = vm
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let mut budget = SliceBudget {
        instructions: 1,
        work: 1,
        allocation_bytes: 0,
        deadline_ns: None,
    };
    loop {
        let before = vm.execution_counts();
        let outcome = vm.run_slice(task, budget, || 0).unwrap();
        let after = vm.execution_counts();
        let delta = after.checked_delta(&before).unwrap();
        assert_eq!(
            delta.opcode_admissions().map(|(_, n)| n).sum::<u64>(),
            delta.instructions
        );
        match outcome {
            RunOutcome::BudgetYield(usage) => {
                assert_eq!(delta.instructions, u64::from(usage.instructions));
                budget.work = usage.required_work.max(1);
                budget.allocation_bytes = usage.required_allocation;
            }
            RunOutcome::HostCall(ticket) => {
                vm.respond(ticket, HostResponse::Ready(Value::int(42)))
                    .unwrap();
            }
            RunOutcome::Returned(delivery) => {
                assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(42));
                vm.discard_delivery(delivery).unwrap();
                break;
            }
            other => panic!("unexpected outcome {other:?}"),
        }
    }
    let counts = vm.execution_counts().checked_delta(&initial).unwrap();
    assert_eq!(counts.instructions, 5);
    assert_eq!(counts.bytecode_execute_steps, 5);
    assert_eq!(counts.script_entries, 2);
    assert_eq!(counts.script_register_values, 4);
    assert_eq!(counts.script_parameter_declarations, 1);
    assert_eq!(counts.script_binding_declarations, 1);
    assert_eq!(counts.argument_values_copied, 1);
    assert_eq!(counts.parameters_prepared, 1);
    assert_eq!(counts.install_bindings_visited, 1);
    assert_eq!(counts.native_frames, 1);
    assert_eq!(counts.native_entries, 1);
    assert!(counts.native_steps.iter().sum::<u64>() > counts.native_frames);
    assert_eq!(counts.slots.open_calls, counts.slots.release_calls);
    assert_eq!(counts.slots.open_values, counts.slots.release_values);
}

#[test]
fn restore_starts_fresh_diagnostics_and_rejects_cross_identity_deltas() {
    let mut vm = vm();
    vm.start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let before = vm.execution_counts();
    assert_eq!(before.script_entries, 1);
    let registry = SnapshotRegistry::default();
    let mut capture = vm.snapshot_begin(&registry).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let image = capture.finish().unwrap();
    let mut restore = Vm::restore_begin(
        &image,
        vm.program().clone(),
        crate::executor::review_tests::echo_registry(),
        VmLimits::default(),
        SessionEpoch(43),
        &registry,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (restored, _) = restore.finish().unwrap();
    let after = restored.execution_counts();
    assert_ne!(after.heap_id, before.heap_id);
    assert!(after.checked_delta(&before).is_none());
    let mut expected = ExecutionCounts::default();
    expected.heap_id = after.heap_id;
    expected.epoch = 43;
    assert_eq!(after, expected);
    let mut lower = before;
    lower.script_entries = 0;
    assert!(lower.checked_delta(&before).is_none());
}

#[test]
fn slots_count_requested_packed_operands_and_null_initialization() {
    let mut vm = vm();
    let before = vm.execution_counts();
    let owner = RootOwner(7);
    let values = vm
        .slots
        .open(&mut vm.heap, owner, RootKind::TaskFrame, 3)
        .unwrap();
    let empty = vm
        .slots
        .open(&mut vm.heap, owner, RootKind::TaskFrame, 0)
        .unwrap();
    vm.slots
        .clear_registers(&mut vm.heap, values, [0, 1, 2])
        .unwrap();
    vm.slots.release(&mut vm.heap, values).unwrap();
    vm.slots.release(&mut vm.heap, empty).unwrap();
    let slots = vm.execution_counts().checked_delta(&before).unwrap().slots;
    assert_eq!(slots.open_calls, 2);
    assert_eq!(slots.open_values, 3);
    assert_eq!(slots.empty_opens, 1);
    assert_eq!(slots.clear_calls, 1);
    assert_eq!(slots.clear_values, 3);
    assert_eq!(slots.release_calls, 2);
    assert_eq!(slots.release_values, 3);
}
