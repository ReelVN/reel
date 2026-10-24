//! Retained Quirrel assertions and output oracles executed by Aside's real VM.
use aside_compiler::{hir, lowering, semantic::*};
use aside_vm::{bytecode as bc, executor::*, *};
use std::path::Path;

const HOSTS: [&str; 3] = ["assert", "print", "println"];

fn registry() -> (bc::VerifiedProgram, AnalysisOptions, bc::RuntimeSupport) {
    let signature = bc::Signature {
        parameters: vec![],
        rest: Some(bc::Parameter {
            name: bc::SymbolId(3),
            ty: bc::TypeId(0),
            mutable: false,
            default: None,
        }),
        return_type: bc::TypeId(1),
        effects: bc::Effects(bc::Effects::HOST),
        property_bag_tail: None,
    };
    let package: bc::BytecodePackage = bc::BytecodePackage {
        symbols: HOSTS
            .iter()
            .map(|name| format!("test::{name}"))
            .chain(["values".into()])
            .collect(),
        types: vec![
            bc::TypeDef::Builtin(bc::BuiltinType::Any),
            bc::TypeDef::Builtin(bc::BuiltinType::Null),
        ],
        signatures: vec![signature.clone()],
        natives: (0..3)
            .map(|i| bc::NativeDef {
                symbol: bc::SymbolId(i),
                signature: bc::SignatureId(0),
                abi_version: 1,
                capabilities: vec![],
            })
            .collect(),
        ..Default::default()
    };
    let options = AnalysisOptions {
        hosts: HOSTS
            .iter()
            .map(|name| {
                ExternalSymbol::function(
                    *name,
                    ExternalSignature {
                        result_parameter: None,
                        result_cases: Vec::new(),
                        parameters: vec![ExternalParameter {
                            name: "values".into(),
                            ty: TypeHint::Any,
                            mutable: false,
                            default: false,
                            rest: true,
                        }],
                        result: TypeHint::Null,
                        effect: CallEffect::NoWait,
                        init_safe: Some(true),
                        derived_safe: Some(false),
                    },
                )
                .with_reference(SymbolReference {
                    module: "test".into(),
                    version: "1".into(),
                    export: Some((*name).into()),
                })
            })
            .collect(),
        ..Default::default()
    };
    let support = bc::RuntimeSupport {
        host_types: Vec::new(),
        opcodes: vec![],
        capabilities: vec![],
        natives: HOSTS
            .iter()
            .map(|name| bc::NativeSupport {
                symbol: format!("test::{name}"),
                abi_version: 1,
                signature: signature.clone(),
                required_capabilities: vec![],
            })
            .collect(),
    };
    (
        bc::verify(package, bc::Limits::default()).unwrap(),
        options,
        support,
    )
}

fn text(heap: &Heap, value: Value) -> String {
    if value.same_bits(Value::NULL) {
        return "null".into();
    }
    if let Some(value) = value.as_bool() {
        return value.to_string();
    }
    if let Some(value) = value.as_int() {
        return value.to_string();
    }
    if let Some(value) = value.as_float() {
        return value.to_string();
    }
    assert_eq!(heap.info(value).unwrap().kind, ObjectKind::String);
    let mut bytes = vec![0; heap.info(value).unwrap().len as usize];
    heap.read_bytes(value, 0, &mut bytes).unwrap();
    String::from_utf8(bytes).unwrap()
}

