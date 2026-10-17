//! Classes cross source, ASB, sliced execution and GC using the shared VM.
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
                if tiny {
                    budget.work = usage.required_work.max(1);
                    budget.allocation_bytes = usage.required_allocation;
                } else {
                    budget.work = budget.work.max(usage.required_work);
                    budget.allocation_bytes =
                        budget.allocation_bytes.max(usage.required_allocation);
                }
            }
            RunOutcome::Returned(id) => {
                let value = vm.delivery_value(id).unwrap();
                vm.discard_delivery(id).unwrap();
                return value;
            }
            RunOutcome::Fault(id) => panic!("class execution fault: {:?}", vm.delivery_fault(id)),
            other => panic!("unexpected class outcome: {other:?}"),
        }
    }
    panic!("class execution made no progress");
}

fn run(source: &str) {
    let analyzed = analyze_module(source, &AnalysisOptions::default(), &mut NoModules).unwrap();
    let module = analyzed
        .into_module()
        .unwrap_or_else(|e| panic!("{source}\n{e:?}"));
    let compiled = lowering::compile(
        &module,
        &lowering::Linkage::default(),
        lowering::Options::default(),
    )
    .unwrap_or_else(|e| panic!("{source}\n{e:?}"));
    let p = compiled.program.package();
    let main = bc::FunctionId(
        p.functions
            .iter()
            .position(|f| p.symbols[f.name.index()] == "main")
            .unwrap() as u32,
    );
    let encoded = bc::encode(p, bc::Limits::default()).unwrap();
    for tiny in [false, true] {
        let program = bc::VerifiedProgram::decode(&encoded, bc::Limits::default()).unwrap();
        let mut vm = Vm::load(
            program,
            bc::RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        let init = vm
            .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
            .unwrap();
        finish(&mut vm, init, tiny);
        let task = vm
            .start(main, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        assert_eq!(
            finish(&mut vm, task, tiny).as_int(),
            Some(42),
            "tiny={tiny}\n{source}"
        );
    }
}

#[test]
fn fields_methods_and_author_data() {
    for source in [
        "class Word { let text:string; let score:int; fn init(self,text,score=42){self.text=text;self.score=score;} fn value(self){self.score} } fn main(){let word:Word=Word(text:\"poem\");let score=word.value;score()}",
        "class Counter {let mut value:int=0; let items=[]; fn add(self,n=1){self.value+=n;} } fn main(){let a=Counter();let b=Counter();a.add(42);a.items.append(1);if b.value==0 && b.items.len()==0 {a.value}else{0}}",
        "fn main(){let seed=40;let C=class {let value=seed;fn read(self){self.value+2}};C().read()}",
        "class Base {let value:int;fn init(self,value){self.value=value;} fn get(self){self.value}} class Child:Base {fn get(self){super.get()+2}} fn main(){let child:Base=Child(40);child.get()}",
        "class Base {let value:int;fn init(self,value){self.value=value;} fn get(self){self.value}} class Child:Base {fn init(self){super.init(40);} fn get(self){let read=||super.get();read()+2}} fn main(){Child().get()}",
        "class Word {static let base:int=40;static let mut calls:int=0;static fn score(){Word.calls+=1;Word.base+Word.calls}} fn main(){Word.score();Word.score()}",
        "class Word {let value=40;static let value=2;}fn main(){Word().value+Word.value}",
        "class Value {let n:int=42;} fn accept(v:Value)->Value{v} fn main(){let value=accept(Value());if value is Value {value.n}else{0}}",
        "class Value {let n:int=42;} fn bad(v){v.n=\"wrong\";} fn main(){let v=Value();let mut caught=false;try {bad(v);}catch e {caught=true;}if caught{v.n}else{0}}",
        "class Base{let mut n=0;fn init(self){self.n+=1;}} class Child:Base{fn init(self){super.init();try{super.init();}catch e{self.n+=41;}}}fn main(){Child().n}",
        "let mut escaped=null;class Broken {let value:int;fn init(self){escaped=self;throw \"broken\";}fn get(self){42}} fn main(){let mut caught=0;try{Broken();}catch e{caught+=1;}try{escaped.value;}catch e{caught+=1;}try{escaped.get();}catch e{caught+=1;}if caught==3{42}else{0}}",
    ] {
        run(source);
    }
}

#[test]
fn cancellation_invalidates_escaped_instance_and_retained_method() {
    let source = "let mut escaped=null;let mut method=null;class Building{let value:int;fn init(self){escaped=self;method=self.read;loop{}}fn read(self){42}}fn make(){Building();}fn published(){escaped!=null}fn field(){escaped.value}fn bound(){method()}";
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
    let p = compiled.program.package();
    let ids = ["make", "published", "field", "bound"].map(|name| {
        bc::FunctionId(
            p.functions
                .iter()
                .position(|f| p.symbols[f.name.index()] == name)
                .unwrap() as u32,
        )
    });
    let encoded = bc::encode(p, bc::Limits::default()).unwrap();
    for cooperative in [false, true] {
        let mut vm = Vm::load(
            bc::VerifiedProgram::decode(&encoded, bc::Limits::default()).unwrap(),
            bc::RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        let init = vm
            .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
            .unwrap();
        finish(&mut vm, init, true);
        let make = vm
            .start(ids[0], &[], RootOwner(2), bc::Context::Ordinary)
            .unwrap();
        assert!(matches!(
            vm.run_slice(make, SliceBudget::default(), || 0).unwrap(),
            RunOutcome::BudgetYield(_)
        ));
        let published = vm
            .start(ids[1], &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        assert_eq!(finish(&mut vm, published, false).as_bool(), Some(true));
        if cooperative {
            vm.cancel_cooperatively(make).unwrap();
            loop {
                match vm.run_slice(make, SliceBudget::default(), || 0).unwrap() {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::Fault(id) => {
                        assert_eq!(
                            vm.delivery_fault(id).unwrap().unwrap().kind,
                            FaultKind::Cancelled
                        );
                        vm.discard_delivery(id).unwrap();
                        break;
                    }
                    other => panic!("cancel outcome: {other:?}"),
                }
            }
        } else {
            vm.cancel(RootOwner(2)).unwrap();
            loop {
                match vm.run_slice(make, SliceBudget::default(), || 0) {
                    Ok(RunOutcome::BudgetYield(_)) => {}
                    Err(VmError::Idle) => break,
                    other => panic!("cancel outcome: {other:?}"),
                }
            }
        }
        for function in &ids[2..] {
            let task = vm
                .start(*function, &[], RootOwner(1), bc::Context::Ordinary)
                .unwrap();
            loop {
                match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
                    RunOutcome::BudgetYield(_) => {}
                    RunOutcome::Fault(id) => {
                        assert_eq!(
                            vm.delivery_fault(id).unwrap().unwrap().kind,
                            FaultKind::ConstructionFailed
                        );
                        vm.discard_delivery(id).unwrap();
                        break;
                    }
                    other => panic!("escaped construction outcome: {other:?}"),
                }
            }
        }
    }
}

#[test]
fn class_payload_charges_match_real_growth_at_inline_and_inheritance_boundaries() {
    let source = r#"
        class Fields {
            let f0 = 0; let f1 = 1; let f2 = 2; let f3 = 3;
            let f4 = 4; let f5 = 5; let f6 = 6; let f7 = 7;
        }
        class FieldsChild: Fields {}
        class Methods {
            fn m0(self) { 0 } fn m1(self) { 1 } fn m2(self) { 2 } fn m3(self) { 3 }
            fn m4(self) { 4 } fn m5(self) { 5 } fn m6(self) { 6 } fn m7(self) { 7 }
        }
        class MethodsChild: Methods { fn m0(self) { 35 } }
        fn main() { FieldsChild().f7 + MethodsChild().m0() }
    "#;
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
    let mut package = compiled.program.package().clone();
    // Eight words put the nominal-type bitset at the same inline boundary.
    package
        .types
        .resize(512, bc::TypeDef::Builtin(bc::BuiltinType::Any));
    let main = bc::FunctionId(
        package
            .functions
            .iter()
            .position(|f| package.symbols[f.name.index()] == "main")
            .unwrap() as u32,
    );
    let mut vm = Vm::load(
        bc::verify(package, bc::Limits::default()).unwrap(),
        bc::RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let mut charged = 0;
    for function in [compiled.entry, main] {
        let task = vm
            .start(function, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let mut allocation_bytes = 0;
        loop {
            let before = vm.heap().stats().chunk_allocations;
            let outcome = vm
                .run_slice(
                    task,
                    SliceBudget {
                        instructions: 1,
                        allocation_bytes,
                        ..SliceBudget::default()
                    },
                    || 0,
                )
                .unwrap();
            let usage = vm.stats().usage;
            let actual =
                (vm.heap().stats().chunk_allocations - before) * aside_vm::VALUE_CHUNK_BYTES;
            assert_eq!(
                usage.allocation_bytes, actual,
                "a phase ending at capacity and an inherited method override do not append"
            );
            charged += actual;
            match outcome {
                RunOutcome::BudgetYield(usage) => allocation_bytes = usage.required_allocation,
                RunOutcome::Returned(id) => {
                    if function == main {
                        assert_eq!(vm.delivery_value(id).unwrap().as_int(), Some(42));
                    }
                    vm.discard_delivery(id).unwrap();
                    break;
                }
                other => panic!("unexpected class outcome: {other:?}"),
            }
        }
    }
    assert!(
        charged > 0,
        "class headers still require real payload chunks"
    );
}

#[test]
fn retained_method_and_typed_field_updates_reuse_storage() {
    let source = "class Counter{let mut value:int=0;fn increment(self){self.value+=1;}}let counter=Counter();let increment=counter.increment;fn main(){for i in 0..10000{increment();}counter.value}";
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
    let p = compiled.program.package();
    let main = bc::FunctionId(
        p.functions
            .iter()
            .position(|f| p.symbols[f.name.index()] == "main")
            .unwrap() as u32,
    );
    let mut vm = Vm::load(
        compiled.program,
        bc::RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let init = vm
        .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
        .unwrap();
    finish(&mut vm, init, false);
    let before = vm.heap().stats().chunk_allocations;
    let started = std::time::Instant::now();
    let task = vm
        .start(main, &[], RootOwner(1), bc::Context::Ordinary)
        .unwrap();
    assert_eq!(finish(&mut vm, task, false).as_int(), Some(10000));
    let chunks = vm.heap().stats().chunk_allocations - before;
    assert_eq!(
        chunks, 0,
        "range and iteration cursor fit inline; retained calls allocate no payload blocks"
    );
    println!(
        "10000 retained method calls with typed field updates: chunks={chunks}, elapsed={:?}",
        started.elapsed()
    );
}

#[test]
fn snapshots_resume_instruction_work_and_exception_cleanup_without_replaying_code() {
    use aside_vm::snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot};
    let source = r#"
        let mut writes = 0;
        class Counter { let items = [1, 2, 3]; fn read(self) { self.items[1] } }
        fn value() {
            let counter = Counter();
            let tuple = (counter, [counter]);
            let get = {count: counter}.get;
            let fallback = get(...["missing", {counter: counter}]);
            if get("count") != counter || fallback.counter != counter { throw "get roots"; }
            let parts = "中::🙂::::".split("::");
            if parts.len() != 4 || parts[1] != "🙂" || parts[3] != "" { throw "split state"; }
            try { throw 7; } catch caught {
                writes += 1;
                try { return counter.read() + 39 + writes; }
                finally { writes += 1; let joined = "x" + "y"; if tuple[1][0] != counter || joined != "xy" { throw 0; } }
            } finally { writes += 1; }
        }
        fn main() { let answer = value(); if writes != 3 { throw writes; } answer }
    "#;
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
    let package = compiled.program.package().clone();
    let main = bc::FunctionId(
        package
            .functions
            .iter()
            .position(|f| package.symbols[f.name.index()] == "main")
            .unwrap() as u32,
    );
    let registry = SnapshotRegistry::default();
    let mut vm = Vm::load(
        compiled.program,
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
    let mut budget = SliceBudget {
        instructions: 1,
        work: 1,
        allocation_bytes: 0,
        deadline_ns: None,
    };
    let mut epochs = 1;
    loop {
        let outcome = vm.run_slice(task, budget, || 0).unwrap();
        if let RunOutcome::BudgetYield(usage) = outcome {
            budget.work = usage.required_work.max(1);
            budget.allocation_bytes = usage.required_allocation;
        }
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture.step(SnapshotBudget::default()).unwrap().complete {}
        let image = VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
        epochs += 1;
        assert!(
            epochs < 4000,
            "execution must make progress across restores"
        );
        let mut restoring = Vm::restore_begin(
            &image,
            bc::verify(package.clone(), bc::Limits::default()).unwrap(),
            bc::RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(epochs),
            &registry,
        )
        .unwrap();
        while !restoring.step(SnapshotBudget::default()).unwrap().complete {}
        let (loaded, map) = restoring.finish().unwrap();
        vm = loaded;
        task = map.task(task).unwrap();
        match outcome {
            RunOutcome::Returned(id) => {
                assert_eq!(
                    vm.delivery_value(map.delivery(id).unwrap())
                        .unwrap()
                        .as_int(),
                    Some(42)
                );
                break;
            }
            RunOutcome::BudgetYield(_) => (),
            other => panic!("unexpected snapshot execution outcome: {other:?}"),
        }
    }
    eprintln!("restored at {} separate execution boundaries", epochs - 1);
}
