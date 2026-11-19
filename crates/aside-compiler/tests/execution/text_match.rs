use super::*;

#[test]
fn typed_short_text_patterns_keep_order_in_a_packed_program() {
    let source = r#"
        fn choose(text: string, earlier: bool) -> int {
            match text {
                "same" if earlier => 3,
                "same" => 6,
                ("界" | "\0") => 8,
                "" => 8,
                "extra" => 0,
                "skip" => 0,
                "unused" => 0,
                "01234567890123456789012345678901234567890123456789012345678901234567890123456789" => 9,
                _ => 0,
            }
        }
        fn main() -> int {
            choose("same", true) + choose("same", false) + choose("界", false)
                + choose("\0", false) + choose("", false) + choose("absent", false)
                + choose("01234567890123456789012345678901234567890123456789012345678901234567890123456789", false)
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
    let choose = package
        .functions
        .iter()
        .find(|function| package.symbols[function.name.index()] == "choose")
        .unwrap();
    let count = |opcode: bc::Opcode| {
        choose
            .code
            .iter()
            .map(Result::unwrap)
            .filter(|instruction| instruction.opcode == opcode as u16)
            .count()
    };
    assert_eq!(count(bc::Opcode::Is), 8);
    assert_eq!(count(bc::Opcode::Eq), 1);
    let main = bc::FunctionId(
        package
            .functions
            .iter()
            .position(|function| package.symbols[function.name.index()] == "main")
            .unwrap() as u32,
    );
    let mut release = package.clone();
    let pool = bc::externalize_text(&mut release).unwrap();
    let bytes = bc::encode(&release, bc::Limits::default()).unwrap();
    let program =
        bc::VerifiedProgram::decode_with_text_pool(&bytes, bc::Limits::default(), pool).unwrap();
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
    assert_eq!(finish(&mut vm, task, true).as_int(), Some(42));
}
