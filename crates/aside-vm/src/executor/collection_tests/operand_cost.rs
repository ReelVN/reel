//! Packed bytecode operand admission and throughput.
use super::*;

#[test]
fn literal_type_check_charges_payload_only_when_string_length_matches() {
    let expected = "a".repeat(64);
    let run = |subject: &str, accepted| {
        let mut p = package(
            vec![Const::Utf8(subject.into())],
            vec![op(Opcode::LoadConst, 0, 0, 0), op(Opcode::Return, 0, 0, 0)],
            vec![],
            1,
        );
        let literal = p.types.len() as u32;
        p.types.push(TypeDef::StringLiteral(expected.clone()));
        p.functions[0].code.insert(1, op(Opcode::Is, 0, 0, literal));
        let mut vm = load(p);
        let task = vm
            .start(FunctionId(0), &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        let out = vm.run_slice(task, SliceBudget::default(), || 0).unwrap();
        let RunOutcome::Returned(delivery) = out else {
            panic!("{out:?}")
        };
        assert_eq!(
            vm.delivery_value(delivery).unwrap().as_bool(),
            Some(accepted)
        );
        vm.last_usage.work
    };
    let mismatch_work = run("short", false);
    assert_eq!(run(&expected, true) - mismatch_work, 64);
    assert_eq!(run(&"b".repeat(64), false) - mismatch_work, 64);
}

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
        // Isolate the original checked operand API; borrowed blocks validate
        // their mapping once and intentionally bypass this test-only switch.
        vm.work_probe.single_scalar_step = true;
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
