//! Compiler-only contracts. These tests never dispatch bytecode or invoke a host.
use aside_compiler::{AnalysisOptions, analyze_module, hir, lowering::{self, Linkage, Options}, semantic::NoModules};
use aside_vm::bytecode as bc;

fn analyzed(source: &str) -> hir::AnalyzedModule {
    let result = analyze_module(source, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:#?}", result.diagnostics);
    result.into_module().unwrap()
}
fn compiled(source: &str) -> lowering::Compilation {
    let module = analyzed(source);
    let ir = lowering::lower(&module, &Linkage::default(), Options::default()).unwrap();
    assert!(ir.functions().iter().all(|f| f.layout.iter().all(|b| f.blocks[b.0 as usize].terminator.is_some())));
    let result = ir.emit().unwrap();
    let encoded = bc::encode(result.program.package(), bc::Limits::default()).unwrap();
    let decoded = bc::decode(&encoded, bc::Limits::default()).unwrap();
    assert_eq!(result.program.package(), &decoded);
    let roundtrip = bc::verify(decoded, bc::Limits::default()).unwrap();
    let mut disassembly = String::new();
    bc::disassemble(&roundtrip, &mut disassembly).unwrap();
    assert!(disassembly.contains("Return"));
    for (i, function) in result.program.package().functions.iter().enumerate() {
        assert_eq!(function.code.len(), function.source_map.len());
        for pc in 0..function.code.len() {
            let span = result.program.source_at(bc::FunctionId(i as u32), bc::Pc(pc as u32)).unwrap();
            assert!(source.is_char_boundary(span.start as usize));
            assert!(source.is_char_boundary(span.end as usize));
        }
        assert_temporary_initialization(function);
    }
    result
}
fn function<'a>(p: &'a bc::BytecodePackage, name: &str) -> &'a bc::FunctionDef {
    p.functions.iter().find(|f| p.symbols[f.name.index()] == name).unwrap()
}

