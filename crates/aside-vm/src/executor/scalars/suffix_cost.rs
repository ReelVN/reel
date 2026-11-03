//! Use the arithmetic lifetime sequence emitted by Motion.sample expressions.
use super::*;

fn fixture(n: i64, sites: usize, legacy: bool) -> Vm {
    let mut p = package(0);
    p.constants.push(Const::Int(n));
    let f = &mut p.functions[0];
    f.register_count = 8;
    f.source_map.clear();
    f.code = vec![
        op(Opcode::LoadConst, 0, 2, 0),
        op(Opcode::LoadConst, 1, 0, 0),
        op(Opcode::LoadConst, 5, 0, 0),
        op(Opcode::LoadConst, 6, 1, 0),
        op(Opcode::LoadConst, 7, 4, 0),
    ];
    for _ in 0..sites {
        f.code.extend([
            op(Opcode::Move, 3, 0, 0),
            op(Opcode::Move, 4, 1, 0),
            op(Opcode::Add, 2, 3, 4),
            op(Opcode::Clear2, 3, 4, 0),
            op(Opcode::Move, 0, 2, 0),
            op(Opcode::Clear, 2, 0, 0),
        ]);
    }
    let done = f.code.len() as u32 + 3;
    f.code.extend([
        op(Opcode::Add, 5, 5, 6),
        op(Opcode::Lt, 3, 5, 7),
        op(Opcode::BranchBool, 3, 5, done),
        op(Opcode::Return, 0, 0, 0),
    ]);
    let program = verify(p, Limits::default()).unwrap();
    let program = VerifiedProgram::decode(
        program.encode(Limits::default()).unwrap().as_bytes(),
        Limits::default(),
    )
    .unwrap();
    let mut vm = Vm::load(
        program,
        RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(56),
    )
    .unwrap();
    vm.legacy_scalar_suffixes = legacy;
    vm
}

#[test]
#[ignore = "release same-binary compiler lifetime-suffix throughput measurement"]
fn scalar_lifetime_suffix_cost() {
    let n = std::env::var("ASIDE_BENCH_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000);
    for sites in [1, 32] {
        for legacy in [true, false, false, true] {
            let iterations = n / sites as i64;
            let mut vm = fixture(iterations, sites, legacy);
            let task = vm
                .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
                .unwrap();
            let start = std::time::Instant::now();
            loop {
                match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::Returned(delivery) => {
                        assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(37));
                        break;
                    }
                    other => panic!("{other:?}"),
                }
            }
            eprintln!(
                "scalar_suffix legacy={legacy} sites={sites} iterations={iterations} elapsed_us={} instructions={} work={} slices={} groups={} dispatches_removed={}",
                start.elapsed().as_micros(),
                vm.cumulative_instructions,
                vm.cumulative_work,
                vm.slices,
                vm.work_probe.scalar_suffix_groups.get(),
                vm.work_probe.scalar_suffix_steps.get()
            );
        }
    }
}

#[test]
fn lifetime_suffix_retires_work_without_changing_completed_execution() {
    let mut observed = Vec::new();
    for legacy in [true, false] {
        let mut vm = fixture(97, 1, legacy);
        let task = vm
            .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let RunOutcome::Returned(delivery) =
            vm.run_slice(task, SliceBudget::default(), || 0).unwrap()
        else {
            panic!("completed loop")
        };
        assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(37));
        observed.push((vm.cumulative_instructions, vm.cumulative_work));
        assert_eq!(vm.work_probe.scalar_suffix_groups.get() != 0, !legacy);
    }
    assert_eq!(observed[0], observed[1]);
}
