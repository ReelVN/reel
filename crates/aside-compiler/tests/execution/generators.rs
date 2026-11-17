//! Generator behavior through the same source and serialized bytecode paths.
use super::*;

#[test]
fn lazy_generators_keep_defaults_results_faults_and_loop_cleanup() {
    let source = r#"
fn main() {
    let log = [];
    let mut defaults = 0;
    fn default_value() { defaults += 1; 5 }
    gen fn values(seed: int = default_value()) -> int {
        log.append("entered");
        try {
            yield null;
            let received = yield seed;
            if received != null { throw "resume value"; }
            return seed + 1;
        } finally { log.append("complete"); }
    }
    let factory: fn() -> generator = values;
    let stream = factory();
    if defaults != 1 || log.len() != 0 { throw "call and body timing"; }
    let next = stream.next;
    if next() != (false, null) || log.len() != 1 { throw "null yield"; }
    if next() != (false, 5) { throw "second item"; }
    if next() != (true, 6) || next() != (true, null) { throw "terminal result"; }
    stream.close(); stream.close();
    if log.len() != 2 || log[0] != "entered" || log[1] != "complete" {
        throw "completion cleanup";
    }

    gen fn guarded(tag) {
        try { yield 1; yield 2; }
        finally { log.append(tag); }
    }
    let unused = guarded("unused");
    unused.close();
    if unused.next() != (true, null) || log.len() != 2 { throw "unstarted close"; }
    for item in guarded("break") { break; }
    if log[2] != "break" { throw "break cleanup"; }
    fn returned() { for item in guarded("return") { return 42; } 0 }
    if returned() != 42 || log[3] != "return" { throw "return cleanup"; }
    let mut total = 0;
    for item in guarded("continue") { total += item; continue; }
    if total != 3 || log[4] != "continue" { throw "continue retains iterator"; }
    let mut caught = false;
    try {
        for item in guarded("throw") { throw "body failure"; }
    } catch error { caught = error == "body failure"; }
    if !caught || log[5] != "throw" { throw "throw cleanup"; }
    outer: for item in guarded("outer") {
        for nested in guarded("inner") { continue outer; }
    }
    if log[6] != "inner" || log[7] != "inner" || log[8] != "outer" {
        throw "labeled continue cleanup";
    }
    outer_break: for item in guarded("outer break") {
        for nested in guarded("inner break") { break outer_break; }
    }
    if log[9] != "inner break" || log[10] != "outer break" {
        throw "labeled break cleanup";
    }

    gen fn failed() {
        try { yield 1; throw "generator failure"; }
        finally { log.append("failed"); }
    }
    let failure = failed();
    if failure.next() != (false, 1) { throw "fault setup"; }
    caught = false;
    try { failure.next(); } catch error { caught = error == "generator failure"; }
    if !caught || failure.next() != (true, null) || log[11] != "failed" {
        throw "escaped fault closes";
    }
    let closing = guarded("explicit");
    closing.next(); closing.close(); closing.close();
    if log.len() != 13 || log[12] != "explicit" || closing.next() != (true, null) {
        throw "explicit close once";
    }
    class Counter {
        let seed = 8;
        gen fn items(self) { yield self.seed; }
    }
    let bound = Counter().items;
    if bound().next() != (false, 8) { throw "bound generator receiver"; }
    42
}
"#;
    let analysis = analyze_module(source, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let compiled = lowering::compile(
        &analysis.into_module().unwrap(),
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
            .position(|function| package.symbols[function.name.index()] == "main")
            .unwrap() as u32,
    );
    let bytes = bc::encode(package, bc::Limits::default()).unwrap();
    for serialized in [false, true] {
        let program = if serialized {
            bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap()
        } else {
            compiled.program.clone()
        };
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
            .start(entry, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        assert_eq!(
            finish(&mut vm, task, false).as_int(),
            Some(42),
            "serialized={serialized}"
        );
    }
}
