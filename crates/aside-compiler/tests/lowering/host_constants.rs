use super::*;
use aside_compiler::semantic::{CompileConstant, ExternalSymbol, MemoryResolver, ModuleInterface};

fn target(source: &str, target: &str) -> lowering::Compilation {
    let mut resolver = MemoryResolver::default();
    resolver.insert(ModuleInterface {
        canonical_path: "platform".into(),
        version: "1".into(),
        exports: vec![ExternalSymbol::constant(
            "target",
            CompileConstant::Utf8(target.into()),
        )],
    });
    let analysis = analyze_module(source, &AnalysisOptions::default(), &mut resolver).unwrap();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    lowering::compile(
        &analysis.into_module().unwrap(),
        &Linkage::default(),
        &[],
        Options::default(),
    )
    .unwrap()
}

#[test]
fn imported_constants_select_branches_and_preserve_shadowing_and_dynamic_conditions() {
    let source = r#"
        use platform;
        use platform::target as selected;
        fn selected_branch() {
            if platform.target == "vita" && !(selected != "vita") { 117 } else { 229 }
        }
        fn dynamic(flag: bool) { if flag { 331 } else { 443 } }
        fn shadowed() {
            let platform = { target: "desktop" };
            if platform.target == "vita" { 557 } else { 661 }
        }
    "#;
    for (name, present, absent) in [("vita", 117, 229), ("desktop", 229, 117)] {
        let compiled = target(source, name);
        let constants = &compiled.program.package().constants;
        assert!(constants.contains(&bc::Const::Int(present)));
        assert!(!constants.contains(&bc::Const::Int(absent)));
        for value in [331, 443, 557, 661] {
            assert!(constants.contains(&bc::Const::Int(value)));
        }
    }
}

#[test]
fn constant_conditions_keep_boolean_short_circuit_and_float_equality() {
    let compiled = target(
        r#"
        use platform;
        fn fail() { throw "called"; }
        fn main() {
            if (platform.target == "vita" || fail()) && -0.0 == 0.0 { 773 } else { 887 }
        }
    "#,
        "vita",
    );
    assert!(
        compiled
            .program
            .package()
            .constants
            .contains(&bc::Const::Int(773))
    );
    assert!(
        !compiled
            .program
            .package()
            .constants
            .contains(&bc::Const::Int(887))
    );
}

#[test]
fn unselected_branches_still_have_source_diagnostics() {
    let analysis = analyze_module(
        "fn main() { if false { missing(); } }",
        &AnalysisOptions::default(),
        &mut NoModules,
    )
    .unwrap();
    assert!(!analysis.is_ok());
}

#[test]
fn source_aliases_keep_runtime_initialization_and_reads() {
    let compiled = target(
        r#"
        use platform;
        fn main() {
            let selected = platform.target;
            if selected == "vita" { 11223 } else { 33445 }
        }
    "#,
        "vita",
    );
    let constants = &compiled.program.package().constants;
    assert!(constants.contains(&bc::Const::Int(11223)));
    assert!(constants.contains(&bc::Const::Int(33445)));
}

#[test]
fn unselected_closures_classes_and_defaults_have_no_executable_or_constant_payload() {
    let compiled = target(
        r#"
        use platform;
        fn main() {
            if platform.target == "vita" {
                let call = || 11111;
                call()
            } else {
                fn discarded(arg = 22222) { 33333 }
                class Unused { fn sample(self) { 44444 } }
                let closure = || 55555;
                (discarded(), Unused(), closure())
            }
        }
    "#,
        "vita",
    );
    let package = compiled.program.package();
    assert!(package.constants.contains(&bc::Const::Int(11111)));
    for value in [22222, 33333, 44444, 55555] {
        assert!(
            !package.constants.contains(&bc::Const::Int(value)),
            "{value}"
        );
    }
    assert!(
        compiled
            .metadata
            .functions
            .iter()
            .filter(|f| f.output.is_none())
            .count()
            >= 3
    );
}
