use super::*;
use crate::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};

fn search_package(method: &str) -> BytecodePackage {
    let mut p = method_package(method);
    let name = SymbolId(p.symbols.len() as u32);
    p.symbols.push("value".into());
    p.signatures[0].parameters.push(Parameter {
        name,
        ty: TypeId(0),
        mutable: false,
        default: None,
    });
    p.functions[0].register_count = 4;
    p.functions[0].code[0].operands[0] = 2;
    p.functions[0].code[1].operands[0] = 3;
    p.functions[0].code[2].operands[0] = 3;
    let Descriptor::Call(call) = &mut p.functions[0].descriptors[0] else {
        unreachable!()
    };
    call.target = CallTarget::Register(Reg(2));
    call.arguments = Arguments::Window(RegWindow {
        start: Reg(1),
        len: 1,
    });
    p
}

#[test]
fn sequence_search_resumes_comparison_without_copying_the_source() {
    for method in ["find", "contains"] {
        for action in 0..4 {
            let mut vm = load(search_package(method));
            let text = "中文🙂".repeat(256);
            let first = vm.heap_mut().alloc_string(&text).unwrap();
            let target = vm.heap_mut().alloc_string(&text).unwrap();
            let source = vm.heap_mut().alloc_array(&[Value::int(7), first]).unwrap();
            let mut task = vm
                .start(
                    FunctionId(0),
                    &[source, target],
                    RootOwner(9),
                    Context::Ordinary,
                )
                .unwrap();
            for _ in 0..1000 {
                if matches!(vm.active.frames.data.last(), Some(Frame::Script(s))
                    if matches!(s.phase, ScriptPhase::Search(_))
                    && vm.active.equality_cursors.data.first().copied().unwrap_or(0) > 0)
                {
                    break;
                }
                assert!(matches!(tick(&mut vm, task), RunOutcome::BudgetYield(_)));
            }
            assert!(
                !vm.active.equality_cursors.data.is_empty(),
                "must suspend within text comparison"
            );
            match action {
                0 => {
                    let registry = SnapshotRegistry::default();
                    let mut capture = vm.snapshot_begin(&registry).unwrap();
                    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
                    let image =
                        VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
                    let mut restore = Vm::restore_begin(
                        &image,
                        vm.program().clone(),
                        RuntimeSupport {
                            host_types: Vec::new(),
                            opcodes: vec![],
                            capabilities: vec![],
                            natives: vec![],
                        },
                        VmLimits::default(),
                        SessionEpoch(82),
                        &registry,
                    )
                    .unwrap();
                    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
                    let (restored, map) = restore.finish().unwrap();
                    task = map.task(task).unwrap();
                    vm = restored;
                    let result = returned(&mut vm, task);
                    if method == "find" {
                        assert_eq!(result.as_int(), Some(1));
                    } else {
                        assert_eq!(result.as_bool(), Some(true));
                    }
                }
                1 => {
                    vm.heap_mut().set_field(source, 0, Value::int(8)).unwrap();
                    fault(&mut vm, task, FaultKind::ConcurrentModification);
                }
                2 => {
                    vm.heap_mut().array_push(source, Value::NULL).unwrap();
                    fault(&mut vm, task, FaultKind::ConcurrentModification);
                }
                _ => {
                    vm.cancel_cooperatively(task).unwrap();
                    fault(&mut vm, task, FaultKind::Cancelled);
                }
            }
            assert!(vm.active.equality_cursors.data.is_empty());
        }
    }
}
