//! Real source graphs, B01 round trips, shared bindings and initialization.
use aside_compiler::{AnalysisOptions, lowering, semantic::*};
use aside_vm::{RootOwner, SessionEpoch, Value, ValueTag, bytecode as bc, executor::*};

fn compile(inputs: &[(&str, &str)]) -> lowering::ProjectCompilation {
    let inputs = inputs
        .iter()
        .map(|&(path, source)| SourceModule {
            canonical_path: path.into(),
            source_path: Some(format!("{path}.aside")),
            version: "1".into(),
            source: source.into(),
        })
        .collect::<Vec<_>>();
    let mut analysis =
        analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let plan = std::mem::take(&mut analysis.plan);
    let modules = analysis.into_modules().unwrap();
    let seed = bc::verify(bc::BytecodePackage::default(), bc::Limits::default()).unwrap();
    let links = vec![Vec::new(); modules.len()];
    lowering::compile_project(&modules, &plan, &seed, &links, lowering::Options::default()).unwrap()
}
fn drive(vm: &mut Vm, function: bc::FunctionId) -> RunOutcome {
    let task = vm
        .start(function, &[], RootOwner(1), bc::Context::Ordinary)
        .unwrap();
    for _ in 0..100_000 {
        let outcome = vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap();
        if !matches!(outcome, RunOutcome::BudgetYield(_)) {
            return outcome;
        }
    }
    panic!("module execution failed to progress");
}
fn value(vm: &mut Vm, function: bc::FunctionId) -> Value {
    let outcome = drive(vm, function);
    let RunOutcome::Returned(delivery) = outcome else {
        panic!("{outcome:?}")
    };
    let value = vm.delivery_value(delivery).unwrap();
    vm.discard_delivery(delivery).unwrap();
    value
}

