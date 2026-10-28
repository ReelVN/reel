//! Script method bindings use the same bounded weak cache as native methods.
use aside_compiler::{AnalysisOptions, analyze_module, lowering, semantic::NoModules};
use aside_vm::{RootOwner, SessionEpoch, Value, bytecode as bc, executor::*};

fn load(source: &str) -> (Vm, bc::FunctionId) {
    let analyzed = analyze_module(source, &AnalysisOptions::default(), &mut NoModules).unwrap();
    let module = analyzed
        .into_module()
        .unwrap_or_else(|errors| panic!("{errors:?}"));
    let compiled = lowering::compile(
        &module,
        &lowering::Linkage::default(),
        lowering::Options::default(),
    )
    .unwrap();
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
    let encoded = compiled.program.encode(bc::Limits::default()).unwrap();
    let program = bc::VerifiedProgram::decode(encoded.as_bytes(), bc::Limits::default()).unwrap();
    let mut vm = Vm::load(
        program,
        bc::RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let task = vm
        .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
        .unwrap();
    finish(&mut vm, task);
    (vm, main)
}
fn finish(vm: &mut Vm, task: TaskId) -> Value {
    let mut budget = SliceBudget::default();
    loop {
        match vm.run_slice(task, budget, || 0).unwrap() {
            RunOutcome::BudgetYield(usage) => {
                budget.work = budget.work.max(usage.required_work);
                budget.allocation_bytes = budget.allocation_bytes.max(usage.required_allocation);
            }
            RunOutcome::Returned(delivery) => {
                let value = vm.delivery_value(delivery).unwrap();
                vm.discard_delivery(delivery).unwrap();
                return value;
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
#[ignore = "release source-to-B01 repeated script method cost"]
fn script_method_call_cost() {
    let source = r#"class Motion {
        let base = 40.75;
        fn sample(self, time: float) -> float { self.base + time }
    }
    fn main() {
        let motion = Motion();
        let mut sum = 0.0;
        for i in 0..20000 { sum += motion.sample(1.25); }
        sum
    }"#;
    for _ in 0..4 {
        let (mut vm, main) = load(source);
        let before = vm.stats();
        let started = std::time::Instant::now();
        let task = vm
            .start(main, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        assert_eq!(finish(&mut vm, task).as_float(), Some(840000.0));
        let elapsed = started.elapsed().as_micros();
        let after = vm.stats();
        eprintln!(
            "script_cache hits={} misses={}",
            after.script_binding_hits - before.script_binding_hits,
            after.script_binding_misses - before.script_binding_misses
        );
        eprintln!(
            "script_method calls=20000 elapsed_us={elapsed} instructions={} work={} added_objects={} added_bytes={} chunks={}",
            after.cumulative_instructions - before.cumulative_instructions,
            after.cumulative_work - before.cumulative_work,
            after.heap.live_objects - before.heap.live_objects,
            after.heap.charged_bytes - before.heap.charged_bytes,
            after.heap.chunk_allocations - before.heap.chunk_allocations
        );
    }
}

fn function(vm: &Vm, name: &str) -> bc::FunctionId {
    bc::FunctionId(
        vm.program()
            .package()
            .functions
            .iter()
            .position(|function| vm.program().package().symbols[function.name.index()] == name)
            .unwrap() as u32,
    )
}
fn invoke(vm: &mut Vm, name: &str, args: &[Value]) -> Value {
    let function = function(vm, name);
    let task = vm
        .start(function, args, RootOwner(1), bc::Context::Ordinary)
        .unwrap();
    finish(vm, task)
}
fn collect(vm: &mut Vm) {
    if vm.heap().phase() == aside_vm::GcPhase::Idle {
        vm.begin_collection().unwrap();
    }
    while vm.heap().phase() != aside_vm::GcPhase::Idle {
        vm.gc_step(aside_vm::GcBudget {
            units: 1,
            ..Default::default()
        })
        .unwrap();
    }
}

#[test]
fn script_method_cache_preserves_receivers_super_static_and_forwarded_call_contracts() {
    let (mut vm, main) = load(
        r#"
        class Base { let value:int; fn init(self,value:int){self.value=value;} fn read(self,extra:int=0){self.value+extra} }
        class Child:Base { fn read(self,extra:int=0){super.read(extra:extra)+2} }
        class Other { static fn read(extra:int=0){40+extra} }
        fn forward(method:fn(int)->int){method(2)}
        fn main(){
            let a=Child(38); let b=Base(41);
            let inherited=a.read; let ordinary=b.read; let shared=Other.read;
            for i in 0..20 {
                if a.read(extra:2)!=42 || b.read()!=41 || Other.read(2)!=42 {throw "receiver";}
            }
            if ordinary()!=41 || shared(2)!=42 {throw "saved methods";}
            forward(inherited)
        }
    "#,
    );
    let task = vm
        .start(main, &[], RootOwner(1), bc::Context::Ordinary)
        .unwrap();
    assert_eq!(finish(&mut vm, task).as_int(), Some(42));
    assert!(vm.stats().script_binding_hits >= 40);
}

#[test]
fn script_method_cache_is_weak_at_gc_cutoff_and_restores_cold() {
    use aside_vm::{
        GcBudget, GcPhase, RootKind,
        snapshot::{SnapshotBudget, SnapshotRegistry},
    };
    let (mut vm, _) = load(
        "class Item {let value=42; fn read(self){self.value}} fn main(){Item()} fn method(item){item.read} fn call(method){method()}",
    );
    let item = invoke(&mut vm, "main", &[]);
    vm.heap_mut()
        .add_root(RootOwner(1), RootKind::Host, item)
        .unwrap();
    let first = invoke(&mut vm, "method", &[item]);
    let first_root = vm
        .heap_mut()
        .add_root(RootOwner(1), RootKind::Host, first)
        .unwrap();
    assert!(invoke(&mut vm, "method", &[item]).same_bits(first));
    collect(&mut vm);
    assert!(invoke(&mut vm, "method", &[item]).same_bits(first));
    vm.heap_mut().release_root(first_root).unwrap();
    vm.begin_collection().unwrap();
    while vm.heap().phase() == GcPhase::Mark {
        vm.gc_step(GcBudget {
            units: 1,
            ..Default::default()
        })
        .unwrap();
    }
    assert!(vm.heap().info(first).is_err());
    let replacement = invoke(&mut vm, "method", &[item]);
    assert!(!replacement.same_bits(first));
    vm.heap_mut()
        .add_root(RootOwner(1), RootKind::Host, replacement)
        .unwrap();
    collect(&mut vm);
    assert_eq!(invoke(&mut vm, "call", &[replacement]).as_int(), Some(42));
    let registry = SnapshotRegistry::default();
    let mut capture = vm.snapshot_begin(&registry).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let image = capture.finish().unwrap();
    let image = aside_vm::snapshot::VmSnapshot::decode(&image.encode().unwrap()).unwrap();
    let mut restore = Vm::restore_begin(
        &image,
        vm.program().clone(),
        bc::RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(2),
        &registry,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (mut restored, map) = restore.finish().unwrap();
    let item = map.value(item).unwrap();
    let saved = map.value(replacement).unwrap();
    assert_eq!(restored.stats().script_binding_hits, 0);
    assert_eq!(restored.stats().script_binding_misses, 0);
    assert_eq!(invoke(&mut restored, "call", &[saved]).as_int(), Some(42));
    let rebound = invoke(&mut restored, "method", &[item]);
    assert!(invoke(&mut restored, "method", &[item]).same_bits(rebound));
    assert_eq!(restored.stats().script_binding_misses, 1);
}
