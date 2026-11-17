use aside_compiler::{AnalysisOptions, analyze_module, lowering, semantic::NoModules};
use aside_vm::{
    GcBudget, GcPhase, RootOwner, SessionEpoch, Value, bytecode as bc,
    executor::{HostResponse, RunOutcome, SliceBudget, TaskId, Vm, VmLimits},
};
use std::time::Instant;

const OWNER: RootOwner = RootOwner(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Batch,
    Sliced,
    IncrementalGc,
    Deadline,
}

impl Mode {
    fn budget(self) -> SliceBudget {
        match self {
            Self::Batch => SliceBudget {
                instructions: 100_000,
                work: 1_000_000,
                allocation_bytes: 16 << 20,
                deadline_ns: None,
            },
            Self::Sliced => SliceBudget {
                instructions: 32,
                work: 512,
                allocation_bytes: 64 << 10,
                deadline_ns: None,
            },
            Self::IncrementalGc => SliceBudget {
                instructions: 256,
                work: 4096,
                allocation_bytes: 64 << 10,
                deadline_ns: None,
            },
            Self::Deadline => SliceBudget {
                // Renew the player's 2 ms monotonic deadline before each slice.
                deadline_ns: Some(0),
                ..SliceBudget::default()
            },
        }
    }
}

pub struct Fixture {
    program: bc::VerifiedProgram,
    encoded: Vec<u8>,
    init: bc::FunctionId,
    support: bc::RuntimeSupport,
}

pub struct Row {
    #[cfg(feature = "execution-counts")]
    pub counts: aside_vm::executor::ExecutionCounts,
    pub value: i64,
    pub wall_ns: u128,
    pub gc_ns: u128,
    pub instructions: u64,
    pub work: u64,
    pub slices: u64,
    pub host_entries: u64,
    pub derived_starts: u64,
    pub collections: u64,
    pub gc_units: u64,
    pub charged_before: u64,
    pub charged_after: u64,
    pub peak_charged: u64,
    pub clock_queries: u64,
}

#[derive(Default)]
struct Progress {
    slices: u64,
    gc_ns: u128,
    clock_queries: u64,
}

impl Fixture {
    pub fn compile() -> Self {
        let module = analyze_module(
            include_str!("kernels.aside"),
            &AnalysisOptions::default(),
            &mut NoModules,
        )
        .unwrap()
        .into_module()
        .unwrap();
        let compiled = lowering::compile(
            &module,
            &lowering::Linkage::default(),
            &[],
            lowering::Options::default(),
        )
        .unwrap();
        let mut package = compiled.program.to_unpacked().unwrap();
        let any = bc::TypeId(
            package
                .types
                .iter()
                .position(|ty| *ty == bc::TypeDef::Builtin(bc::BuiltinType::Any))
                .unwrap() as u32,
        );
        let mut support = bc::RuntimeSupport::default();
        // The test owns both these explicit host declarations and their source
        // stubs. Conversion uses the shared verifier and ordinary native binder.
        for (index, (name, parameter)) in [
            ("bench_echo", &["value"][..]),
            ("bench_model", &["value"][..]),
            ("bench_derive", &["callback"][..]),
            ("bench_flush", &[][..]),
            ("bench_host", &["value"][..]),
            ("bench_method", &["receiver", "value"][..]),
        ]
        .into_iter()
        .enumerate()
        {
            let function = package
                .functions
                .iter()
                .position(|f| package.symbols[f.name.index()] == name)
                .unwrap();
            let native_symbol = bc::SymbolId(package.symbols.len() as u32);
            let symbol = format!("benchmark::{name}");
            package.symbols.push(symbol.clone());
            let signature = bc::Signature {
                parameters: parameter
                    .iter()
                    .map(|name| bc::Parameter {
                        name: bc::SymbolId(
                            package
                                .symbols
                                .iter()
                                .position(|symbol| symbol == *name)
                                .unwrap() as u32,
                        ),
                        ty: any,
                        mutable: false,
                        default: None,
                    })
                    .collect(),
                rest: None,
                return_type: any,
                effects: bc::Effects::ALL,
                property_bag_tail: None,
            };
            let signature_id = bc::SignatureId(package.signatures.len() as u32);
            package.signatures.push(signature.clone());
            package.natives.push(bc::NativeDef {
                symbol: native_symbol,
                signature: signature_id,
                abi_version: 1,
                capabilities: vec![],
            });
            let f = &mut package.functions[function];
            f.name = native_symbol;
            f.kind = bc::FunctionKind::Native(bc::NativeId(index as u32));
            f.signature = signature_id;
            f.register_count = 0;
            f.bindings.clear();
            f.descriptors.clear();
            f.code.clear();
            f.regions.clear();
            f.source_map.clear();
            support.natives.push(bc::NativeSupport {
                standard_derived_commit: false,
                read_only_leaf: None,
                symbol,
                abi_version: 1,
                signature,
                required_capabilities: vec![],
            });
        }
        support.host_types.push(bc::HostTypeSupport {
            await_adapter: None,
            call: None,
            name: "benchmark::Host".into(),
            version: 1,
            bases: Vec::new(),
            methods: vec![bc::HostMethodSupport {
                name: "sample".into(),
                native: "benchmark::bench_method".into(),
                abi_version: 1,
            }],
        });
        let program = bc::verify(package, bc::Limits::default()).unwrap();
        let encoded = bc::encode(program.package(), bc::Limits::default()).unwrap();
        Self {
            program,
            encoded,
            init: compiled.entry,
            support,
        }
    }

