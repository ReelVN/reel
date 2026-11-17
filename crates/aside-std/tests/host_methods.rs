//! Exercise curve host methods across VM slices and garbage collections.
mod support;

use aside_host::{aside_compiler, aside_vm};
use aside_vm::{GcBudget, GcPhase, RootOwner, SessionEpoch, Value, bytecode as bc, executor::*};

const SOURCE: &str = r#"
use std::math::curve;
class Motion {
    let mut values: any = null;
    fn init(self, values) { self.values = values; }
    fn sample(self, seconds) { self.values.sample(seconds) }
}
class AuthoredMotion {
    let values;
    let direct: bool = true;
    let end_time: float = 3.0;
    let loop_start: float;
    fn init(self, values, loop_start: float) {
        self.values = values;
        self.loop_start = loop_start;
    }
    fn sample(self, seconds: float) -> float {
        if self.direct {
            let time = if self.loop_start >= 0.0 && seconds >= self.end_time
                && self.end_time > self.loop_start {
                self.loop_start + (seconds - self.end_time) % (self.end_time - self.loop_start)
            } else { seconds };
            return self.values.sample(time);
        }
        self.values.sample(seconds)
    }
}
fn construct() {
    let value = curve([{time:0,value:0},{time:3,value:6}]);
    let values = [];
    let authored = [];
    for i in 0..143 {
        values.append(Motion(curve([{time:0,value:0},{time:3,value:6}])));
        authored.append(AuthoredMotion(
            curve([{time:0,value:0},{time:3,value:6}]),
            if i % 2 == 0 { -1.0 } else { 1.0 }));
    }
    [value, values, authored]
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
fn authored_motion(values, n: int) {
    let mut sum = 0.0;
    for i in 0..n { sum += values[i % 143].sample((i % 4) * 1.0); }
    sum
}
"#;

struct Fixture {
    vm: Vm,
    library: aside_std::StandardLibrary,
    values: [Value; 3],
}

impl Fixture {
    fn new(leaves: bool) -> Self {
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
        let mut support =
            aside_host::runtime_support(&program, &contracts, aside_std::types(), &[]).unwrap();
        if !leaves {
            for native in &mut support.natives {
                native.read_only_leaf = None;
            }
        }
        let library = aside_std::StandardLibrary::new(&program);
        let vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
        let mut fixture = Self {
            vm,
            library,
            values: [Value::NULL; 3],
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
        // The unreleased construction delivery roots all reusable inputs.
        let delivery = fixture.finish(task, false);
        let array = fixture.vm.delivery_value(delivery).unwrap();
        fixture.values = [
            fixture.vm.heap().get_field(array, 0).unwrap(),
            fixture.vm.heap().get_field(array, 1).unwrap(),
            fixture.vm.heap().get_field(array, 2).unwrap(),
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

    fn run(&mut self, name: &str, n: i64, sliced: bool) -> (f64, u64) {
        let function = self.function(name);
        let value = self.values[match name {
            "motion" => 1,
            "authored_motion" => 2,
            _ => 0,
        }];
        let native_entries = self.vm.stats().native_entries;
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
        (value, self.vm.stats().native_entries - native_entries)
    }
}

fn oracle(name: &str, n: i64) -> f64 {
    (0..n)
        .map(|i| {
            let time = if name == "authored_motion" && i % 143 % 2 == 1 && i % 4 == 3 {
                1
            } else {
                i % 4
            };
            (time * 2) as f64
        })
        .sum()
}

#[test]
fn curve_method_workloads_preserve_values_across_slices_and_gc() {
    for leaves in [false, true] {
        let mut fixture = Fixture::new(leaves);
        for sliced in [false, true] {
            for name in ["direct", "retained", "motion", "authored_motion"] {
                fixture.collect();
                let (value, native_entries) = fixture.run(name, 293, sliced);
                assert_eq!(
                    value,
                    oracle(name, 293),
                    "{name} sliced={sliced} leaves={leaves}"
                );
                assert_eq!(native_entries, if leaves { 0 } else { 293 });
            }
        }
    }
}
