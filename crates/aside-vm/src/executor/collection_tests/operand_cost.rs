//! Packed bytecode operand admission and throughput.
use super::*;

#[test]
#[ignore = "release differential paired-register execution measurement"]
fn packed_arithmetic_operand_cost() {
    for paired in [false, true, true, false] {
        let p = package(
            vec![Const::Int(0), Const::Int(1), Const::Int(1000000)],
            vec![
                op(Opcode::LoadConst, 0, 0, 0),
                op(Opcode::LoadConst, 1, 1, 0),
                op(Opcode::LoadConst, 2, 2, 0),
                op(Opcode::Add, 0, 0, 1),
                op(Opcode::Lt, 3, 0, 2),
                op(Opcode::BranchBool, 3, 3, 6),
                op(Opcode::Return, 0, 0, 0),
            ],
            vec![],
            4,
        );
        let program = verify(p, Limits::default()).unwrap();
        let program = VerifiedProgram::decode(
            program.encode(Limits::default()).unwrap().as_bytes(),
            Limits::default(),
        )
        .unwrap();
        let mut vm = Vm::load(
            program,
            RuntimeSupport {
                host_types: vec![],
                opcodes: vec![],
                capabilities: vec![],
                natives: vec![],
            },
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        vm.slots.paired_reads = paired;
        let task = vm
            .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let start = std::time::Instant::now();
        let mut work = 0;
        let mut instructions = 0;
        loop {
            let result = vm.run_slice(task, SliceBudget::default(), || 0).unwrap();
            work += vm.last_usage.work;
            instructions += vm.last_usage.instructions as u64;
            match result {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(delivery) => {
                    assert_eq!(vm.delivery_value(delivery).unwrap().as_int(), Some(1000000));
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        eprintln!(
            "packed_operands paired={paired} elapsed_us={} work={work} instructions={instructions}",
            start.elapsed().as_micros()
        );
    }
}
