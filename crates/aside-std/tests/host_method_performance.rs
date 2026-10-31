//! Engine-independent method-call workloads using the real curve host adapter.
//! Construction is separate from steady execution; timings include VM dispatch,
//! native adapter work, delivery release and any explicitly interleaved GC.
mod support;

use aside_host::{aside_compiler, aside_vm};
use aside_vm::{GcBudget, GcPhase, RootOwner, SessionEpoch, Value, bytecode as bc, executor::*};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, fmt::Write as _, time::Instant};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const SOURCE: &str = r#"
use std::math::curve;
class Motion {
    let mut values: any = null;
    fn init(self, values) { self.values = values; }
    fn sample(self, seconds) { self.values.sample(seconds) }
}
fn construct() {
    let value = curve([{time:0,value:0},{time:3,value:6}]);
    let values = [];
    for i in 0..143 {
        values.append(Motion(curve([{time:0,value:0},{time:3,value:6}])));
    }
    [value, values]
}
fn direct(value, n: int) {
    let mut sum = 0.0;
    for i in 0..n { sum += value.sample(i % 4); }
    sum
}
fn retained(value, n: int) {
    let sample = value.sample;
    let mut sum = 0.0;
    for i in 0..n { sum += sample(i % 4); }
    sum
}
fn motion(values, n: int) {
    let mut sum = 0.0;
    for i in 0..n { sum += values[i % 143].sample(i % 4); }
    sum
}
"#;

struct Fixture {
    vm: Vm,
    library: aside_std::StandardLibrary,
    values: [Value; 2],
}

struct Row {
    value: f64,
    wall_ns: u128,
    allocations: usize,
    reallocations: usize,
    instructions: u64,
    work: u64,
    native_entries: u64,
    native_hits: u64,
    native_misses: u64,
    script_hits: u64,
    script_misses: u64,
    objects_before: u32,
    objects_after: u32,
}

