use super::*;

fn literal(count: u32, long: bool) -> BytecodePackage {
    let keys = (0..count)
        .chain(std::iter::once(7))
        .map(|i| {
            if long && i == 15 {
                "x".repeat(65)
            } else {
                format!("pose {i}")
            }
        })
        .collect::<Vec<_>>();
    let mut constants = Vec::new();
    let mut code = Vec::new();
    let mut fields = Vec::new();
    for (i, key) in keys.into_iter().enumerate() {
        let register = i as u32 * 2;
        constants.push(Const::Utf8(key.into()));
        constants.push(Const::Int(i as i64));
        code.push(op(Opcode::LoadConst, register, register, 0));
        code.push(op(Opcode::LoadConst, register + 1, register + 1, 0));
        fields.push([Reg(register), Reg(register + 1)]);
    }
    let result = constants.len() as u32;
    code.push(op(Opcode::MakeTable, result, 0, 0));
    code.push(op(Opcode::Return, result, 0, 0));
    package(constants, code, vec![Descriptor::Table(fields)], result + 1)
}

fn execute(vm: &mut Vm, count: u32, work: u64, collect: bool) -> u64 {
    let task = vm
        .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let mut total_work = 0;
    let result = loop {
        let out = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 100_000,
                    work: work.max(vm.last_usage.required_work),
                    allocation_bytes: (16 << 20).max(vm.last_usage.required_allocation),
                    deadline_ns: None,
                },
                || 0,
            )
            .unwrap();
        total_work += vm.last_usage.work;
        if let RunOutcome::Returned(delivery) = out {
            break vm.delivery_value(delivery).unwrap();
        }
        assert!(matches!(out, RunOutcome::BudgetYield(_)), "{out:?}");
        if collect {
            gc(vm);
        }
    };
    assert_eq!(vm.heap.table_len(result).unwrap(), count);
    for i in 0..count {
        let (_, value) = vm.heap.table_entry(result, i).unwrap();
        assert_eq!(
            value.as_int(),
            Some(if i == 7 { count as i64 } else { i as i64 })
        );
    }
    total_work
}

#[test]
fn indexed_literals_preserve_duplicate_order_and_small_budget_gc_continuations() {
    for enabled in [false, true] {
        for long in [false, true] {
            for work in [1, 64, 4096] {
                let mut vm = load(literal(32, long));
                vm.work_probe.linear_table_scan = !enabled;
                execute(&mut vm, 32, work, true);
            }
        }
    }
}

#[test]
fn large_indexed_literals_keep_order_and_duplicate_updates() {
    let mut vm = load(literal(768, false));
    execute(&mut vm, 768, 4096, true);
}

#[test]
fn indexed_literals_keep_subquadratic_work_across_budget_yields() {
    let mut indexed = load(literal(192, false));
    let mut scanned = load(literal(192, false));
    scanned.work_probe.linear_table_scan = true;
    let indexed_work = execute(&mut indexed, 192, 4096, false);
    let scanned_work = execute(&mut scanned, 192, 4096, false);
    assert!(
        indexed_work * 2 < scanned_work,
        "{indexed_work} vs {scanned_work}"
    );
    let mut larger = load(literal(384, false));
    let larger_work = execute(&mut larger, 384, 4096, false);
    assert!(
        larger_work * 2 < indexed_work * 5,
        "{indexed_work} -> {larger_work}"
    );
}

#[test]
fn partially_indexed_table_restores_and_finishes_with_the_same_order() {
    use crate::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};
    let mut vm = load(literal(64, false));
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let mut indexed = false;
    for _ in 0..10_000 {
        assert!(matches!(
            vm.run_slice(
                task,
                SliceBudget {
                    instructions: 100_000,
                    work: 512.max(vm.last_usage.required_work),
                    allocation_bytes: (1 << 20).max(vm.last_usage.required_allocation),
                    ..Default::default()
                },
                || 0
            )
            .unwrap(),
            RunOutcome::BudgetYield(_)
        ));
        if let Some(Frame::Script(frame)) = vm.active.frames.data.last()
            && frame.table_work.is_some()
        {
            let table = vm.value(frame.env, 1).unwrap();
            if vm.heap.table_index_bytes(table).unwrap() < crate::payload::table::INDEX_BASE_BYTES {
                indexed = true;
                break;
            }
        }
    }
    assert!(indexed);
    let registry = SnapshotRegistry::default();
    let mut capture = vm.snapshot_begin(&registry).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let image = VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = Vm::restore_begin(
        &image,
        vm.program().clone(),
        RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(82),
        &registry,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (mut restored, map) = restore.finish().unwrap();
    let result = returned(&mut restored, map.task(task).unwrap());
    assert_eq!(restored.heap.table_len(result).unwrap(), 64);
    for position in 0..64 {
        assert_eq!(
            restored
                .heap
                .table_entry(result, position)
                .unwrap()
                .1
                .as_int(),
            Some(if position == 7 {
                64
            } else {
                i64::from(position)
            })
        );
    }
}
