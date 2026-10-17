//! Source/B01/VM sorting contracts, including suspended work and ordinary callbacks.
use aside_compiler::{AnalysisOptions, analyze_module, lowering, semantic::NoModules};
use aside_vm::{
    GcBudget, GcPhase, RootKind, RootOwner, SessionEpoch, Value, bytecode as bc,
    executor::*,
    snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot},
};

fn compile(source: &str) -> lowering::Compilation {
    let module = analyze_module(source, &AnalysisOptions::default(), &mut NoModules)
        .unwrap()
        .into_module()
        .unwrap_or_else(|e| panic!("{source}\n{e:?}"));
    lowering::compile(
        &module,
        &lowering::Linkage::default(),
        lowering::Options::default(),
    )
    .unwrap()
}

fn load(source: &str) -> Vm {
    let compiled = compile(source);
    let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
    let mut vm = Vm::load(
        bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap(),
        bc::RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let task = vm
        .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
        .unwrap();
    finish(&mut vm, task, false);
    vm
}

fn start(vm: &mut Vm, name: &str, args: &[Value]) -> TaskId {
    let p = vm.program().package();
    let id = bc::FunctionId(
        p.functions
            .iter()
            .position(|f| p.symbols[f.name.index()] == name)
            .unwrap() as u32,
    );
    vm.start(id, args, RootOwner(1), bc::Context::Ordinary)
        .unwrap()
}

fn tiny_budget() -> SliceBudget {
    SliceBudget {
        instructions: 1,
        work: 1,
        allocation_bytes: 0,
        deadline_ns: None,
    }
}

fn collect(vm: &mut Vm) {
    if vm.heap().phase() == GcPhase::Idle {
        vm.begin_collection().unwrap();
    }
    vm.gc_step(GcBudget {
        units: 1,
        ..Default::default()
    })
    .unwrap();
}

fn next(vm: &mut Vm, task: TaskId, tiny: bool) -> RunOutcome {
    let mut budget = if tiny {
        tiny_budget()
    } else {
        SliceBudget::default()
    };
    for _ in 0..1_000_000 {
        let result = vm.run_slice(task, budget, || 0).unwrap();
        if tiny {
            collect(vm);
        }
        match result {
            RunOutcome::BudgetYield(usage) => {
                if tiny {
                    budget.work = usage.required_work.max(1);
                    budget.allocation_bytes = usage.required_allocation;
                } else {
                    budget.work = budget.work.max(usage.required_work);
                    budget.allocation_bytes =
                        budget.allocation_bytes.max(usage.required_allocation);
                }
            }
            _ => return result,
        }
    }
    panic!("sort must make progress");
}

fn finish(vm: &mut Vm, task: TaskId, tiny: bool) -> Value {
    match next(vm, task, tiny) {
        RunOutcome::Returned(id) => {
            let value = vm.delivery_value(id).unwrap();
            vm.discard_delivery(id).unwrap();
            value
        }
        RunOutcome::Fault(id) => panic!("sort fault: {:?}", vm.delivery_fault(id)),
        other => panic!("sort outcome: {other:?}"),
    }
}

#[test]
fn default_order_stability_callbacks_and_atomic_failures_survive_small_slices_and_gc() {
    let prefix = "字".repeat(80);
    let source = format!(
        r#"
        class ByKey {{ let direction = 1; fn compare(self, a, b) {{ (a.key - b.key) * self.direction }} }}
        fn main() {{
            let empty = []; if empty.sort() != empty {{ throw "empty identity"; }}
            let one = [9]; if one.sort(null) != one || one[0] != 9 {{ throw "singleton"; }}
            let numbers = [9007199254740993, -2, 9007199254740992.0, 1.0, 1, 0];
            if numbers.sort() != numbers {{ throw "identity"; }}
            if numbers[0] != -2 || numbers[1] != 0 || numbers[2] != 1.0 || numbers[3] != 1
                || numbers[4] != 9007199254740992 || numbers[5] != 9007199254740993 {{ throw "numeric order"; }}
            let strings = ["{prefix}🙂", "z", "{prefix}a", "", "{prefix}"];
            strings.sort();
            if strings[0] != "" || strings[1] != "z" || strings[2] != "{prefix}"
                || strings[3] != "{prefix}a" || strings[4] != "{prefix}🙂" {{ throw "text order"; }}
            let items = [{{key: 2, id: 0}}, {{key: 1, id: 1}}, {{key: 2, id: 2}}, {{key: 1, id: 3}}];
            let first = items[0];
            items.sort(compare: ByKey().compare);
            if items[0].id != 1 || items[1].id != 3 || items[2] != first || items[3].id != 2 {{ throw "stable method"; }}
            let direction = -1;
            items.sort(|a, b| (a.key - b.key) * direction);
            if items[0] != first || items[1].id != 2 || items[2].id != 1 || items[3].id != 3 {{ throw "stable closure"; }}
            let source = [3, 2, 1];
            let mut caught = 0;
            try {{ source.sort(|a, b| {{ throw "compare"; }}); }} catch e {{ if e == "compare" {{ caught += 1; }} }}
            try {{ source.sort(|a, b| true); }} catch e {{ caught += 1; }}
            try {{ source.sort(42); }} catch e {{ caught += 1; }}
            if caught != 3 || source[0] != 3 || source[1] != 2 || source[2] != 1 {{ throw "failed copy published"; }}
            try {{ source.sort(|a, b| {{ source[1] = 99; a - b }}); }} catch e {{ caught += 1; }}
            if caught != 4 || source[0] != 3 || source[1] != 99 || source[2] != 1 {{ throw "concurrent write overwritten"; }}
            let mixed = [2, "1"];
            try {{ mixed.sort(); }} catch e {{ caught += 1; }}
            if caught != 5 || mixed[0] != 2 || mixed[1] != "1" {{ throw "invalid default"; }}
            42
        }}
    "#
    );
    for tiny in [false, true] {
        let mut vm = load(&source);
        let task = start(&mut vm, "main", &[]);
        assert_eq!(finish(&mut vm, task, tiny).as_int(), Some(42));
    }
}

#[test]
fn snapshots_at_every_execution_boundary_preserve_comparator_effects_and_sort_cursors() {
    let source = r#"
        fn main() {
            let a = [4, 1, 3, 2];
            let mut calls = "";
            a.sort(|x, y| { calls += f"{x}{y},"; x - y });
            if calls != "41,32,12,42,43," { throw calls; }
            for i in 0..4 { if a[i] != i + 1 { throw "order"; } }
            let b = ["zz", "za"];
            b.sort();
            if b[0] != "za" || b[1] != "zz" { throw "text"; }
            42
        }
    "#;
    let mut vm = load(source);
    let package = vm.program().package().clone();
    let registry = SnapshotRegistry::default();
    let mut task = start(&mut vm, "main", &[]);
    let mut budget = tiny_budget();
    for epoch in 2..10_000 {
        let result = vm.run_slice(task, budget, || 0).unwrap();
        collect(&mut vm);
        if let RunOutcome::BudgetYield(usage) = result {
            budget.work = usage.required_work.max(1);
            budget.allocation_bytes = usage.required_allocation;
        }
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture.step(SnapshotBudget { units: 3 }).unwrap().complete {}
        let image = VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
        let mut restore = Vm::restore_begin(
            &image,
            bc::verify(package.clone(), bc::Limits::default()).unwrap(),
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
        match result {
            RunOutcome::Returned(id) => {
                assert_eq!(
                    vm.delivery_value(map.delivery(id).unwrap())
                        .unwrap()
                        .as_int(),
                    Some(42)
                );
                return;
            }
            RunOutcome::BudgetYield(_) => (),
            other => panic!("snapshot outcome: {other:?}"),
        }
    }
    panic!("restored sort must finish");
}

#[test]
fn cancellation_and_other_tasks_cannot_publish_a_partial_sort() {
    let source = r#"
        let values = [3, 1, 2]; let mut comparing = false; let mut proceed = false;
        fn sorting() { values.sort(|a, b| { comparing = true; while !proceed {} a - b }); }
        fn observed() { comparing }
        fn mutate() { values[1] = 99; proceed = true; }
        fn unchanged() { values[0] == 3 && values[1] == 1 && values[2] == 2 }
        fn preserved() { values[0] == 3 && values[1] == 99 && values[2] == 2 }
    "#;
    for cancel in [true, false] {
        let mut vm = load(source);
        let task = start(&mut vm, "sorting", &[]);
        assert!(matches!(
            vm.run_slice(task, SliceBudget::default(), || 0).unwrap(),
            RunOutcome::BudgetYield(_)
        ));
        let check = start(&mut vm, "observed", &[]);
        assert_eq!(finish(&mut vm, check, true).as_bool(), Some(true));
        if cancel {
            vm.cancel_cooperatively(task).unwrap();
        } else {
            let mutate = start(&mut vm, "mutate", &[]);
            finish(&mut vm, mutate, true);
        }
        let RunOutcome::Fault(id) = next(&mut vm, task, true) else {
            panic!("sort must fault");
        };
        assert_eq!(
            vm.delivery_fault(id).unwrap().unwrap().kind,
            if cancel {
                FaultKind::Cancelled
            } else {
                FaultKind::ConcurrentModification
            }
        );
        vm.discard_delivery(id).unwrap();
        let check = start(&mut vm, if cancel { "unchanged" } else { "preserved" }, &[]);
        assert_eq!(finish(&mut vm, check, true).as_bool(), Some(true));
    }
}

#[test]
fn reverse_input_has_bounded_merge_comparisons() {
    let mut vm = load(
        r#"
        fn main() {
            let values = []; for i in 0..4096 { values.append(4095 - i); }
            let mut calls = 0;
            values.sort(|a, b| { calls += 1; a - b });
            for i in 0..4096 { if values[i] != i { throw "order"; } }
            if calls > 4096 * 12 { throw "quadratic comparisons"; }
            calls
        }
    "#,
    );
    let task = start(&mut vm, "main", &[]);
    assert_eq!(finish(&mut vm, task, false).as_int(), Some(4096 * 6));
}

#[test]
fn native_comparators_share_call_binding_and_cannot_wait() {
    let source = r#"
        let values = [3, 1, 2];
        fn sorting(compare) { values.sort(compare); values[0] * 100 + values[1] * 10 + values[2] }
        fn awaiting(request) { values.sort(|a, b| { await request; a - b }); }
        fn unchanged() { values[0] == 3 && values[1] == 1 && values[2] == 2 }
    "#;
    let compiled = compile(source);
    let mut package = compiled.program.package().clone();
    let any = bc::TypeId(
        package
            .types
            .iter()
            .position(|t| *t == bc::TypeDef::Builtin(bc::BuiltinType::Any))
            .unwrap() as u32,
    );
    let mut support = bc::RuntimeSupport::default();
    let mut functions = Vec::new();
    for (symbol, names) in [
        ("host::compare", &["receiver", "a", "b"][..]),
        ("host::wait", &["request"][..]),
    ] {
        let name = bc::SymbolId(package.symbols.len() as u32);
        package.symbols.push(symbol.into());
        let parameters = names
            .iter()
            .map(|&name| {
                let symbol = if let Some(index) = package.symbols.iter().position(|s| s == name) {
                    bc::SymbolId(index as u32)
                } else {
                    let symbol = bc::SymbolId(package.symbols.len() as u32);
                    package.symbols.push(name.into());
                    symbol
                };
                bc::Parameter {
                    name: symbol,
                    ty: any,
                    mutable: false,
                    default: None,
                }
            })
            .collect();
        let contract = bc::Signature {
            parameters,
            rest: None,
            return_type: any,
            effects: bc::Effects::ALL,
            property_bag_tail: None,
        };
        let signature = bc::SignatureId(package.signatures.len() as u32);
        package.signatures.push(contract.clone());
        let native = bc::NativeId(package.natives.len() as u32);
        package.natives.push(bc::NativeDef {
            symbol: name,
            signature,
            abi_version: 1,
            capabilities: vec![],
        });
        let function = bc::FunctionId(package.functions.len() as u32);
        package.functions.push(bc::FunctionDef {
            module: bc::ModuleId(0),
            name,
            kind: bc::FunctionKind::Native(native),
            signature,
            context: bc::Context::Ordinary,
            register_count: 0,
            captures: vec![],
            bindings: vec![],
            descriptors: vec![],
            code: vec![],
            regions: vec![],
            source_map: vec![],
        });
        functions.push(function);
        support.natives.push(bc::NativeSupport {
            symbol: symbol.into(),
            abi_version: 1,
            signature: contract,
            required_capabilities: vec![],
        });
    }
    let bytes = bc::encode(&package, bc::Limits::default()).unwrap();
    for mode in 0..3 {
        let mut vm = Vm::load(
            bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap(),
            support.clone(),
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        let init = vm
            .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
            .unwrap();
        finish(&mut vm, init, true);
        let fields = vm.heap_mut().alloc_table().unwrap();
        let host = vm.heap_mut().alloc_host(fields, &[], None).unwrap();
        let callable = vm.bind_native(host, functions[0]).unwrap();
        vm.bind_awaitable(host, bc::NativeId(1)).unwrap();
        let root = vm
            .heap_mut()
            .add_root(RootOwner(1), RootKind::Host, callable)
            .unwrap();
        let task = start(
            &mut vm,
            if mode == 2 { "awaiting" } else { "sorting" },
            &[if mode == 2 { host } else { callable }],
        );
        let mut calls = 0;
        loop {
            match next(&mut vm, task, true) {
                RunOutcome::HostCall(ticket) => {
                    assert_ne!(mode, 2, "await is prohibited before adapter dispatch");
                    calls += 1;
                    let args = vm.host_arguments(ticket).unwrap();
                    assert!(args[0].same_bits(host));
                    let order = args[1].as_int().unwrap() - args[2].as_int().unwrap();
                    let response = if mode == 1 {
                        HostResponse::Pending(ticket.wait_id())
                    } else {
                        HostResponse::Ready(Value::int(order))
                    };
                    vm.respond(ticket, response).unwrap();
                }
                RunOutcome::Returned(id) => {
                    assert_eq!(mode, 0);
                    assert_eq!(vm.delivery_value(id).unwrap().as_int(), Some(123));
                    assert_eq!(calls, 3);
                    vm.discard_delivery(id).unwrap();
                    break;
                }
                RunOutcome::Fault(id) => {
                    assert_ne!(mode, 0);
                    assert_eq!(
                        vm.delivery_fault(id).unwrap().unwrap().kind,
                        FaultKind::Effect
                    );
                    vm.discard_delivery(id).unwrap();
                    let check = start(&mut vm, "unchanged", &[]);
                    assert_eq!(finish(&mut vm, check, true).as_bool(), Some(true));
                    break;
                }
                other => panic!("native sort outcome: {other:?}"),
            }
        }
        vm.heap_mut().release_root(root).unwrap();
    }
}