    pub fn load(&self, encoded: bool) -> (Vm, [u128; 3]) {
        let start = Instant::now();
        let program = if encoded {
            bc::VerifiedProgram::decode(&self.encoded, bc::Limits::default()).unwrap()
        } else {
            self.program.clone()
        };
        let decode = start.elapsed().as_nanos();
        let start = Instant::now();
        let mut vm = Vm::load(
            program,
            self.support.clone(),
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        let load = start.elapsed().as_nanos();
        let start = Instant::now();
        let task = vm.start(self.init, &[], OWNER, bc::Context::Init).unwrap();
        Self::finish(&mut vm, task, Mode::Batch, &mut Progress::default());
        let init = start.elapsed().as_nanos();
        (vm, [decode, load, init])
    }

    pub fn run(&self, vm: &mut Vm, name: &str, n: i64, mode: Mode) -> Row {
        let package = vm.program().package();
        let function = bc::FunctionId(
            package
                .functions
                .iter()
                .position(|f| package.symbols[f.name.index()] == name)
                .unwrap() as u32,
        );
        let before = vm.stats();
        #[cfg(feature = "execution-counts")]
        let counts_before = vm.execution_counts();
        let mut progress = Progress::default();
        let start = Instant::now();
        let task = vm
            .start(function, &[Value::int(n)], OWNER, bc::Context::Ordinary)
            .unwrap();
        let value = Self::finish(vm, task, mode, &mut progress)
            .as_int()
            .unwrap();
        let wall_ns = start.elapsed().as_nanos();
        let after = vm.stats();
        Row {
            #[cfg(feature = "execution-counts")]
            counts: vm.execution_counts().checked_delta(&counts_before).unwrap(),
            value,
            wall_ns,
            gc_ns: progress.gc_ns,
            instructions: after.cumulative_instructions - before.cumulative_instructions,
            work: after.cumulative_work - before.cumulative_work,
            slices: after.slices - before.slices,
            host_entries: after.native_entries - before.native_entries,
            derived_starts: after.derived_starts - before.derived_starts,
            collections: after.heap.collections - before.heap.collections,
            gc_units: after.heap.gc_units - before.heap.gc_units,
            charged_before: before.heap.charged_bytes,
            charged_after: after.heap.charged_bytes,
            peak_charged: after.heap.peak_charged_bytes,
            clock_queries: progress.clock_queries,
        }
    }

    fn finish(vm: &mut Vm, task: TaskId, mode: Mode, progress: &mut Progress) -> Value {
        let mut budget = mode.budget();
        let clock = (mode == Mode::Deadline).then(Instant::now);
        loop {
            let outcome = if mode == Mode::Deadline {
                let elapsed = || u64::try_from(clock.unwrap().elapsed().as_nanos()).unwrap();
                budget.deadline_ns = Some(elapsed().saturating_add(2_000_000));
                vm.run_slice(task, budget, || {
                    progress.clock_queries += 1;
                    elapsed()
                })
                .unwrap()
            } else {
                vm.run_slice(task, budget, || 0).unwrap()
            };
            progress.slices += 1;
            if mode == Mode::IncrementalGc
                && (progress.slices == 1 || progress.slices.is_multiple_of(8))
            {
                let start = Instant::now();
                if vm.heap().phase() == GcPhase::Idle {
                    vm.begin_collection().unwrap();
                }
                vm.gc_step(GcBudget {
                    units: 256,
                    ..Default::default()
                })
                .unwrap();
                progress.gc_ns += start.elapsed().as_nanos();
            }
            match outcome {
                RunOutcome::BudgetYield(usage) => {
                    budget.work = mode.budget().work.max(usage.required_work);
                    budget.allocation_bytes = mode
                        .budget()
                        .allocation_bytes
                        .max(usage.required_allocation);
                }
                RunOutcome::HostCall(ticket) => match ticket.native.0 {
                    0 => {
                        let value = vm.host_arguments(ticket).unwrap()[0];
                        vm.respond(ticket, HostResponse::Ready(value)).unwrap();
                    }
                    1 => {
                        vm.respond_model(ticket, 64).unwrap();
                    }
                    2 => {
                        vm.respond_derive(ticket).unwrap();
                    }
                    3 => {
                        while let Some(output) = vm.heap_mut().next_derived().unwrap() {
                            let observer = vm.start_derived(output, OWNER).unwrap();
                            Self::finish(vm, observer, mode, progress);
                        }
                        vm.respond(ticket, HostResponse::Ready(Value::NULL))
                            .unwrap();
                    }
                    4 => {
                        let fields = vm.host_arguments(ticket).unwrap()[0];
                        let count = vm.heap().table_len(fields).unwrap();
                        let host = vm.heap_mut().alloc_host(fields, &[], None).unwrap();
                        let ty = vm.heap().host_type("benchmark::Host").unwrap();
                        vm.heap_mut().set_host_type(host, ty).unwrap();
                        vm.heap_mut()
                            .observe_host_properties(host, 0, count)
                            .unwrap();
                        vm.respond(ticket, HostResponse::Ready(host)).unwrap();
                    }
                    5 => {
                        let value = vm.host_arguments(ticket).unwrap()[1];
                        vm.respond(ticket, HostResponse::Ready(value)).unwrap();
                    }
                    _ => unreachable!(),
                },
                RunOutcome::Returned(delivery) => {
                    let value = vm.delivery_value(delivery).unwrap();
                    vm.discard_delivery(delivery).unwrap();
                    return value;
                }
                RunOutcome::Fault(delivery) => {
                    panic!("VM kernel fault: {:?}", vm.delivery_fault(delivery))
                }
                other => panic!("unexpected VM kernel result: {other:?}"),
            }
        }
    }

    pub fn collect(&self, vm: &mut Vm) {
        if vm.heap().phase() != GcPhase::Idle {
            while vm.heap().phase() != GcPhase::Idle {
                vm.gc_step(GcBudget::default()).unwrap();
            }
        }
        vm.begin_collection().unwrap();
        while vm.heap().phase() != GcPhase::Idle {
            vm.gc_step(GcBudget::default()).unwrap();
        }
    }
}
