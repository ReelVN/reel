//! State/Root directives use current static class globals and the ordinary VM binder.
use aside_compiler::{AnalysisOptions, analyze_module, lowering, semantic::*};
use aside_vm::{
    RootKind, RootOwner, SessionEpoch, Value, bytecode as bc, executor::*, resume::*,
    typed_data::RegistryInitCursor,
};

fn resolver() -> MemoryResolver {
    let mut resolver = MemoryResolver::default();
    let exports = [
        ("state", Intrinsic::ResumeState, Some("fields")),
        ("root", Intrinsic::ResumeRoot, None),
    ]
    .into_iter()
    .map(|(name, intrinsic, third)| {
        let mut symbol = ExternalSymbol::intrinsic(name, intrinsic);
        symbol.ty = TypeHint::Closure;
        let mut parameters = vec![
            ExternalParameter {
                name: "target".into(),
                ty: TypeHint::Any,
                mutable: false,
                default: false,
                rest: false,
            },
            ExternalParameter {
                name: "key".into(),
                ty: TypeHint::String,
                mutable: false,
                default: false,
                rest: false,
            },
        ];
        if let Some(third) = third {
            parameters.push(ExternalParameter {
                name: third.into(),
                ty: TypeHint::Table,
                mutable: false,
                default: true,
                rest: false,
            });
        }
        symbol.signature = Some(ExternalSignature {
            parameters,
            result: TypeHint::Null,
            result_cases: vec![],
            result_parameter: None,
            effect: CallEffect::NoWait,
            init_safe: Some(true),
            derived_safe: Some(false),
        });
        symbol
    })
    .collect();
    resolver.insert(ModuleInterface {
        canonical_path: "std::resume".into(),
        version: "1".into(),
        exports,
    });
    resolver
}
fn compile_modules(
    inputs: &[SourceModule],
    version: u64,
) -> Result<lowering::ProjectCompilation, String> {
    let mut analysis = analyze_project(inputs, &AnalysisOptions::default(), &mut resolver())
        .map_err(|e| e.to_string())?;
    if !analysis.is_ok() {
        return Err(format!("{:?}", analysis.diagnostics));
    }
    let plan = std::mem::take(&mut analysis.plan);
    let modules = analysis.into_modules().unwrap();
    let links = (0..modules.len()).map(|_| Vec::new()).collect::<Vec<_>>();
    let seed: bc::BytecodePackage = bc::BytecodePackage {
        content_version: version,
        ..Default::default()
    };
    let seed = bc::verify(seed, bc::Limits::default()).unwrap();
    lowering::compile_project(
        modules,
        &plan,
        &seed,
        &[],
        &links,
        &[],
        lowering::Options {
            content_version: version,
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())
}
fn compile(source: &str) -> Result<lowering::ProjectCompilation, String> {
    compile_modules(
        &[SourceModule {
            canonical_path: "game".into(),
            source_path: Some("game.aside".into()),
            version: "1".into(),
            source: source.into(),
        }],
        1,
    )
}
fn run(vm: &mut Vm, function: bc::FunctionId, args: &[Value]) -> Value {
    let task = vm
        .start(function, args, RootOwner(7), bc::Context::Ordinary)
        .unwrap();
    loop {
        match vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 1,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap()
        {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::Returned(delivery) => {
                let value = vm.delivery_value(delivery).unwrap();
                let root = vm
                    .heap_mut()
                    .add_root(RootOwner(7), RootKind::Host, value)
                    .unwrap();
                vm.discard_delivery(delivery).unwrap();
                return vm.heap().root_value(root).unwrap();
            }
            outcome => panic!("State bootstrap: {outcome:?}"),
        }
    }
}
const SOURCE: &str = r#"
    use std::resume::{state,root};let mut constructors=0;
    class Profile{let mut score:int=7;fn init(self){constructors+=1;}}
    state(Profile,"profile",fields:{score:"points"});
    root(main,"game");
    fn main(value:Profile=Profile()){value}
    fn init_count(){constructors}
"#;

#[test]
fn single_module_root_uses_the_proven_global_initializer_function() {
    let module = analyze_module(SOURCE, &AnalysisOptions::default(), &mut resolver())
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
    let root = compiled.resume.root.as_ref().unwrap();
    let package = compiled.program.package();
    assert_eq!(
        package.symbols[package.functions[root.function.index()].name.index()],
        "main"
    );
    assert!(
        package
            .modules
            .iter()
            .flat_map(|m| &m.globals)
            .all(|g| g.linked_function.is_none())
    );
    let checked = compiled
        .program
        .verify_resume(compiled.resume, &[], ResumeLimits::default())
        .unwrap();
    assert_eq!(checked.table().states.len(), 1);
}

#[test]
fn value_contracts_keep_state_field_identity_across_record_reordering() {
    use aside_vm::typed_data::DataType;
    let mut schemas = Vec::new();
    for annotation in [
        r#"{label: "back" | "title", count: int}"#,
        r#"{count: int, label: "title" | "back"}"#,
    ] {
        let compiled = compile(&format!(
            r#"
            use std::resume::state;
            class Menu {{ let mut view: {annotation}; }}
            state(Menu, "menu");
        "#
        ))
        .unwrap();
        let checked = compiled
            .program
            .verify_resume(compiled.resume, &[], ResumeLimits::default())
            .unwrap();
        let state = checked
            .normalized_state_contracts(&[], ResumeLimits::default())
            .unwrap();
        let schema = state.states.into_iter().next().unwrap().schema;
        assert!(matches!(&schema.fields[0].ty, DataType::Record(fields)
        if matches!(&fields[1].1, DataType::Union(values) if values == &[
            DataType::StringLiteral("back".into()), DataType::StringLiteral("title".into())
        ])));
        schemas.push(schema);
    }
    assert_eq!(schemas[0], schemas[1]);
}

#[test]
fn static_declarations_roundtrip_metadata_and_register_current_classes_without_native_stubs() {
    let compiled = compile(SOURCE).unwrap();
    assert!(compiled.program.package().natives.is_empty());
    assert!(!compiled.resume.is_empty());
    assert!(compiled.resume.flows.is_empty());
    assert!(
        !compiled
            .program
            .package()
            .constants
            .iter()
            .any(|c| matches!(c,bc::Const::Utf8(s)if s=="profile"||s=="points"))
    );
    let metadata = serde_json::to_vec(&compiled.resume).unwrap();
    let table: ResumeTable = serde_json::from_slice(&metadata).unwrap();
    assert_eq!(table.version, 1);
    let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
    let program = bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap();
    let checked = program
        .verify_resume(table, &[], ResumeLimits::default())
        .unwrap();
    let contract = checked
        .normalized_state_contracts(&[], ResumeLimits::default())
        .unwrap();
    assert_eq!(contract.states[0].schema.fields[0].key.name, "points");
    let mut vm = Vm::load(
        program,
        bc::RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    run(&mut vm, compiled.initializer, &[]);
    let mut cursor = RegistryInitCursor::new(&mut vm, &checked, RootOwner(7)).unwrap();
    let mut units = 0;
    while !cursor.step(&mut vm, 1).unwrap() {
        units += 1;
    }
    assert!(units > 3);
    let registry = cursor.finish(vm.heap_mut()).unwrap();
    let main = checked.table().root.as_ref().unwrap().function;
    let first = run(&mut vm, main, &[]);
    let second = run(&mut vm, main, &[first]);
    assert!(first.same_bits(second));
    let signature = &compiled.program.package().signatures[compiled.program.package().functions
        [main.index()]
    .signature
    .index()];
    assert!(matches!(
        signature.parameters[0].default,
        Some(bc::ParameterDefault::Function(_))
    ));
    assert_eq!(registry.class_for_instance(vm.heap(), first).unwrap(), 0);
    let count = compiled
        .program
        .package()
        .functions
        .iter()
        .position(|f| compiled.program.package().symbols[f.name.index()] == "init_count")
        .unwrap();
    assert_eq!(
        run(&mut vm, bc::FunctionId(count as u32), &[]).as_int(),
        Some(1)
    );
    registry.release(vm.heap_mut()).unwrap();
    assert!(serde_json::from_str::<ResumeTable>(r#"{"version":1,"flows":[]}"#).is_err());
}

#[test]
fn declaration_contracts_reject_forged_locators_and_source_errors() {
    let compiled = compile(SOURCE).unwrap();
    let mut table = compiled.resume.clone();
    table.states[0].class.nominal = bc::TypeId(u32::MAX);
    assert!(
        compiled
            .program
            .verify_resume(table, &[], ResumeLimits::default())
            .unwrap_err()
            .to_string()
            .contains("locator differs")
    );
    for (source, expected) in [
        (
            "class S{let x:int=1;}state(S,\"s\");state(S,\"s2\");",
            "multiple State",
        ),
        (
            "class S{let x:int=1;let y:int=2;}state(S,\"s\",fields:{x:\"same\",y:\"same\"});",
            "repeated key",
        ),
        (
            "fn make(){1}class S{let x:int=make();}state(S,\"s\");",
            "default requires",
        ),
        (
            "class Base{let x:int=1;}class S:Base{}state(S,\"s\");",
            "base class",
        ),
        (
            "class Other{}class S{let child:Other|null=null;}state(S,\"s\");",
            "Other has no state declaration",
        ),
        (
            "class S{let x:int=1;}state(S,\"s\");fn boot(saved:S){saved}root(boot,\"game\");",
            "default value",
        ),
        (
            "class S{}class Other{}state(Other,\"game.state\");fn boot(value:S=S()){value}root(boot,\"game\");",
            "inferred Root State key \"game.state\" is already in use",
        ),
        (
            "class S{}state(S,\"s\");fn boot(value:S|null=null){value}root(boot,\"game\");",
            "direct State type",
        ),
        (
            "class S{}state(S,\"s\");fn boot(value:any=S()){value}root(boot,\"game\");",
            "direct State type",
        ),
        (
            "class S{}state(S,\"s\");fn boot(){S()}root(boot,\"game\");",
            "one State parameter",
        ),
        (
            "class S{let x:int=1;}fn bad(){state(S,\"s\");}",
            "module statements",
        ),
    ] {
        let error = compile(&format!("use std::resume::{{state,root}};{source}"))
            .expect_err("invalid contract");
        assert!(error.contains(expected), "expected {expected}: {error}");
    }
    assert!(compile("fn ordinary(){1}").unwrap().resume.is_empty());
}

#[test]
fn root_infers_an_imported_class_with_the_same_schema_as_explicit_state() {
    let mut inputs = [
        SourceModule {
            canonical_path: "data".into(),
            source_path: None,
            version: "1".into(),
            source: "pub class Settings { let volume: int = 5; }".into(),
        },
        SourceModule {
            canonical_path: "game".into(),
            source_path: None,
            version: "1".into(),
            source: r#"use data::Settings as State;
                use std::resume::{state, root};
                fn start(value: State = State()) { value }
                root(start, "game");"#
                .into(),
        },
    ];
    let inferred = compile_modules(&inputs, 1).unwrap();
    assert_eq!(inferred.resume.version, 1);
    assert_eq!(inferred.resume.states.len(), 1);
    let root = inferred.resume.root.as_ref().unwrap();
    assert_eq!(root.state.name, "game.state");
    inferred
        .program
        .verify_resume(inferred.resume.clone(), &[], ResumeLimits::default())
        .unwrap();

    inputs[1].source = format!(r#"{}state(State, "game.state");"#, inputs[1].source).into();
    let explicit = compile_modules(&inputs, 1).unwrap();
    assert_eq!(inferred.resume.states, explicit.resume.states);
    assert_eq!(inferred.resume.root, explicit.resume.root);
}

#[test]
fn module_aliases_merge_inherited_contracts_and_required_fields() {
    let inputs=[
        SourceModule{canonical_path:"data".into(),source_path:None,version:"1".into(),source:r#"use std::resume::state;pub class Base{let identity:int;fn init(self){self.identity=7;}}state(Base,"base",fields:{identity:"id"});"#.into()},
        SourceModule{canonical_path:"game".into(),source_path:None,version:"1".into(),source:r#"use data::Base as Parent;use std::resume::{state,root};class State:Parent{let value:any=null;}state(State,"state");fn main(value:State=State()){value}root(main,"game");"#.into()},
    ];
    let compiled = compile_modules(&inputs, 1).unwrap();
    let table = compiled
        .program
        .verify_resume(compiled.resume, &[], ResumeLimits::default())
        .unwrap();
    let contracts = table
        .normalized_state_contracts(&[], ResumeLimits::default())
        .unwrap();
    let state = contracts
        .states
        .iter()
        .find(|s| s.schema.key.name == "state")
        .unwrap();
    assert_eq!(state.required.len(), 1);
    let field = &state.schema.fields[state.required[0] as usize];
    assert_eq!(field.key.owner.name, "base");
    assert_eq!(field.key.name, "id");
}

#[test]
fn automatic_registration_defers_global_reads_and_releases_cancelled_roots() {
    let compiled = compile(SOURCE).unwrap();
    let checked = compiled
        .program
        .verify_resume(compiled.resume.clone(), &[], ResumeLimits::default())
        .unwrap();
    let mut vm = Vm::load(
        compiled.program.clone(),
        bc::RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(2),
    )
    .unwrap();
    let roots = vm.heap().stats().roots;
    let mut cursor = RegistryInitCursor::new(&mut vm, &checked, RootOwner(7)).unwrap();
    assert!(!cursor.step(&mut vm, 0).unwrap());
    let error = loop {
        match cursor.step(&mut vm, 1) {
            Err(e) => break e,
            Ok(false) => {}
            Ok(true) => panic!("uninitialized class accepted"),
        }
    };
    assert!(error.to_string().contains("initialization"), "{error}");
    cursor.cancel(vm.heap_mut()).unwrap();
    assert_eq!(vm.heap().stats().roots, roots);
    run(&mut vm, compiled.initializer, &[]);
    let mut cursor = RegistryInitCursor::new(&mut vm, &checked, RootOwner(7)).unwrap();
    while !cursor.step(&mut vm, 1).unwrap() {}
    cursor
        .finish(vm.heap_mut())
        .unwrap()
        .release(vm.heap_mut())
        .unwrap();
}

#[test]
fn normalized_baseline_ignores_source_names_layout_and_function_positions() {
    let a=compile(r#"use std::resume::{state,root};
        class First{let id:int;}state(First,"z");
        class Second{let values:array=[];let peer:First|null=null;}state(Second,"a",fields:{values:"rows"});
        fn main(value:Second=Second()){value}root(main,"game");
    "#).unwrap();
    let b=compile(r#"use std::resume::{state,root};fn moved(){42}
        class RenamedSecond{let peer:RenamedFirst|null=null;let items:array=[];}state(RenamedSecond,"a",fields:{items:"rows"});
        class RenamedFirst{let identifier:int;}state(RenamedFirst,"z",fields:{identifier:"id"});
        fn current(value:RenamedSecond=RenamedSecond()){value}root(current,"game");
    "#).unwrap();
    let a = a
        .program
        .verify_resume(a.resume, &[], ResumeLimits::default())
        .unwrap()
        .normalized_state_contracts(&[], ResumeLimits::default())
        .unwrap();
    let b = b
        .program
        .verify_resume(b.resume, &[], ResumeLimits::default())
        .unwrap()
        .normalized_state_contracts(&[], ResumeLimits::default())
        .unwrap();
    assert_eq!(a, b);
}