fn execute(source: &str, tiny: bool) -> Result<(String, usize), String> {
    let (seed, options, support) = registry();
    let analysis =
        analyze_module(source, &options, &mut NoModules).map_err(|e| format!("parse: {e:?}"))?;
    if !analysis.is_ok() {
        return Err(format!("analysis: {:?}", analysis.diagnostics));
    }
    let module = analysis.into_module().unwrap();
    let links: Vec<_> = module
        .external_bindings()
        .iter()
        .filter(|external| external.kind == hir::ExternalBindingKind::Reference)
        .filter_map(|external| {
            let symbol = external.symbol?;
            let reference = external.descriptor.reference.as_deref()?;
            let native = HOSTS
                .iter()
                .position(|name| Some(*name) == reference.export.as_deref())?;
            Some(lowering::NativeLink {
                source_signature: None,
                target: lowering::ExternalTarget::Symbol(hir::SymbolId(symbol.0)),
                native: bc::NativeId(native as u32),
                reference,
            })
        })
        .collect();
    let compiled = lowering::compile(
        &module,
        &lowering::Linkage {
            package: Some(&seed),
            natives: &links,
            expansions: &[],
        },
        lowering::Options::default(),
    )
    .map_err(|e| format!("lower: {e:?}"))?;
    let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
    let mut vm = Vm::load(
        bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap(),
        support,
        VmLimits::default(),
        SessionEpoch(1),
    )
    .map_err(|e| format!("load: {e:?}"))?;
    let task = vm
        .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
        .unwrap();
    let mut budget = SliceBudget::default();
    if tiny {
        budget.instructions = 1;
        budget.work = 1;
        budget.allocation_bytes = 0;
    }
    let mut output = String::new();
    let mut assertions = 0;
    loop {
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
                budget.work = usage.required_work.max(1);
                budget.allocation_bytes = usage.required_allocation;
            }
            RunOutcome::HostCall(ticket) => {
                let args = vm.host_arguments(ticket).unwrap()[0];
                let len = vm.heap().sequence_len(args).unwrap();
                if ticket.native == bc::NativeId(0) {
                    assert!(len == 1 || len == 2);
                    let condition = vm.heap().sequence_get(args, 0).unwrap();
                    if condition.as_bool() != Some(true) {
                        return Err(format!("original assertion {} failed", assertions + 1));
                    }
                    assertions += 1;
                } else {
                    for i in 0..len {
                        if i > 0 && ticket.native == bc::NativeId(2) {
                            output.push(' ');
                        }
                        output.push_str(&text(vm.heap(), vm.heap().sequence_get(args, i).unwrap()));
                    }
                    if ticket.native == bc::NativeId(2) {
                        output.push('\n');
                    }
                }
                vm.respond(ticket, HostResponse::Ready(Value::NULL))
                    .unwrap();
            }
            RunOutcome::Returned(delivery) => {
                vm.discard_delivery(delivery).unwrap();
                assert!(vm.is_idle());
                return Ok((output, assertions));
            }
            RunOutcome::Fault(delivery) => {
                return Err(format!("VM fault: {:?}", vm.delivery_fault(delivery)));
            }
            other => return Err(format!("unexpected outcome: {other:?}")),
        }
    }
}

#[test]
fn native_values_keep_identity_rest_and_optional_call_semantics() {
    let source = r#"
        fn forward(callback) { callback(...[40,2]); }
        let output=print;
        let check=assert;
        let line=println;
        check(output==print);
        forward(output);
        line?("done");
        fn identity(value) { value }
        identity(check)(line==println);
        let absent=null;
        absent?(output("unreachable"));
    "#;
    for tiny in [false, true] {
        assert_eq!(execute(source, tiny).unwrap(), ("402done\n".into(), 2));
    }
}

#[test]
fn original_assertions_and_output_survive_bytecode_slices_and_gc() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/quirrel");
    let provenance: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("provenance.json")).unwrap()).unwrap();
    let mut failures = vec![];
    for entry in provenance["files"].as_array().unwrap() {
        let name = entry["path"].as_str().unwrap();
        let source = std::fs::read_to_string(root.join(name)).unwrap();
        let golden = std::fs::read_to_string(
            root.join(entry["source"].as_str().unwrap())
                .with_extension("out"),
        )
        .unwrap();
        for tiny in [false, true] {
            match execute(&source, tiny) {
                Ok((output, assertions)) => {
                    assert_eq!(
                        output.lines().collect::<Vec<_>>(),
                        golden.lines().collect::<Vec<_>>(),
                        "{name}, tiny={tiny}"
                    );
                    println!("{name}: execution=passed assertions={assertions} tiny={tiny}");
                }
                Err(error) => failures.push(format!("{name}, tiny={tiny}: {error}")),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
