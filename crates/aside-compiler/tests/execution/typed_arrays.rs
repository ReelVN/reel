use super::*;

#[test]
fn typed_arrays_construct_in_context_and_enforce_alias_writes() {
    let source = r#"
        fn checked(xs: array<int> = []) -> array<int> { xs }
        fn add(a: int, b: int) -> int { a + b }
        fn main() -> int {
            let values: array<int> = [3, 1];
            let empty = checked();
            let supplied = checked([2, ...empty]);
            let nested: array<array<int>>=[[]];
            nested[0].append(4);
            let loose: any = values;
            let mut rejected = 0;
            try { loose[0] = "wrong"; } catch e { rejected += 1; }
            try { loose.append("wrong"); } catch e { rejected += 1; }
            try { loose.extend([8, "wrong"]); } catch e { rejected += 1; }
            try { loose.insert(0, "wrong"); } catch e { rejected += 1; }
            try { loose.resize(3, "wrong"); } catch e { rejected += 1; }
            if values.len() != 2 { throw "partially committed array"; }
            values.sort();
            let slice: array<int> = values.slice(0..1);
            let filtered: array<int> = values.filter(|item| item == 3);
            if !(slice is array<int>) || !(filtered is array<int>) || (values is array<float>) {
                throw "array identity";
            }
            if [] is array<int> { throw "untyped literal"; }
            add(...values) + supplied[0] + nested[0][0] + rejected
        }
    "#;
    let analyzed = analyze_module(source, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(
        analyzed.diagnostics.is_empty(),
        "{:?}",
        analyzed.diagnostics
    );
    let compiled = lowering::compile(
        &analyzed.into_module().unwrap(),
        &lowering::Linkage::default(),
        &[],
        lowering::Options::default(),
    )
    .unwrap();
    let package = compiled.program.package();
    let entry = bc::FunctionId(
        package
            .functions
            .iter()
            .position(|f| package.symbols[f.name.index()] == "main")
            .unwrap() as u32,
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
    let init = vm
        .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
        .unwrap();
    assert!(finish(&mut vm, init, true).same_bits(Value::NULL));
    let main = vm
        .start(entry, &[], RootOwner(1), bc::Context::Ordinary)
        .unwrap();
    assert_eq!(finish(&mut vm, main, true).as_int(), Some(15));
}