impl Fixture {
    fn new() -> Self {
        let contracts = aside_std::contracts();
        let module =
            aside_compiler::analyze_module(SOURCE, &Default::default(), &mut support::resolver())
                .unwrap()
                .into_module()
                .unwrap();
        let compiled = aside_host::lower(
            &module,
            &contracts,
            &aside_std::api().functions,
            aside_std::types(),
            &[],
            Default::default(),
        )
        .unwrap();
        let init = compiled.entry;
        let encoded = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
        let program = bc::VerifiedProgram::decode(&encoded, bc::Limits::default()).unwrap();
        let support =
            aside_host::runtime_support(&program, &contracts, aside_std::types(), &[]).unwrap();
        let library = aside_std::StandardLibrary::new(&program);
        let vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
        let mut fixture = Self {
            vm,
            library,
            values: [Value::NULL; 2],
        };
        let init = fixture
            .vm
            .start(init, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let init = fixture.finish(init, false);
        fixture.vm.discard_delivery(init).unwrap();
        let construct = fixture.function("construct");
        let task = fixture
            .vm
            .start(construct, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        // The unreleased construction delivery roots both reusable inputs.
        let delivery = fixture.finish(task, false);
        let array = fixture.vm.delivery_value(delivery).unwrap();
        fixture.values = [
            fixture.vm.heap().get_field(array, 0).unwrap(),
            fixture.vm.heap().get_field(array, 1).unwrap(),
        ];
        fixture
    }

    fn function(&self, name: &str) -> bc::FunctionId {
        let p = self.vm.program().package();
        bc::FunctionId(
            p.functions
                .iter()
                .position(|f| p.symbols[f.name.index()] == name)
                .unwrap() as u32,
        )
    }

    fn finish(&mut self, task: TaskId, sliced: bool) -> DeliveryId {
        let mut budget = SliceBudget::default();
        if sliced {
            budget.instructions = 32;
            budget.work = 512;
        }
        let mut slices = 0;
        loop {
            match self.vm.run_slice(task, budget, || 0).unwrap() {
                RunOutcome::HostCall(ticket) => {
                    self.library
                        .dispatch(&mut self.vm, ticket, 64)
                        .unwrap()
                        .unwrap();
                }
                RunOutcome::BudgetYield(usage) => {
                    budget.work = budget.work.max(usage.required_work);
                    budget.allocation_bytes =
                        budget.allocation_bytes.max(usage.required_allocation);
                    slices += 1;
                    if sliced && slices % 8 == 0 {
                        if self.vm.heap().phase() == GcPhase::Idle {
                            self.vm.begin_collection().unwrap();
                        }
                        self.vm
                            .gc_step(GcBudget {
                                units: 64,
                                ..Default::default()
                            })
                            .unwrap();
                    }
                }
                RunOutcome::Returned(id) => return id,
                other => panic!("curve workload: {other:?}"),
            }
        }
    }

    fn collect(&mut self) {
        while self.vm.heap().phase() != GcPhase::Idle {
            self.vm.gc_step(GcBudget::default()).unwrap();
        }
        self.vm.begin_collection().unwrap();
        while self.vm.heap().phase() != GcPhase::Idle {
            self.vm.gc_step(GcBudget::default()).unwrap();
        }
    }

    fn run(&mut self, name: &str, n: i64, sliced: bool) -> Row {
        let function = self.function(name);
        let value = self.values[usize::from(name == "motion")];
        let before = self.vm.stats();
        let region = Region::new(GLOBAL);
        let start = Instant::now();
        let task = self
            .vm
            .start(
                function,
                &[value, Value::int(n)],
                RootOwner(1),
                bc::Context::Ordinary,
            )
            .unwrap();
        let delivery = self.finish(task, sliced);
        let value = self
            .vm
            .delivery_value(delivery)
            .unwrap()
            .as_float()
            .unwrap();
        self.vm.discard_delivery(delivery).unwrap();
        let wall_ns = start.elapsed().as_nanos();
        let allocated = region.change();
        let after = self.vm.stats();
        Row {
            value,
            wall_ns,
            allocations: allocated.allocations,
            reallocations: allocated.reallocations,
            instructions: after.cumulative_instructions - before.cumulative_instructions,
            work: after.cumulative_work - before.cumulative_work,
            native_entries: after.native_entries - before.native_entries,
            native_hits: after.native_binding_hits - before.native_binding_hits,
            native_misses: after.native_binding_misses - before.native_binding_misses,
            script_hits: after.script_binding_hits - before.script_binding_hits,
            script_misses: after.script_binding_misses - before.script_binding_misses,
            objects_before: before.heap.live_objects,
            objects_after: after.heap.live_objects,
        }
    }
}

fn oracle(n: i64) -> f64 {
    (0..n).map(|i| (i % 4 * 2) as f64).sum()
}

#[test]
fn curve_method_workloads_preserve_values_across_slices_and_gc() {
    let mut fixture = Fixture::new();
    for sliced in [false, true] {
        for name in ["direct", "retained", "motion"] {
            fixture.collect();
            let row = fixture.run(name, 293, sliced);
            assert_eq!(row.value, oracle(293), "{name} sliced={sliced}");
            assert_eq!(row.native_entries, 293);
        }
    }
}

#[test]
#[ignore = "release host-method benchmark; includes actual curve adapter, wall time"]
fn measure_curve_method_workloads() {
    assert!(
        !std::hint::black_box(cfg!(debug_assertions)),
        "use --release"
    );
    let n: i64 = std::env::var("ASIDE_BENCH_N").map_or(2000, |s| s.parse().unwrap());
    let samples: usize = std::env::var("ASIDE_BENCH_SAMPLES").map_or(9, |s| s.parse().unwrap());
    assert!((1..=1_000_000).contains(&n) && (1..=1000).contains(&samples));
    let mut fixture = Fixture::new();
    let mut output = String::from(
        "# construction/compile and pre-sample collection excluded; task/run/native dispatch/delivery included; warmup=3; allocations are process allocator calls, objects are traced VM objects\ncase\tsliced\tsample\tn\twall_ns\tallocations\treallocations\tinstructions\twork\tnative_entries\tnative_hits\tnative_misses\tscript_hits\tscript_misses\tobjects_before\tobjects_after\toracle\n",
    );
    for name in ["direct", "retained", "motion"] {
        for sliced in [false, true] {
            for _ in 0..3 {
                fixture.collect();
                assert_eq!(fixture.run(name, n, sliced).value, oracle(n));
            }
            for sample in 0..samples {
                fixture.collect();
                let r = fixture.run(name, n, sliced);
                assert_eq!(r.value, oracle(n));
                writeln!(output, "{name}\t{sliced}\t{sample}\t{n}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}", r.wall_ns, r.allocations, r.reallocations, r.instructions, r.work, r.native_entries, r.native_hits, r.native_misses, r.script_hits, r.script_misses, r.objects_before, r.objects_after, r.value).unwrap();
            }
        }
    }
    if let Ok(path) = std::env::var("ASIDE_BENCH_OUTPUT") {
        std::fs::write(path, &output).unwrap();
    }
    print!("{output}");
}
