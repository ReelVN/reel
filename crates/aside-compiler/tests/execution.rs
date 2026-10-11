//! Source -> analysis -> B01 round trip -> real VM acceptance.
use aside_compiler::{AnalysisOptions, analyze_module, lowering, semantic::NoModules};
use aside_vm::{GcBudget, GcPhase, RootOwner, SessionEpoch, Value, bytecode as bc, executor::*};

fn finish(vm: &mut Vm, task: TaskId, tiny: bool) -> Value {
    let mut budget = if tiny {
        SliceBudget {
            instructions: 1,
            work: 1,
            allocation_bytes: 0,
            deadline_ns: None,
        }
    } else {
        SliceBudget::default()
    };
    for _ in 0..100_000 {
        let outcome = vm.run_slice(task, budget, || 0).unwrap();
        if tiny {
            if vm.heap().phase() == GcPhase::Idle {
                vm.begin_collection().unwrap();
            }
            vm.gc_step(GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
        }
        match outcome {
            RunOutcome::BudgetYield(usage) => {
                budget.work = usage.required_work.max(1);
                budget.allocation_bytes = usage.required_allocation;
            }
            RunOutcome::Returned(id) => {
                let value = vm.delivery_value(id).unwrap();
                // These acceptance cases return scalars; no unrooted object escapes.
                assert!(value.as_int().is_some() || value.same_bits(Value::NULL));
                vm.discard_delivery(id).unwrap();
                return value;
            }
            RunOutcome::Fault(id) => panic!("unexpected source fault: {:?}", vm.delivery_fault(id)),
            other => panic!("unexpected source outcome: {other:?}"),
        }
    }
    panic!("source fixture exceeded bounded slice count");
}

#[test]
fn source_contracts_survive_codec_tiny_slices_and_interleaved_gc() {
    let cases = [
        ("fn main() { 6 * 7 }", 42),
        (
            "fn main() { let mut n=0; fn add() { n+=1; n } add(); add(); n }",
            2,
        ),
        ("fn main() { fn f(a, b=a+1) { b } f(41) }", 42),
        (
            "let mut n=0; fn main() { try { try { throw 7; } catch e { n=e; } finally { n+=10; } } finally { n+=100; } n }",
            117,
        ),
        (
            "fn main() { let mut a=[1,2]; let old=a; fn replace() { a=[9]; a[0] } old[0]=replace(); old[0]*10+a[0] }",
            99,
        ),
        ("fn main() { let t={x:2}; t.x+=3; t[\"x\"] }", 5),
        (
            "fn main() { let mut n=0; fn side() { n+=1; 0 } let absent=null; let result=absent?.x[side()]; n }",
            0,
        ),
        (
            "fn main() { fn f(a,...rest) { a+rest[0]+rest[1] } let values=[1,2]; f(...values,3) }",
            6,
        ),
        (
            "fn main() { fn f(...rest) { rest } let first=f(1); let second=f(2); first[0]=9; second[0] }",
            2,
        ),
        ("fn main() { let mut n=0; while n<3 { n+=1; } n }", 3),
    ];
    for (source, expected) in cases {
        let module = analyze_module(source, &AnalysisOptions::default(), &mut NoModules)
            .unwrap()
            .into_module()
            .unwrap();
        let compiled = lowering::compile(
            &module,
            &lowering::Linkage::default(),
            lowering::Options::default(),
        )
        .unwrap();
        let package = compiled.program.package();
        let entry = bc::FunctionId(
            package
                .functions
                .iter()
                .position(|f| package.symbols[f.name.index()] == "main")
                .unwrap() as u32,
        );
        let bytes = bc::encode(package, bc::Limits::default()).unwrap();
        for tiny in [false, true] {
            let program = bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap();
            let mut vm = Vm::load(
                program,
                bc::RuntimeSupport {
                    opcodes: vec![],
                    natives: vec![],
                    capabilities: vec![],
                },
                VmLimits::default(),
                SessionEpoch(1),
            )
            .unwrap();
            let init = vm
                .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
                .unwrap();
            assert!(finish(&mut vm, init, tiny).same_bits(Value::NULL));
            let task = vm
                .start(entry, &[], RootOwner(1), bc::Context::Ordinary)
                .unwrap();
            assert_eq!(
                finish(&mut vm, task, tiny).as_int(),
                Some(expected),
                "{source}, tiny={tiny}"
            );
            assert!(vm.is_idle());
        }
    }
}
