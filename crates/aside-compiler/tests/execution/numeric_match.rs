use super::*;

#[test]
fn numeric_patterns_compare_constants_directly_in_a_packed_program() {
    let source = r#"
        fn choose(index: any, enabled: bool) -> int {
            match index {
                7 if enabled => 1,
                7.0 => 2,
                -1.5 => 4,
                9007199254740993 => 8,
                9007199254740992.0 => 16,
                -9223372036854775808 => 32,
                _ => 64
            }
        }
        fn main() -> int {
            choose(7.0, true) + choose(7, false) + choose(-1.5, false)
                + choose(9007199254740993, false) + choose(9007199254740992, false)
                + choose(-9223372036854775808, false) + choose("7", false)
        }
    "#;
    let module = analyze_module(source, &AnalysisOptions::default(), &mut NoModules)
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
    let package = compiled.program.package();
    let function = |name: &str| {
        package
            .functions
            .iter()
            .position(|function| package.symbols[function.name.index()] == name)
            .unwrap()
    };
    let choose = &package.functions[function("choose")];
    assert_eq!(
        choose
            .code
            .iter()
            .map(Result::unwrap)
            .filter(|instruction| instruction.opcode == bc::Opcode::EqNumber as u16)
            .count(),
        6
    );
    let main = bc::FunctionId(function("main") as u32);
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
    finish(&mut vm, init, false);
    let task = vm
        .start(main, &[], RootOwner(1), bc::Context::Ordinary)
        .unwrap();
    assert_eq!(finish(&mut vm, task, true).as_int(), Some(127));
}
