//! Run alone in a fresh process so each source registry starts uninitialized.
use aside_compiler::{
    AnalysisOptions,
    semantic::{ExternalSymbol, NoModules, SymbolReference, TypeHint},
};
use aside_vm::{ObjectKind, bytecode as bc};
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{RunOutcome, SessionBudget, SessionStep, SessionVm, SliceBudget, VmLimits, source},
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, Stats, StatsAlloc};
use std::{alloc::System, collections::BTreeSet, hint::black_box, time::Instant};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn report(name: &str, stats: Stats) {
    eprintln!(
        "{name}: allocations={} reallocations={} deallocations={} allocated_bytes={} freed_bytes={} resident_bytes={}",
        stats.allocations,
        stats.reallocations,
        stats.deallocations,
        stats.bytes_allocated,
        stats.bytes_deallocated,
        stats.bytes_allocated as i64 - stats.bytes_deallocated as i64
    );
}

fn run_typed_asb(contracts: &[aside_host::NativeContract], types: &[bc::HostTypeSupport]) {
    let mut node = ExternalSymbol::value("Node", TypeHint::Named("reel::types::Node".into()))
        .with_reference(SymbolReference {
            module: "reel::types".into(),
            version: "1".into(),
            export: Some("Node".into()),
        });
    node.type_only = true;
    let text = contracts
        .iter()
        .find(|contract| contract.symbol == "reel::text")
        .unwrap();
    let analysis = aside_compiler::analyze_module(
        "fn opening() -> Node { text(\"typed ASB\") }",
        &AnalysisOptions {
            hosts: vec![node, text.external()],
            ..Default::default()
        },
        &mut NoModules,
    )
    .unwrap();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let module = analysis.into_module().unwrap();
    let capabilities = contracts
        .iter()
        .flat_map(|contract| contract.required_capabilities.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let compiled = aside_host::lower(
        &module,
        contracts,
        &[],
        types,
        &capabilities,
        Default::default(),
    )
    .unwrap();
    let initializer = compiled.entry;
    let entry = bc::FunctionId(
        compiled
            .program
            .package()
            .functions
            .iter()
            .position(|function| {
                compiled.program.package().symbols[function.name.index()] == "opening"
            })
            .unwrap() as u32,
    );
    let encoded = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
    let program = bc::VerifiedProgram::decode(&encoded, bc::Limits::default()).unwrap();
    let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
    let mut vm = SessionVm::load(&session, program, VmLimits::default()).unwrap();
    for function in [initializer, entry] {
        vm.start(&session, function).unwrap();
        let mut returned = false;
        for _ in 0..10_000 {
            match vm
                .step(
                    &mut session,
                    SessionBudget::RetryMinimum(SliceBudget::default()),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    vm.dispatch(
                        &mut session,
                        ticket,
                        "en",
                        reel_engine::aside::NativeBudget::Exact(1024),
                    )
                    .unwrap();
                }
                SessionStep::Vm(RunOutcome::Returned(delivery)) => {
                    if function == entry {
                        let node = vm.vm().delivery_value(delivery).unwrap();
                        assert_eq!(vm.vm().heap().info(node).unwrap().kind, ObjectKind::Host);
                    }
                    vm.discard_delivery(&mut session).unwrap();
                    returned = true;
                    break;
                }
                SessionStep::Vm(RunOutcome::Fault(delivery)) => {
                    panic!("{:?}", vm.vm().delivery_fault(delivery).unwrap())
                }
                SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => panic!("{error:?}"),
                _ => {}
            }
        }
        assert!(returned, "typed ASB entry must finish");
    }
}

#[test]
fn source_registry_first_build_and_resident_metadata() {
    let region = Region::new(GLOBAL);
    let contracts = black_box(source::contracts());
    let abi = region.change();
    let region = Region::new(GLOBAL);
    let types = black_box(source::types());
    let registrations = region.change();
    run_typed_asb(contracts, types);
    let start = Instant::now();
    let region = Region::new(GLOBAL);
    let api = black_box(source::api());
    let source_metadata = region.change();
    let elapsed = start.elapsed();
    assert!(
        source_metadata.allocations > 0,
        "loading and executing typed ASB must leave the source API cache uninitialized"
    );
    assert!(
        registrations.bytes_allocated - registrations.bytes_deallocated
            < source_metadata.bytes_allocated - source_metadata.bytes_deallocated
    );
    let region = Region::new(GLOBAL);
    for _ in 0..10_000 {
        black_box(source::contracts());
        black_box(source::api());
        black_box(source::types());
    }
    let cached = region.change();
    assert_eq!(cached.allocations, 0);
    assert_eq!(cached.reallocations, 0);
    report("native_ABI", abi);
    report("source_API_first_build_after_typed_ASB", source_metadata);
    report("cold_runtime_type_projection", registrations);
    report("30000_cached_reads", cached);
    let documents = api
        .functions
        .iter()
        .map(|declaration| &declaration.documentation)
        .chain(api.types.iter().map(|ty| &ty.documentation))
        .chain(api.types.iter().flat_map(|ty| {
            ty.members
                .iter()
                .map(|member| &member.declaration.documentation)
        }));
    let mut unique = BTreeSet::new();
    let mut document_count = 0;
    let mut document_bytes = 0;
    let mut document_capacity = 0;
    for document in documents {
        document_count += usize::from(!document.is_empty());
        document_bytes += document.len();
        document_capacity += document.capacity();
        unique.insert(document.as_str());
    }
    eprintln!(
        "schema: natives={} types={} fields={} globals={} modules={} API_build_debug_us={}",
        contracts.len(),
        types.len(),
        api.types.iter().map(|ty| ty.members.len()).sum::<usize>(),
        api.globals.len(),
        api.modules.len(),
        elapsed.as_micros()
    );
    eprintln!(
        "documentation: nonempty_copies={document_count} payload_bytes={document_bytes} capacity_bytes={document_capacity} unique_payload_bytes={}",
        unique.iter().map(|document| document.len()).sum::<usize>()
    );
}