#[test]
fn scalar_string_arithmetic_and_defaults_roundtrip() {
    let result = compiled(r#"
        fn add(a: int, b: int = 1) -> int { a + b }
        fn text() { "中文\n\u{1f642}" }
        fn raw() { r"a\nb" }
        fn minimum() { -9223372036854775808 }
        fn radix() { 0xff + 0o10 + 0b10 + 1_000 }
        fn float() { -1.25e2 }
        let mut result = 1;
        result += add(2);
    "#);
    let p = result.program.package();
    assert!(p.constants.contains(&bc::Const::Utf8("中文\n🙂".into())));
    assert!(p.constants.contains(&bc::Const::Utf8(r"a\nb".into())));
    assert!(p.constants.contains(&bc::Const::Int(i64::MIN)));
    assert!(p.constants.contains(&bc::Const::FloatBits((-125.0f64).to_bits())));
    let f = function(p, "add");
    assert_eq!(f.bindings[0].storage.reg(), bc::Reg(0));
    assert_eq!(f.bindings[1].storage.reg(), bc::Reg(1));
    let signature = &p.signatures[f.signature.index()];
    let default = &p.functions[signature.parameters[1].default.unwrap().index()];
    assert_eq!(default.context, bc::Context::NoWait);
    assert_eq!(default.bindings[0].name, f.bindings[0].name);
    assert_eq!(default.bindings[0].storage, f.bindings[0].storage);
}

#[test]
fn dynamic_calls_have_contiguous_windows_and_preserve_live_values() {
    let result = compiled("fn one() { 1 } fn two() { 2 } fn take(a,b) { a+b } fn order() { one() + take(two(), b: one()) }");
    let p = result.program.package();
    let f = function(p, "order");
    let calls: Vec<_> = f.code.iter().filter(|i| i.opcode == bc::Opcode::Call as u16).map(|i| {
        let bc::Descriptor::Call(call) = &f.descriptors[i.operands[1] as usize] else { panic!() };
        call
    }).collect();
    assert_eq!(calls.len(), 4);
    let bc::Arguments::Window(window) = calls[3].arguments else { panic!() };
    assert_eq!(window.len, 2);
    assert_eq!(calls[3].names[0].offset, 1);
    assert_eq!(p.symbols[calls[3].names[0].name.index()], "b");
    let pcs: Vec<_> = f.code.iter().enumerate().filter(|(_, i)| i.opcode == bc::Opcode::Call as u16).map(|(pc, _)| pc).collect();
    let spans: Vec<_> = pcs.iter().map(|&pc| &p.sources[0].text[f.source_map[pc].span.start as usize..f.source_map[pc].span.end as usize]).collect();
    assert_eq!(spans, ["one()", "two()", "one()", "take(two(), b: one())"]);
}

#[test]
fn branches_short_circuit_and_null_coalescing_have_explicit_edges() {
    let result = compiled("fn next() { true } fn branch(x,y) { if x { y ?? next() } else { x && next() || y } } fn terminal(x) { if x { return 1; } else { return 2; } }");
    let f = function(result.program.package(), "branch");
    assert!(f.code.iter().any(|i| i.opcode == bc::Opcode::BranchNull as u16));
    assert!(f.code.iter().filter(|i| i.opcode == bc::Opcode::BranchBool as u16).count() >= 5);
}

#[test]
fn forward_readonly_cells_and_mutual_recursion_keep_binding_identity() {
    let result = compiled("fn outer() { fn first() { later } fn a() { b() } fn b() { a() } let later = null; first } fn mutable() { let mut value=0; let next=|| { value += 1; value }; next }");
    let p = result.program.package();
    let outer = function(p, "outer");
    let later = outer.bindings.iter().find(|b| p.symbols[b.name.index()] == "later").unwrap();
    assert!(!later.mutable);
    assert!(matches!(later.storage, bc::BindingStorage::Cell(_)));
    let first = function(p, "first");
    assert_eq!(first.captures, [bc::CaptureKind::Cell]);
    let new_pc = outer.code.iter().position(|i| i.opcode == bc::Opcode::CellNew as u16 && outer.bindings[i.operands[0] as usize].name == later.name).unwrap();
    let closure_pc = outer.code.iter().position(|i| i.opcode == bc::Opcode::MakeClosure as u16).unwrap();
    let init_pc = outer.code.iter().position(|i| i.opcode == bc::Opcode::BindInit as u16 && outer.bindings[i.operands[0] as usize].name == later.name).unwrap();
    assert!(new_pc < closure_pc && closure_pc < init_pc);
    assert!(p.constants.contains(&bc::Const::Null));
}

#[test]
fn try_catch_finally_uses_shared_region_contract_and_cleanup_queries() {
    let result = compiled("fn f(x) { try { try { if x { return 1; } throw x; } finally { let y=2; } } catch error { try { throw; } finally { let z=3; } } finally { let w=4; } }");
    let p = result.program.package();
    let id = bc::FunctionId(p.functions.iter().position(|f| p.symbols[f.name.index()] == "f").unwrap() as u32);
    let f = &p.functions[id.index()];
    assert_eq!(f.regions.len(), 3);
    let pc = bc::Pc(f.code.iter().position(|i| i.opcode == bc::Opcode::Return as u16).unwrap() as u32);
    let first = result.program.cleanup_handler(id, pc, bc::CleanupDestination::Return, None).unwrap().unwrap();
    let second = result.program.cleanup_handler(id, pc, bc::CleanupDestination::Return, Some(first.region)).unwrap().unwrap();
    assert_ne!(first.region, second.region);
    assert!(result.program.cleanup_handler(id, pc, bc::CleanupDestination::Return, Some(second.region)).unwrap().is_none());
    assert!(f.code.iter().any(|i| i.opcode == bc::Opcode::Rethrow as u16));
}

#[test]
fn sequential_temporaries_are_reused_and_limits_fail_before_growth() {
    let mut source = String::from("fn f() {");
    for _ in 0..500 { source.push_str("1+2;"); }
    source.push_str("3 }");
    let result = compiled(&source);
    assert!(function(result.program.package(), "f").register_count <= 4);
    let module = analyzed(&source);
    for limits in [lowering::Limits { max_work: 1, ..Default::default() }, lowering::Limits { max_capacity_bytes: 64, ..Default::default() },
        lowering::Limits { max_instructions: 5, ..Default::default() }, lowering::Limits { max_registers: 1, ..Default::default() }, lowering::Limits { max_blocks: 0, ..Default::default() }] {
        let error = lowering::lower(&module, &Linkage::default(), Options { limits, ..Default::default() }).unwrap_err();
        assert_eq!(error.kind, lowering::ErrorKind::Limit);
        assert!(error.stats.capacity_peak_bytes <= limits.max_capacity_bytes);
        assert!(error.stats.work_units <= limits.max_work);
    }
}

#[test]
fn unsupported_nodes_report_original_range() {
    for (source, node_text) in [("fn f() { [1,2] }", "[1,2]"), ("fn f() { let (a,b)=(1,2); }", "(a,b)"), ("fn f() { f\"value {1}\" }", "f\"value {1}\"")] {
        let module = analyzed(source);
        let error = lowering::lower(&module, &Linkage::default(), Options::default()).unwrap_err();
        assert_eq!(error.kind, lowering::ErrorKind::Unsupported);
        assert_eq!(&source[aside_compiler::byte_range(error.span)], node_text);
    }
}

/// Definite initialization of temporary registers over the actual shared CFG.
/// Fixed binding slots have independent runtime initialization checks, so an
/// intentional forward BindRead remains valid compiler output.
fn assert_temporary_initialization(f: &bc::FunctionDef) {
    let mut incoming: Vec<Option<Vec<bool>>> = vec![None; f.code.len()];
    let entry = vec![false; f.register_count as usize];
    incoming[0] = Some(entry.clone());
    for region in &f.regions {
        if let Some(catch) = &region.catch { let mut state=entry.clone(); state[catch.exception.index()]=true; incoming[catch.range.start.index()]=Some(state); }
        if let Some(finally)=region.finally { incoming[finally.start.index()]=Some(entry.clone()); }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for pc in 0..f.code.len() {
            let Some(mut state)=incoming[pc].clone() else { continue; };
            let i=f.code[pc];
            let spec=bc::instruction_spec(i.opcode).unwrap();
            spec.visit_registers(f,&i,|reg, access| {
                if reg.index() >= f.bindings.len() && matches!(access,bc::Access::Read|bc::Access::ReadWrite) { assert!(state[reg.index()], "temporary r{} not initialized at PC {} {:?}",reg.0,pc,spec.opcode); }
            }).unwrap();
            spec.visit_registers(f,&i,|reg,access| { if matches!(access,bc::Access::Write|bc::Access::ReadWrite) { state[reg.index()]=i.opcode!=bc::Opcode::Clear as u16; } }).unwrap();
            let targets: Vec<usize> = match spec.flow {
                bc::Flow::Next => vec![pc+1], bc::Flow::Jump|bc::Flow::Leave => vec![i.operands[0] as usize], bc::Flow::Branch => vec![i.operands[1] as usize,i.operands[2] as usize], _=>vec![],
            };
            for target in targets {
                let next=match &incoming[target] { Some(old)=>old.iter().zip(&state).map(|(a,b)| *a && *b).collect(), None=>state.clone() };
                if incoming[target].as_ref()!=Some(&next) { incoming[target]=Some(next); changed=true; }
            }
        }
    }
}
