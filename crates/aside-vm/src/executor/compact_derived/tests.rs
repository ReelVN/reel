use super::*;
use crate::snapshot::{SnapshotBudget, SnapshotRegistry};

fn prepared(generic: bool) -> (Vm, Value, Value, RuntimeSupport) {
    prepared_expression(generic, false)
}
fn prepared_expression(generic: bool, calls: bool) -> (Vm, Value, Value, RuntimeSupport) {
    prepared_options(generic, calls, false)
}
fn prepared_options(
    generic: bool,
    calls: bool,
    generic_parameters: bool,
) -> (Vm, Value, Value, RuntimeSupport) {
    prepared_storage(generic, calls, generic_parameters, false)
}
fn prepared_storage(
    generic: bool,
    calls: bool,
    generic_parameters: bool,
    packed: bool,
) -> (Vm, Value, Value, RuntimeSupport) {
    let mut p = fixtures::helper_native();
    p.signatures[0].parameters[0].ty = TypeId(0);
    p.signatures[0].return_type = TypeId(0);
    p.signatures[0].effects = Effects(Effects::ALLOCATE);
    p.signatures[1].return_type = TypeId(0);
    p.functions.truncate(1);
    let f = &mut p.functions[0];
    f.register_count = 4;
    f.captures = vec![CaptureKind::Value];
    f.source_map.clear();
    f.descriptors.clear();
    f.code = vec![
        Instruction::new(Opcode::CaptureRead, 0, 0, 0),
        Instruction::new(Opcode::MemberGet, 1, 0, 4),
        Instruction::new(Opcode::LoadConst, 2, 1, 0),
        Instruction::new(Opcode::Div, 3, 2, 1),
        Instruction::new(Opcode::Return, 3, 0, 0),
    ];
    let mut native = f.clone();
    native.kind = FunctionKind::Native(NativeId(0));
    native.name = SymbolId(3);
    native.signature = SignatureId(0);
    native.captures.clear();
    native.code.clear();
    native.register_count = 0;
    p.functions.push(native);
    let symbol = SymbolId(p.symbols.len() as u32);
    p.symbols.push("value".into());
    let mut downstream = p.functions[0].clone();
    downstream.code[1].operands[2] = symbol.0;
    downstream.code[3].opcode = Opcode::Add as u16;
    p.functions.push(downstream);
    let mut text = p.functions[0].clone();
    text.code = vec![
        Instruction::new(Opcode::CaptureRead, 0, 0, 0),
        Instruction::new(Opcode::MemberGet, 1, 0, 4),
        Instruction::new(Opcode::TextNew, 2, 0, 0),
        Instruction::new(Opcode::TextPush, 2, 1, 0),
        Instruction::new(Opcode::TextFinish, 3, 2, 0),
        Instruction::new(Opcode::Return, 3, 0, 0),
    ];
    p.functions.push(text);
    let mut support = review_tests::echo_registry();
    support.natives[0].signature = p.signatures[0].clone();
    if calls {
        let method = SymbolId(p.symbols.len() as u32);
        p.symbols.push("sample".into());
        let native_name = SymbolId(p.symbols.len() as u32);
        p.symbols.push("host::sample".into());
        let mut signature = p.signatures[0].clone();
        let mut parameter = signature.parameters[0].clone();
        parameter.name = method;
        signature.parameters.push(parameter);
        p.signatures.push(signature.clone());
        let mut native = p.natives[0].clone();
        native.symbol = native_name;
        native.signature = SignatureId(2);
        p.natives.push(native);
        let mut function = p.functions[1].clone();
        function.kind = FunctionKind::Native(NativeId(1));
        function.name = native_name;
        function.signature = SignatureId(2);
        p.functions.push(function);
        let mut registration = support.natives[0].clone();
        registration.symbol = "host::sample".into();
        registration.signature = signature;
        support.natives.push(registration);
        let f = &mut p.functions[0];
        f.register_count = 6;
        f.source_map = vec![SourceMapEntry {
            pc: Pc(8),
            span: SourceSpan {
                file: SourceId(0),
                start: 12,
                end: 22,
            },
        }];
        f.code = vec![
            Instruction::new(Opcode::CaptureRead, 0, 0, 0),
            Instruction::new(Opcode::MemberGet, 1, 0, 4),
            Instruction::new(Opcode::LoadConst, 2, 1, 0),
            Instruction::new(Opcode::Div, 3, 2, 1),
            Instruction::new(Opcode::LoadConst, 4, 2, 0),
            Instruction::new(Opcode::Ge, 5, 1, 4),
            Instruction::new(Opcode::BranchBool, 5, 7, 9),
            Instruction::new(Opcode::MemberGet, 4, 0, method.0),
            Instruction::new(Opcode::Call, 3, 0, 0),
            Instruction::new(Opcode::Return, 3, 0, 0),
        ];
        f.descriptors = vec![Descriptor::Call(CallDesc {
            contract: None,
            target: CallTarget::Register(Reg(4)),
            arguments: Arguments::Window(RegWindow {
                start: Reg(3),
                len: 1,
            }),
            names: Vec::new(),
        })];
    }
    let program = verify(p, Limits::default()).unwrap();
    let program = if packed {
        VerifiedProgram::decode(
            program.encode(Limits::default()).unwrap().as_bytes(),
            Limits::default(),
        )
        .unwrap()
    } else {
        program
    };
    let program = if generic {
        program.without_compact_derived_plans()
    } else {
        program
    };
    let program = if generic_parameters {
        program.without_native_call_plans()
    } else {
        program
    };
    let mut vm = Vm::load(
        program,
        support.clone(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    assert_eq!(vm.program.compact_derived(FunctionId(0)), !generic);
    let fields = vm.heap.alloc_table().unwrap();
    let name = vm.heap.alloc_string("x").unwrap();
    vm.heap.table_store(fields, 0, name, Value::int(1)).unwrap();
    let source = vm.heap.alloc_host(fields, &[], None).unwrap();
    vm.heap.observe_host_properties(source, 0, 1).unwrap();
    if calls {
        let method = vm.bind_native(source, FunctionId(4)).unwrap();
        let key = vm.heap.alloc_string("sample").unwrap();
        vm.heap.table_store(fields, 1, key, method).unwrap();
    }
    let closure = vm
        .heap
        .alloc_record(
            ObjectKind::Closure,
            &[Value::int(0), Value::int(1), Value::int(0), source],
        )
        .unwrap();
    let task = vm
        .start(FunctionId(1), &[closure], RootOwner(1), Context::Ordinary)
        .unwrap();
    let outcome = drive(&mut vm, task, false).0;
    let RunOutcome::Returned(delivery) = outcome else {
        panic!("initial derive: {outcome:?}")
    };
    let reference = vm.delivery_value(delivery).unwrap();
    let output = vm.heap.property_ref(reference).unwrap().0.host;
    assert_eq!(vm.compact_output(output).unwrap(), !generic);
    (vm, output, source, support)
}
fn drive(vm: &mut Vm, task: TaskId, tiny: bool) -> (RunOutcome, u64, u64, u32, u32, u32) {
    let mut work = 0;
    let mut instructions = 0;
    let mut ranges = 0;
    let mut frames = 0;
    let mut values = 0;
    loop {
        let budget = if tiny {
            SliceBudget {
                instructions: 1,
                work: vm.last_usage.required_work.max(1),
                allocation_bytes: vm.last_usage.required_allocation,
                ..Default::default()
            }
        } else {
            SliceBudget::default()
        };
        let result = vm.run_slice(task, budget, || 0).unwrap();
        work += vm.last_usage.work;
        instructions += u64::from(vm.last_usage.instructions);
        ranges = ranges.max(vm.stats().slots.active_ranges);
        frames = frames.max(vm.stats().frames);
        values = values.max(vm.stats().slots.active_values);
        match result {
            RunOutcome::HostCall(ticket) if ticket.native == NativeId(1) => {
                let value = vm.host_arguments(ticket).unwrap()[1];
                vm.respond(ticket, HostResponse::Ready(value)).unwrap();
            }
            RunOutcome::HostCall(ticket) => vm.respond_derive(ticket).unwrap(),
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::Returned(_) | RunOutcome::Fault(_) => {
                return (result, work, instructions, ranges, frames, values);
            }
            other => panic!("{other:?}"),
        }
        if tiny {
            review_tests::gc(vm);
        }
    }
}
#[test]
fn scheduled_compact_derives_track_properties_and_preserve_both_snapshot_stages() {
    for stage in 0..3 {
        let (mut vm, output, source, support) = prepared(false);
        vm.heap
            .publish_host_property(
                crate::PropertyId {
                    host: source,
                    field: 0,
                },
                Value::int(2),
            )
            .unwrap();
        assert!(vm.heap.next_derived().unwrap().unwrap().same_bits(output));
        let mut task = vm.start_derived(output, RootOwner(1)).unwrap();
        if stage != 0 {
            loop {
                if matches!(vm.active.frames.data.last(), Some(Frame::Native(_))) {
                    break;
                }
                assert!(matches!(
                    vm.run_slice(
                        task,
                        SliceBudget {
                            instructions: 1,
                            work: vm.last_usage.required_work.max(1),
                            allocation_bytes: vm.last_usage.required_allocation,
                            ..Default::default()
                        },
                        || 0
                    )
                    .unwrap(),
                    RunOutcome::BudgetYield(_)
                ));
            }
        }
        if stage == 2 {
            // A callback response may admit args before bound growth is denied.
            // The retryable ticket still borrows its original closure root.
            let Some(Frame::Native(frame)) = vm.active.frames.data.last().copied() else {
                panic!("native tail")
            };
            vm.slots.grow(&mut vm.heap, frame.args, 2).unwrap();
        }
        review_tests::collect(&mut vm);
        let registry = SnapshotRegistry::default();
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let image = capture.finish().unwrap();
        let mut restore = Vm::restore_begin(
            &image,
            vm.program().clone(),
            support,
            vm.limits,
            SessionEpoch(2),
            &registry,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (restored, map) = restore.finish().unwrap();
        vm = restored;
        task = map.task(task).unwrap();
        let output = vm.reactive_task_output(task).unwrap().unwrap();
        assert!(matches!(
            drive(&mut vm, task, true).0,
            RunOutcome::Returned(_)
        ));
        assert_eq!(
            vm.heap
                .host_property(crate::PropertyId {
                    host: output,
                    field: 0
                })
                .unwrap()
                .as_int(),
            Some(20)
        );
    }
}
#[test]
fn compact_fault_retains_the_logical_native_and_script_stack() {
    let mut stacks = Vec::new();
    for generic in [true, false] {
        let (mut vm, output, source, _) = prepared(generic);
        vm.heap
            .publish_host_property(
                crate::PropertyId {
                    host: source,
                    field: 0,
                },
                Value::int(0),
            )
            .unwrap();
        let task = vm.start_derived(output, RootOwner(1)).unwrap();
        let RunOutcome::Fault(delivery) = drive(&mut vm, task, true).0 else {
            panic!("division fault")
        };
        let info = vm.delivery_fault(delivery).unwrap().unwrap();
        let mut cursor = vm.heap.fault_stack(info.id).unwrap();
        let mut entries = Vec::new();
        while let Some(entry) = cursor.next(&vm.heap).unwrap() {
            entries.push((entry.location, entry.module, entry.kind));
        }
        stacks.push((info.code, info.location, entries));
        assert!(!vm.heap.reactive_evaluating());
    }
    assert_eq!(stacks[0], stacks[1]);
}
#[test]
#[ignore = "release differential standard derived execution measurement"]
fn compact_derived_cost() {
    for generic in [true, false] {
        let (mut vm, output, _, _) = prepared(generic);
        let mut work = 0;
        let mut instructions = 0;
        let mut ranges = 0;
        let mut frames = 0;
        let base_ranges = vm.stats().slots.active_ranges;
        let base_values = vm.stats().slots.active_values;
        let sample = vm.start_derived(output, RootOwner(1)).unwrap();
        let (result, _, _, r, f, values) = drive(&mut vm, sample, true);
        ranges = ranges.max(r);
        frames = frames.max(f);
        let RunOutcome::Returned(delivery) = result else {
            panic!("sample")
        };
        vm.discard_delivery(delivery).unwrap();
        let entries = vm.native_entries;
        let commits = vm.native_resumes;
        let start = std::time::Instant::now();
        for _ in 0..20000 {
            let task = vm.start_derived(output, RootOwner(1)).unwrap();
            let (result, w, i, r, f, _) = drive(&mut vm, task, false);
            work += w;
            instructions += i;
            ranges = ranges.max(r);
            frames = frames.max(f);
            let RunOutcome::Returned(delivery) = result else {
                panic!("derived result")
            };
            vm.discard_delivery(delivery).unwrap();
        }
        let elapsed = start.elapsed().as_micros();
        let entries = vm.native_entries - entries;
        let commits = vm.native_resumes - commits;
        assert_eq!(entries, if generic { 20000 } else { 0 });
        assert_eq!(commits, 20000);
        eprintln!(
            "compact_derived generic={generic} evaluations=20000 elapsed_us={} work={work} instructions={instructions} peak_ranges={ranges} peak_frames={frames} peak_root_values={values} base_ranges={base_ranges} base_root_values={base_values} entries={entries} commits={commits}",
            elapsed
        );
    }
}

#[test]
fn lazy_compact_reads_refresh_upstream_without_reentering_the_native() {
    let (mut vm, upstream, source, _) = prepared(false);
    let reference = vm
        .heap
        .alloc_property_ref(
            crate::PropertyId {
                host: upstream,
                field: 0,
            },
            false,
        )
        .unwrap();
    let closure = vm
        .heap
        .alloc_record(
            ObjectKind::Closure,
            &[Value::int(2), Value::int(1), Value::int(0), reference],
        )
        .unwrap();
    let initial = vm
        .start(FunctionId(1), &[closure], RootOwner(1), Context::Ordinary)
        .unwrap();
    let RunOutcome::Returned(delivery) = drive(&mut vm, initial, true).0 else {
        panic!("downstream initial")
    };
    let downstream = vm
        .heap
        .property_ref(vm.delivery_value(delivery).unwrap())
        .unwrap()
        .0
        .host;
    vm.heap
        .publish_host_property(
            crate::PropertyId {
                host: source,
                field: 0,
            },
            Value::int(2),
        )
        .unwrap();
    let entries = vm.native_entries;
    #[cfg(feature = "execution-counts")]
    let before = vm.execution_counts();
    let task = vm.start_derived(downstream, RootOwner(1)).unwrap();
    assert!(matches!(
        drive(&mut vm, task, true).0,
        RunOutcome::Returned(_)
    ));
    assert_eq!(vm.native_entries, entries);
    #[cfg(feature = "execution-counts")]
    {
        let counts = vm.execution_counts().checked_delta(&before).unwrap();
        assert_eq!(counts.derived_starts, 2);
        assert_eq!(counts.compact_derived_starts, 2);
        assert_eq!(counts.script_entries, 2);
        assert_eq!(counts.compact_commits, 2);
        assert_eq!(counts.install_bindings_visited, 0);
        assert_eq!(counts.parameters_prepared, 0);
        assert_eq!(counts.bytecode_execute_steps, 10);
        assert_eq!(counts.reactive_read_suspensions, 1);
        assert_eq!(counts.instructions, 11);
        assert_eq!(counts.opcode_admissions().map(|(_, n)| n).sum::<u64>(), 11);
    }
    assert_eq!(
        vm.heap
            .host_property(crate::PropertyId {
                host: downstream,
                field: 0
            })
            .unwrap()
            .as_int(),
        Some(61)
    );
    assert_eq!(
        vm.heap
            .host_property(crate::PropertyId {
                host: upstream,
                field: 0
            })
            .unwrap()
            .as_int(),
        Some(20)
    );
}
#[test]
fn cancel_compact_capture_before_entry_and_during_expression_releases_ownership() {
    for begin in [false, true] {
        let (mut vm, output, _, _) = prepared(false);
        let task = vm.start_derived(output, RootOwner(1)).unwrap();
        if begin {
            loop {
                if matches!(vm.active.frames.data.last(),Some(Frame::Script(s)) if matches!(s.phase,ScriptPhase::Body))
                {
                    break;
                }
                assert!(matches!(
                    vm.run_slice(
                        task,
                        SliceBudget {
                            instructions: 0,
                            work: vm.last_usage.required_work.max(1),
                            allocation_bytes: vm.last_usage.required_allocation,
                            ..Default::default()
                        },
                        || 0
                    )
                    .unwrap(),
                    RunOutcome::BudgetYield(_)
                ));
            }
            assert!(vm.heap.reactive_evaluating());
        }
        vm.cancel_task(task).unwrap();
        assert!(matches!(
            vm.run_slice(task, SliceBudget::default(), || 0),
            Err(VmError::Idle)
        ));
        assert!(!vm.heap.reactive_evaluating());
        review_tests::collect(&mut vm);
        assert!(vm.active.frames.data.is_empty());
    }
}

#[test]
fn compact_interpolation_keeps_original_text_execution_and_gc_roots() {
    let (mut vm, _, source, _) = prepared(false);
    let closure = vm
        .heap
        .alloc_record(
            ObjectKind::Closure,
            &[Value::int(3), Value::int(1), Value::int(0), source],
        )
        .unwrap();
    let initial = vm
        .start(FunctionId(1), &[closure], RootOwner(1), Context::Ordinary)
        .unwrap();
    let RunOutcome::Returned(delivery) = drive(&mut vm, initial, true).0 else {
        panic!("text initial")
    };
    let output = vm
        .heap
        .property_ref(vm.delivery_value(delivery).unwrap())
        .unwrap()
        .0
        .host;
    assert!(vm.compact_output(output).unwrap());
    let text = "derived text ".repeat(100);
    let input = vm.heap.alloc_string(&text).unwrap();
    vm.heap
        .publish_host_property(
            crate::PropertyId {
                host: source,
                field: 0,
            },
            input,
        )
        .unwrap();
    let task = vm.start_derived(output, RootOwner(1)).unwrap();
    assert!(matches!(
        drive(&mut vm, task, true).0,
        RunOutcome::Returned(_)
    ));
    let result = vm
        .heap
        .host_property(crate::PropertyId {
            host: output,
            field: 0,
        })
        .unwrap();
    let mut bytes = vec![0; text.len()];
    vm.heap.read_bytes(result, 0, &mut bytes).unwrap();
    assert_eq!(bytes, text.as_bytes());
}

#[test]
fn compact_commit_materializes_its_argument_only_for_a_new_callback() {
    let (mut vm, output, source, _) = prepared(false);
    let task = vm.start_derived(output, RootOwner(1)).unwrap();
    let ticket = loop {
        match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
            RunOutcome::HostCall(ticket) => break ticket,
            RunOutcome::BudgetYield(_) => {}
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(ticket.input, NativeInput::CallbackReturned);
    assert!(vm.ticket(ticket).unwrap().inline_argument);
    let original = vm.host_arguments(ticket).unwrap()[0];
    assert!(
        vm.host_provided_arguments(ticket)
            .unwrap()
            .get(0)
            .unwrap()
            .1
            .same_bits(original)
    );
    let alternate = vm
        .heap
        .alloc_record(
            ObjectKind::Closure,
            &[Value::int(3), Value::int(1), Value::int(0), source],
        )
        .unwrap();
    vm.respond(
        ticket,
        HostResponse::CallScript {
            closure: alternate,
            arguments: &[],
            continuation: Continuation {
                version: 1,
                stage: 0,
                values: &[output, Value::bool(true)],
                faults: FaultRetention::Preserve,
            },
        },
    )
    .unwrap();
    let ticket = loop {
        match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
            RunOutcome::HostCall(ticket) => break ticket,
            RunOutcome::BudgetYield(_) => {}
            other => panic!("{other:?}"),
        }
    };
    let native = vm.ticket(ticket).unwrap();
    assert!(!native.inline_argument);
    assert_eq!(vm.slots.len(native.args).unwrap(), 2);
    assert_eq!(vm.slots.len(native.bound).unwrap(), 1);
    assert!(vm.host_arguments(ticket).unwrap()[0].same_bits(original));
    assert!(
        vm.host_provided_arguments(ticket)
            .unwrap()
            .get(0)
            .unwrap()
            .1
            .same_bits(original)
    );
    vm.respond_derive(ticket).unwrap();
    assert!(matches!(
        drive(&mut vm, task, true).0,
        RunOutcome::Returned(_)
    ));
}

#[test]
fn differential_baseline_keeps_every_recomputation_on_its_selected_path() {
    for generic in [true, false] {
        let (mut vm, output, _, _) = prepared(generic);
        let entries = vm.native_entries;
        let commits = vm.native_resumes;
        for _ in 0..3 {
            let task = vm.start_derived(output, RootOwner(1)).unwrap();
            let RunOutcome::Returned(delivery) = drive(&mut vm, task, false).0 else {
                panic!("derived")
            };
            vm.discard_delivery(delivery).unwrap();
        }
        assert_eq!(vm.native_entries - entries, if generic { 3 } else { 0 });
        assert_eq!(vm.native_resumes - commits, 3);
    }
}

#[path = "calls.rs"]
mod calls;