#[test]
fn repeated_text_is_shared_across_modules_and_defaults_after_roundtrip() {
    let compilation = compile(&[
        ("shared", r#"pub fn word(value = "共同文本") { value }"#),
        (
            "main",
            r#"use shared::word; fn main() { let tuple = ("共同文本", word()); if tuple[0] != tuple[1] { throw "relocation"; } match word() { "共同文本" => true, _ => false } }"#,
        ),
    ]);
    let p = compilation.program.package();
    assert_eq!(
        p.constants
            .iter()
            .filter(|c| matches!(c, bc::Const::Utf8(s) if s == "共同文本"))
            .count(),
        1
    );
    let entry = compilation
        .modules
        .iter()
        .find(|m| m.metadata.canonical_path == "main")
        .unwrap()
        .metadata
        .functions[0]
        .output;
    let encoded = bc::encode(p, bc::Limits::default()).unwrap();
    let program = bc::VerifiedProgram::decode(&encoded, bc::Limits::default()).unwrap();
    let mut vm = Vm::load(
        program,
        bc::RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    value(&mut vm, compilation.initializer);
    assert_eq!(value(&mut vm, entry).as_bool(), Some(true));
}

#[test]
fn imported_classes_keep_nominal_identity_across_aliases_and_inheritance() {
    let compilation = compile(&[
        (
            "types",
            "pub class Word {let score:int;fn init(self,score){self.score=score;}fn value(self){self.score}}",
        ),
        ("other", "pub class Word {let score=0;}"),
        ("facade", "pub use types::{Word as Entry};"),
        (
            "main",
            "use facade::Entry;use other::{Word as Other};use types as t;class PoemWord:Entry{fn value(self){super.value()+2}}fn accepts(word:t::Word)->Entry{word}fn main(){let word:Entry=accepts(PoemWord(40));let mut caught=false;try{accepts(Other());}catch e{caught=true;}if caught{word.value()}else{0}}",
        ),
    ]);
    let p = compilation.program.package();
    let main = bc::FunctionId(
        p.functions
            .iter()
            .position(|f| p.symbols[f.name.index()] == "main")
            .unwrap() as u32,
    );
    let encoded = bc::encode(p, bc::Limits::default()).unwrap();
    let program = bc::VerifiedProgram::decode(&encoded, bc::Limits::default()).unwrap();
    let mut vm = Vm::load(
        program,
        bc::RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    value(&mut vm, compilation.initializer);
    assert_eq!(value(&mut vm, main).as_int(), Some(42));
}

#[test]
fn imported_functions_reexports_and_objects_keep_one_identity_and_private_environment() {
    let compilation = compile(&[
        (
            "main",
            "use facade as api; use facade::{shared,make}; fn main(){ api.bump(); let f=make(4); api.bump(); if shared.n!=2 {throw \"sharing\";} if f()!=6 {throw \"closure\";} shared.n=7; if api.shared.n!=7 {throw \"alias\";} shared.n }",
        ),
        (
            "state",
            "pub let shared={n:0}; pub fn bump(){shared.n+=1;} pub fn make(x){ || {shared.n+=x; shared.n} }",
        ),
        ("facade", "pub use state::{shared,bump,make};"),
    ]);
    let entry = compilation
        .modules
        .iter()
        .find(|m| m.metadata.canonical_path == "main")
        .unwrap()
        .metadata
        .functions[0]
        .output;
    let encoded = bc::encode(compilation.program.package(), bc::Limits::default()).unwrap();
    for _ in 0..2 {
        let program = bc::VerifiedProgram::decode(&encoded, bc::Limits::default()).unwrap();
        let mut vm = Vm::load(
            program,
            bc::RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        assert_eq!(
            value(&mut vm, compilation.initializer).tag(),
            ValueTag::Null
        );
        assert_eq!(value(&mut vm, entry).as_int(), Some(7));
    }
}

#[test]
fn legal_function_cycles_link_before_scc_initializers() {
    let compilation = compile(&[
        (
            "b",
            "use a as a; pub fn right(n){ if n==0 {20} else {a.left(n-1)} }",
        ),
        ("main", "use a as a; fn main(){a.seed+a.left(4)}"),
        (
            "a",
            "use b as b; pub fn left(n){if n==0 {10} else {b.right(n-1)}} pub let seed=b.right(2);",
        ),
    ]);
    let entry = compilation
        .modules
        .iter()
        .find(|m| m.metadata.canonical_path == "main")
        .unwrap()
        .metadata
        .functions[0]
        .output;
    let order = compilation
        .initialization_order
        .iter()
        .map(|id| {
            let package = compilation.program.package();
            package.symbols[package.modules[id.index()].path.index()].as_str()
        })
        .collect::<Vec<_>>();
    assert_eq!(order, ["a", "b", "main"]);
    let mut vm = Vm::load(
        compilation.program,
        bc::RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(2),
    )
    .unwrap();
    value(&mut vm, compilation.initializer);
    assert_eq!(value(&mut vm, entry).as_int(), Some(30));
}

#[test]
fn initialization_cycle_fault_points_to_the_imported_read_in_its_source_file() {
    let compilation = compile(&[
        ("b", "use a::{a}; pub let b=a;"),
        ("a", "use b::{b}; pub let a=b;"),
    ]);
    let mut vm = Vm::load(
        compilation.program,
        bc::RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(3),
    )
    .unwrap();
    let RunOutcome::Fault(delivery) = drive(&mut vm, compilation.initializer) else {
        panic!("cycle did not fail")
    };
    let fault = vm.delivery_fault(delivery).unwrap().unwrap();
    assert_eq!(fault.kind, FaultKind::ModuleInitializationCycle);
    let span = fault.location.source.unwrap();
    let package = vm.program().package();
    let source = &package.sources[span.file.index()];
    assert_eq!(package.symbols[source.path.index()], "a.aside");
    assert_eq!(&source.text[span.start as usize..span.end as usize], "b");
}
