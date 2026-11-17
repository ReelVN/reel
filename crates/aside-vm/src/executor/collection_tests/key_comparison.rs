//! Key comparison fusion against the existing resumable equality implementation.
use super::*;

fn construction(count: u32) -> BytecodePackage {
    let mut constants = Vec::new();
    let mut code = vec![op(Opcode::MakeTable, 0, 0, 0)];
    // Include equal prefixes, unequal lengths, UTF-8, empty keys and keys that
    // span the bounded fast comparison. The second pass overwrites every value.
    for pass in 0..2 {
        for i in 0..count {
            let key = match i % 4 {
                0 => format!("圖片/{i:03}"),
                1 => format!("asset/{i}"),
                2 => format!("{}-{i}", "long-key-".repeat(8)),
                _ => {
                    if i == 3 {
                        String::new()
                    } else {
                        format!("{i}")
                    }
                }
            };
            let index = constants.len() as u32;
            constants.push(Const::Utf8(key));
            constants.push(Const::Int((i + pass) as i64));
            code.extend([
                op(Opcode::LoadConst, 1, index, 0),
                op(Opcode::LoadConst, 2, index + 1, 0),
                op(Opcode::IndexSet, 0, 1, 2),
            ]);
        }
    }
    code.extend([op(Opcode::IndexGet, 3, 0, 1), op(Opcode::Return, 3, 0, 0)]);
    package(constants, code, vec![Descriptor::Table(vec![])], 4)
}

fn run(vm: &mut Vm, work: u64, deadline: bool, collect: bool) -> (i64, u64, u64) {
    let task = vm
        .start(FunctionId(0), &[], RootOwner(9), Context::Ordinary)
        .unwrap();
    let mut budget = SliceBudget {
        work,
        instructions: 100_000,
        deadline_ns: deadline.then_some(1),
        ..Default::default()
    };
    let mut used = 0;
    let mut clocks = 0;
    for _ in 0..2_000_000 {
        let mut queries = 0;
        let outcome = vm
            .run_slice(task, budget, || {
                queries += 1;
                u64::from(queries > 1)
            })
            .unwrap();
        used += vm.last_usage.work;
        clocks += queries;
        assert!(vm.last_usage.work <= budget.work);
        match outcome {
            RunOutcome::BudgetYield(usage) => {
                budget.work = work.max(usage.required_work);
                budget.allocation_bytes = budget.allocation_bytes.max(usage.required_allocation);
                if collect {
                    gc(vm);
                }
            }
            RunOutcome::Returned(delivery) => {
                let value = vm.delivery_value(delivery).unwrap().as_int().unwrap();
                vm.discard_delivery(delivery).unwrap();
                assert!(vm.active.equality_cursors.data.is_empty());
                return (value, used, clocks);
            }
            other => panic!("{other:?}"),
        }
    }
    panic!("table construction did not make bounded progress");
}

#[test]
fn text_key_comparison_preserves_results_work_gc_and_deadline_boundaries() {
    for encoded in [false, true] {
        for work in [1, 7, 64, 4096] {
            for deadline in [false, true] {
                let mut results = Vec::new();
                for general in [true, false] {
                    let program = verify(construction(12), Limits::default()).unwrap();
                    let program = if encoded {
                        VerifiedProgram::decode(
                            program.encode(Limits::default()).unwrap().as_bytes(),
                            Limits::default(),
                        )
                        .unwrap()
                    } else {
                        program
                    };
                    let mut vm = Vm::load(
                        program,
                        RuntimeSupport::default(),
                        VmLimits::default(),
                        SessionEpoch(81),
                    )
                    .unwrap();
                    vm.work_probe.general_table_key_comparison = general;
                    results.push(run(&mut vm, work, deadline, true));
                }
                assert_eq!(results[0].0, 12);
                assert_eq!(
                    results[0], results[1],
                    "encoded={encoded} work={work} deadline={deadline}"
                );
            }
        }
    }
}

#[test]
fn dynamic_table_index_matches_the_scan_under_gc_and_small_budgets() {
    for count in [32, 128] {
        for work in [1, 64, 4096] {
            for linear in [true, false] {
                let mut vm = load(construction(count));
                vm.work_probe.linear_table_scan = linear;
                assert_eq!(run(&mut vm, work, false, true).0, i64::from(count));
            }
        }
    }
}
