use super::*;

fn prepared(reference: bool) -> (Vm, TaskId) {
    let mut package = fixtures::helper_native();
    package.functions[0].register_count = 98;
    for index in 0..96 {
        let name = SymbolId(package.symbols.len() as u32);
        package.symbols.push(format!("local_{index}"));
        package.functions[0].bindings.push(BindingDef {
            name,
            ty: TypeId(0),
            mutable: false,
            storage: BindingStorage::Value(Reg(index + 2)),
            initialization: BindingInitialization::Explicit,
        });
    }
    let mut vm = Vm::load(
        verify(package, Limits::default()).unwrap(),
        super::review_tests::echo_registry(),
        VmLimits::default(),
        SessionEpoch(12),
    )
    .unwrap();
    vm.work_probe.single_install_step = reference;
    let task = vm
        .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
        .unwrap();
    let Frame::Script(mut frame) = *vm.active.frames.data.last().unwrap() else {
        unreachable!()
    };
    // Default-prefix temporaries may have occupied explicit local registers.
    for reg in 2..98 {
        vm.put(frame.regs, reg, Value::int(7)).unwrap();
    }
    frame.phase = ScriptPhase::Install { binding: 0 };
    *vm.active.frames.data.last_mut().unwrap() = Frame::Script(frame);
    (vm, task)
}

fn position(vm: &Vm) -> (Option<u32>, Vec<bool>) {
    let Frame::Script(frame) = *vm.active.frames.data.last().unwrap() else {
        unreachable!()
    };
    let cursor = match frame.phase {
        ScriptPhase::Install { binding } => Some(binding),
        ScriptPhase::Body => None,
        _ => panic!("unexpected preparation phase"),
    };
    (
        cursor,
        (2..98)
            .map(|reg| vm.value(frame.regs, reg).is_ok())
            .collect(),
    )
}

#[test]
fn installation_keeps_exact_work_yields_and_initialized_slot_state() {
    for work in [1, 7, 31, 32, 64, 128] {
        let traces = [true, false].map(|reference| {
            let (mut vm, task) = prepared(reference);
            let mut trace = Vec::new();
            loop {
                let RunOutcome::BudgetYield(usage) = vm
                    .run_slice(
                        task,
                        SliceBudget {
                            instructions: 0,
                            work,
                            allocation_bytes: 0,
                            deadline_ns: None,
                        },
                        || 0,
                    )
                    .unwrap()
                else {
                    panic!("expected budget boundary")
                };
                let state = position(&vm);
                let done = state.0.is_none();
                trace.push((usage, state));
                if done {
                    break;
                }
            }
            assert!(
                trace
                    .last()
                    .unwrap()
                    .1
                    .1
                    .iter()
                    .all(|initialized| !initialized)
            );
            trace
        });
        assert_eq!(traces[0], traces[1], "work quota {work}");
    }
}

#[test]
fn installation_stops_at_the_same_soft_deadline_check() {
    for reference in [true, false] {
        let (mut vm, task) = prepared(reference);
        let mut clock_calls = 0;
        let RunOutcome::BudgetYield(usage) = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 0,
                    work: 1024,
                    allocation_bytes: 0,
                    deadline_ns: Some(1),
                },
                || {
                    let value = clock_calls;
                    clock_calls += 1;
                    value
                },
            )
            .unwrap()
        else {
            panic!("expected clock boundary")
        };
        assert_eq!(clock_calls, 2);
        assert_eq!(usage.work, 32);
        assert_eq!(position(&vm).0, Some(32));
    }
}
