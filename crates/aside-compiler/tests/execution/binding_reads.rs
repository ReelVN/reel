//! Borrowed lexical roots survive each budget, GC and snapshot boundary.
use super::*;
use aside_vm::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};

#[test]
fn immutable_member_receivers_survive_snapshot_at_every_small_slice() {
    let source = r#"
        class Reading {
            let value: int = 40;
            fn sample(self) { self.value + 2 }
        }
        fn fetch(receiver) { receiver.sample() }
        fn main() { fetch(Reading()) }
    "#;
    let module = analyze_module(source, &AnalysisOptions::default(), &mut NoModules)
        .unwrap()
        .into_module()
        .unwrap();
    let compiled =
        lowering::compile(&module, &Default::default(), &[], Default::default()).unwrap();
    let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
    let main = bc::FunctionId(
        compiled
            .program
            .package()
            .functions
            .iter()
            .position(|function| {
                compiled.program.package().symbols[function.name.index()] == "main"
            })
            .unwrap() as u32,
    );
    let mut vm = Vm::load(
        bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap(),
        bc::RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let init = vm
        .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
        .unwrap();
    finish(&mut vm, init, false);
    let mut task = vm
        .start(main, &[], RootOwner(1), bc::Context::Ordinary)
        .unwrap();
    let registry = SnapshotRegistry::default();
    let mut budget = SliceBudget {
        instructions: 1,
        work: 1,
        allocation_bytes: 0,
        deadline_ns: None,
    };
    let mut member_yields = 0;
    for epoch in 2..2000 {
        let outcome = vm.run_slice(task, budget, || 0).unwrap();
        if vm.heap().phase() == GcPhase::Idle {
            vm.begin_collection().unwrap();
        }
        vm.gc_step(GcBudget {
            units: 1,
            ..Default::default()
        })
        .unwrap();
        if let RunOutcome::BudgetYield(usage) = outcome {
            budget.work = usage.required_work.max(1);
            budget.allocation_bytes = usage.required_allocation;
            if vm.task_location(task).unwrap().is_some_and(|location| {
                vm.program().package().functions[location.code.function.index()]
                    .code
                    .instruction(location.pc.index())
                    .unwrap()
                    .is_some_and(|instruction| instruction.opcode == bc::Opcode::MemberGet as u16)
            }) {
                member_yields += 1;
            }
        }
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture.step(SnapshotBudget { units: 3 }).unwrap().complete {}
        let image = VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
        let mut restore = Vm::restore_begin(
            &image,
            bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap(),
            bc::RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(epoch),
            &registry,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 3 }).unwrap().complete {}
        let (loaded, map) = restore.finish().unwrap();
        vm = loaded;
        task = map.task(task).unwrap();
        match outcome {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::Returned(delivery) => {
                assert_eq!(
                    vm.delivery_value(map.delivery(delivery).unwrap())
                        .unwrap()
                        .as_int(),
                    Some(42)
                );
                assert!(
                    member_yields > 0,
                    "exercise the rewritten receiver across suspension"
                );
                return;
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
    panic!("restored immutable member reads did not finish");
}
