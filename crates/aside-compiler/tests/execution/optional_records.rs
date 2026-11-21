use super::*;

#[test]
fn optional_record_checks_absence_distinct_from_present_null() {
    let source = r#"
        type View = {title: string, count?: int, note?: string | null};
        fn value(x: any) -> any { x }
        fn make(x: any) -> View { x }
        fn main() -> int {
            let raw = value({title: "背包"});
            let view = make(raw);
            if view.has_own("count") || view.count != null || view.get("count", default: 6) != 6 {
                throw "missing optional field";
            }
            if !match view { {title} => true, _ => false } {
                throw "exact pattern uses actual keys";
            }
            view.count = 40;
            view.note = null;
            if !view.has_own("note") || view.note != null {
                throw "present null";
            }
            let mut rejected = 0;
            try { let _ = make(value({title: "item", count: null})); } catch error { rejected += 1; }
            try { let _ = make(value({title: "item", extra: 2})); } catch error { rejected += 1; }
            try { view.count = value(null); } catch error { rejected += 1; }
            raw.count = null;
            try { let _ = view.count; } catch error { rejected += 1; }
            view.count = 38;
            view.count + rejected
        }
    "#;
    let analysis = analyze_module(source, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let module = analysis.into_module().unwrap();
    let compiled = lowering::compile(
        &module,
        &lowering::Linkage::default(),
        &[],
        lowering::Options::default(),
    )
    .unwrap();
    let package = compiled.program.package();
    let entry = bc::FunctionId(
        package.functions.iter().position(|f| package.symbols[f.name.index()] == "main").unwrap() as u32,
    );
    let bytes = bc::encode(package, bc::Limits::default()).unwrap();
    let program = bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap();
    let mut vm = Vm::load(
        program,
        bc::RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap();
    let task = vm.start(compiled.entry, &[], RootOwner(1), bc::Context::Init).unwrap();
    finish(&mut vm, task, false);
    let task = vm.start(entry, &[], RootOwner(1), bc::Context::Ordinary).unwrap();
    assert_eq!(finish(&mut vm, task, true).as_int(), Some(42));
}
