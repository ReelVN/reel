//! Compiler-only contracts. These tests never dispatch bytecode or invoke a host.
use aside_compiler::{
    AnalysisOptions, analyze_module, hir,
    lowering::{self, Linkage, Options},
    semantic::NoModules,
};
use aside_vm::bytecode as bc;

fn clears(instruction: &bc::Instruction, register: u32) -> bool {
    let count = match bc::instruction_spec(instruction.opcode).unwrap().opcode {
        bc::Opcode::Clear => 1,
        bc::Opcode::Clear2 => 2,
        bc::Opcode::Clear3 => 3,
        _ => 0,
    };
    instruction.operands[..count].contains(&register)
}

#[path = "lowering/call_arguments.rs"]
mod call_arguments;
#[path = "lowering/collections_locations.rs"]
mod collections_locations;
#[path = "lowering/discard.rs"]
mod discard;
#[path = "lowering/host_constants.rs"]
mod host_constants;
#[path = "lowering/initializer_locals.rs"]
mod initializer_locals;
#[path = "lowering/members.rs"]
mod members;
#[path = "lowering/optional_chains.rs"]
mod optional_chains;

fn analyzed(source: &str) -> hir::AnalyzedModule {
    let result = analyze_module(source, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:#?}", result.diagnostics);
    result.into_module().unwrap()
}
fn compiled(source: &str) -> lowering::Compilation {
    let module = analyzed(source);
    let ir = lowering::lower(&module, &Linkage::default(), Options::default()).unwrap();
    assert!(ir.functions().iter().all(|f| {
        f.layout
            .iter()
            .all(|b| f.blocks[b.0 as usize].terminator.is_some())
    }));
    let result = ir.emit(&[]).unwrap();
    let encoded = bc::encode(result.program.package(), bc::Limits::default()).unwrap();
    let decoded = bc::decode(&encoded, bc::Limits::default()).unwrap();
    assert_eq!(result.program.package(), &decoded);
    let roundtrip = bc::verify(decoded, bc::Limits::default()).unwrap();
    let mut disassembly = String::new();
    bc::disassemble(&roundtrip, &mut disassembly).unwrap();
    assert!(disassembly.contains("Return"));
    for (i, function) in result.program.package().functions.iter().enumerate() {
        assert!(function.source_map.len() <= function.code.len());
        for (pc, _) in function.code.indexed_iter().map(Result::unwrap) {
            let span = result
                .program
                .source_at(bc::FunctionId(i as u32), bc::Pc(pc as u32))
                .unwrap();
            assert!(source.is_char_boundary(span.start as usize));
            assert!(source.is_char_boundary(span.end as usize));
        }
        assert_temporary_initialization(function);
    }
    result
}

#[test]
fn generators_lower_yield_destinations_defaults_and_owned_for_cleanup() {
    let result = compiled(
        r#"
gen fn items(value: int = 3) -> bool {
    try { let resumed = yield value; yield; return resumed == null; }
    finally { let finished = true; }
}
fn consume() {
    outer: for value in items() {
        for nested in items(value) {
            if nested == 0 { continue; }
            if nested == 1 { continue outer; }
            if nested == 2 { break outer; }
            if nested == 3 { return nested; }
            throw nested;
        }
    }
    0
}
"#,
    );
    let package = result.program.package();
    let generator = package
        .functions
        .iter()
        .find(|function| package.symbols[function.name.index()] == "items")
        .unwrap();
    assert_eq!(generator.kind, bc::FunctionKind::Generator);
    assert_eq!(generator.context, bc::Context::NoWait);
    assert_eq!(
        generator
            .code
            .iter()
            .map(Result::unwrap)
            .filter(|op| op.opcode == bc::Opcode::Yield as u16)
            .count(),
        2
    );
    let consumer = package
        .functions
        .iter()
        .find(|function| package.symbols[function.name.index()] == "consume")
        .unwrap();
    assert_eq!(consumer.regions.len(), 2);
    for region in &consumer.regions {
        let cleanup = region.finally.unwrap();
        let code = &consumer
            .code
            .indexed_iter()
            .map(Result::unwrap)
            .filter(|(pc, _)| cleanup.contains(bc::Pc(*pc as u32)))
            .map(|(_, i)| i)
            .collect::<Vec<_>>();
        assert_eq!(
            code.iter()
                .filter(|op| op.opcode == bc::Opcode::IterClose as u16)
                .count(),
            1
        );
        assert!(
            code.iter()
                .any(|op| op.opcode == bc::Opcode::EndFinally as u16)
        );
    }
}
fn source_span(f: &bc::RuntimeFunction, pc: usize) -> bc::SourceSpan {
    f.source_map.source_at(bc::Pc(pc as u32)).unwrap()
}

#[test]
fn source_runs_preserve_computation_and_control_flow_locations() {
    let module = analyzed(
        r#"fn repeat(value = "same") { try { if value == "same" { return value + "same"; } throw value; } finally { let cleanup = "same"; } } let result = repeat();"#,
    );
    let ir = lowering::lower(&module, &Linkage::default(), Options::default()).unwrap();
    let expected: Vec<_> = ir
        .functions()
        .iter()
        .map(|function| {
            let spans: Vec<_> = function
                .layout
                .iter()
                .flat_map(|id| {
                    let block = &function.blocks[id.0 as usize];
                    block
                        .operations
                        .iter()
                        .filter(|operation| {
                            !matches!(
                                bc::instruction_spec(operation.instruction.opcode)
                                    .unwrap()
                                    .opcode,
                                bc::Opcode::Move
                                    | bc::Opcode::Clear
                                    | bc::Opcode::Clear2
                                    | bc::Opcode::Clear3
                            )
                        })
                        .map(|operation| (Some(operation.instruction.opcode), operation.span))
                        .chain(block.terminator.as_ref().map(|(_, span)| (None, *span)))
                })
                .collect();
            (function.output, spans)
        })
        .collect();
    let compiled = ir.emit(&[]).unwrap();
    assert!(
        compiled
            .program
            .package()
            .functions
            .iter()
            .any(|f| f.source_map.len() < f.code.len())
    );
    for (function, spans) in expected {
        let computation: Vec<_> = compiled.program.package().functions[function.index()]
            .code
            .indexed_iter()
            .map(Result::unwrap)
            .filter(|(_, instruction)| {
                !matches!(
                    bc::instruction_spec(instruction.opcode).unwrap().opcode,
                    bc::Opcode::Move | bc::Opcode::Clear | bc::Opcode::Clear2 | bc::Opcode::Clear3
                )
            })
            .collect();
        let mut spans = spans.into_iter().peekable();
        for (pc, instruction) in computation {
            let actual = compiled
                .program
                .source_at(function, bc::Pc(pc as u32))
                .unwrap();
            // Initialized frame-local reads may be borrowed by their consumer.
            // Every retained read and all other computations keep their span.
            while spans.peek().is_some_and(|(opcode, span)| {
                *opcode == Some(bc::Opcode::BindRead as u16)
                    && (instruction.opcode != bc::Opcode::BindRead as u16
                        || (u32::from(span.start()), u32::from(span.end()))
                            != (actual.start, actual.end))
            }) {
                spans.next();
            }
            let (opcode, expected) = spans.next().expect("retained computation");
            if let Some(opcode) = opcode {
                assert_eq!(instruction.opcode, opcode);
            }
            assert_eq!(
                (actual.start, actual.end),
                (u32::from(expected.start()), u32::from(expected.end()))
            );
        }
        assert!(spans.all(|(opcode, _)| opcode == Some(bc::Opcode::BindRead as u16)));
    }
    assert_eq!(
        compiled
            .program
            .package()
            .constants
            .iter()
            .filter(|c| matches!(c, bc::Const::Utf8(s) if s == "same"))
            .count(),
        1
    );
}

#[test]
fn repeated_value_constants_share_slots_without_merging_object_constants() {
    let compiled = compiled("fn f(){[null,null,true,true,false,false,42,42,1.25,1.25,(1,),(1,)]}");
    let constants = &compiled.program.package().constants;
    for expected in [
        bc::Const::Null,
        bc::Const::Bool(true),
        bc::Const::Bool(false),
        bc::Const::Int(42),
        bc::Const::FloatBits(1.25f64.to_bits()),
    ] {
        assert_eq!(
            constants
                .iter()
                .filter(|constant| **constant == expected)
                .count(),
            1
        );
    }
    let function = function(compiled.program.package(), "f");
    assert_eq!(
        function
            .code
            .iter()
            .map(Result::unwrap)
            .filter(|instruction| instruction.opcode == bc::Opcode::MakeTuple as u16)
            .count(),
        2
    );
}
fn function<'a, C, S>(p: &'a bc::BytecodePackage<C, S>, name: &str) -> &'a bc::FunctionDef<C, S> {
    p.functions
        .iter()
        .find(|f| p.symbols[f.name.index()] == name)
        .unwrap()
}

#[test]
fn scalar_string_arithmetic_and_defaults_roundtrip() {
    let result = compiled(
        r#"
        fn add(a: int, b: int = 1) -> int { a + b }
        fn text() { "中文\n\u{1f642}" }
        fn raw() { r"a\nb" }
        fn minimum() { -9223372036854775808 }
        fn radix() { 0xff + 0o10 + 0b10 + 1_000 }
        fn float() { -1.25e2 }
        let mut result = 1;
        result += add(2);
    "#,
    );
    let p = result.program.package();
    assert!(p.constants.contains(&bc::Const::Utf8("中文\n🙂".into())));
    assert!(p.constants.contains(&bc::Const::Utf8(r"a\nb".into())));
    assert_eq!(function(p, "text").signature, function(p, "raw").signature);
    assert!(p.constants.contains(&bc::Const::Int(i64::MIN)));
    assert!(
        p.constants
            .contains(&bc::Const::FloatBits((-125.0f64).to_bits()))
    );
    let f = function(p, "add");
    assert_eq!(f.bindings[0].storage.reg(), bc::Reg(0));
    assert_eq!(f.bindings[1].storage.reg(), bc::Reg(1));
    let signature = &p.signatures[f.signature.index()];
    let Some(bc::ParameterDefault::Constant(id)) = signature.parameters[1].default else {
        panic!("literal default must use the constant pool")
    };
    assert_eq!(p.constants[id.index()], bc::Const::Int(1));
    assert_eq!(signature.parameters[1].ty, f.bindings[1].ty);
    assert!(
        p.functions
            .iter()
            .all(|f| p.symbols[f.name.index()] != "$default")
    );
}

#[test]
fn dynamic_calls_have_contiguous_windows_and_preserve_live_values() {
    let result = compiled(
        "fn one() { 1 } fn two() { 2 } fn take(a,b) { a+b } fn order() { one() + take(two(), b: one()) }",
    );
    let p = result.program.package();
    let f = function(p, "order");
    let calls: Vec<_> = f
        .code
        .iter()
        .map(Result::unwrap)
        .filter(|i| i.opcode == bc::Opcode::Call as u16)
        .map(|i| {
            let bc::Descriptor::Call(call) = &f.descriptors[i.operands[1] as usize] else {
                panic!()
            };
            call
        })
        .collect();
    assert_eq!(calls.len(), 4);
    let bc::Arguments::Window(window) = calls[3].arguments else {
        panic!()
    };
    assert_eq!(window.len, 2);
    assert_eq!(calls[3].names[0].offset, 1);
    assert_eq!(p.symbols[calls[3].names[0].name.index()], "b");
    let pcs: Vec<_> = f
        .code
        .indexed_iter()
        .map(Result::unwrap)
        .filter(|(_, i)| i.opcode == bc::Opcode::Call as u16)
        .map(|(pc, _)| pc)
        .collect();
    let spans: Vec<_> = pcs
        .iter()
        .map(|&pc| {
            &p.sources[0].text[source_span(f, pc).start as usize..source_span(f, pc).end as usize]
        })
        .collect();
    assert_eq!(spans, ["one()", "two()", "one()", "take(two(), b: one())"]);
    let saved_left = f.code.instruction(pcs[0]).unwrap().unwrap().operands[0];
    let add_pc = f
        .code
        .indexed_iter()
        .map(Result::unwrap)
        .filter(|(_, i)| i.opcode == bc::Opcode::Add as u16)
        .last()
        .unwrap()
        .0;
    assert_eq!(
        f.code.instruction(add_pc).unwrap().unwrap().operands[1],
        saved_left
    );
    for i in &f
        .code
        .indexed_iter()
        .map(Result::unwrap)
        .filter(|(pc, _)| *pc > pcs[0] && *pc < add_pc)
        .map(|(_, i)| i)
        .collect::<Vec<_>>()
    {
        bc::instruction_spec(i.opcode)
            .unwrap()
            .visit_registers(f, i, |reg, access| {
                assert!(
                    !(reg.0 == saved_left
                        && matches!(access, bc::Access::Write | bc::Access::ReadWrite)),
                    "live left operand overwritten across a call"
                );
            })
            .unwrap();
    }
}

#[test]
fn branches_short_circuit_and_null_coalescing_have_explicit_edges() {
    let result = compiled(
        "fn next() { true } fn branch(x,y) { if x { y ?? next() } else { x && next() || y } } fn terminal(x) { if x { return 1; } else { return 2; } }",
    );
    let f = function(result.program.package(), "branch");
    assert!(
        f.code
            .iter()
            .map(Result::unwrap)
            .any(|i| i.opcode == bc::Opcode::BranchNull as u16)
    );
    assert!(
        f.code
            .iter()
            .map(Result::unwrap)
            .filter(|i| i.opcode == bc::Opcode::BranchBool as u16)
            .count()
            >= 5
    );
}

#[test]
fn terminal_value_subexpressions_preserve_return_and_short_circuit_edges() {
    let result = compiled(
        "fn called(a,b){a+b} \
         fn in_arg(x){called(a:1,b:if x{return 1;}else{return 2;})} \
         fn in_let(x){let y=if x{return 1;}else{return 2;}; y} \
         fn in_eq(x){null==(if x{return 1;}else{return 2;})} \
         fn in_assign(x){let mut y=0; y+=if x{return 1;}else{return 2;};} \
         fn in_return(x){return if x{return 1;}else{return 2;};} \
         fn in_throw(x){throw if x{return 1;}else{return 2;};} \
         fn in_short(x){x && (if x{return true;}else{false})} \
         fn in_coalesce(x){x ?? (if x{return 1;}else{return 2;})}",
    );
    let p = result.program.package();
    let in_arg = function(p, "in_arg");
    assert!(
        !in_arg
            .code
            .iter()
            .map(Result::unwrap)
            .any(|i| i.opcode == bc::Opcode::Call as u16)
    );
    let in_eq = function(p, "in_eq");
    assert!(
        !in_eq
            .code
            .iter()
            .map(Result::unwrap)
            .any(|i| i.opcode == bc::Opcode::Eq as u16)
    );
    for (name, returns) in [("in_short", 2), ("in_coalesce", 3)] {
        let f = function(p, name);
        assert_eq!(
            f.code
                .iter()
                .map(Result::unwrap)
                .filter(|i| i.opcode == bc::Opcode::Return as u16)
                .count(),
            returns
        );
    }
}

#[test]
fn forward_readonly_cells_and_mutual_recursion_keep_binding_identity() {
    let result = compiled(
        "fn outer() { fn first() { later } fn a() { b() } fn b() { a() } let later = null; first } \
         fn mutable() { let mut value=0; let next=|| { value += 1; value }; next } \
         fn initialized_null(){let mut value=null;value} \
         fn uninitialized_cell(){fn read(){value} read();let mut value=null;read}",
    );
    let p = result.program.package();
    let outer = function(p, "outer");
    for (name, initialized, cell) in [
        ("initialized_null", true, false),
        ("uninitialized_cell", false, true),
    ] {
        let f = function(p, name);
        let binding = f
            .bindings
            .iter()
            .position(|b| p.symbols[b.name.index()] == "value")
            .unwrap();
        assert_eq!(
            matches!(f.bindings[binding].storage, bc::BindingStorage::Cell(_)),
            cell
        );
        let init_pc = f
            .code
            .indexed_iter()
            .map(Result::unwrap)
            .find(|(_, i)| {
                i.opcode == bc::Opcode::BindInit as u16 && i.operands[0] == binding as u32
            })
            .map(|(pc, _)| pc)
            .unwrap();
        let first_read_pc = f
            .code
            .indexed_iter()
            .map(Result::unwrap)
            .find(|(_, i)| {
                if cell {
                    i.opcode == bc::Opcode::Call as u16
                } else {
                    i.opcode == bc::Opcode::BindRead as u16 && i.operands[1] == binding as u32
                }
            })
            .map(|(pc, _)| pc)
            .unwrap();
        assert_eq!(init_pc < first_read_pc, initialized);
        if cell {
            assert!(f.code.iter().map(Result::unwrap).any(|i| {
                i.opcode == bc::Opcode::CellNew as u16 && i.operands[0] == binding as u32
            }));
        } else {
            assert_eq!(f.kind, bc::FunctionKind::Plain);
            assert_eq!(
                f.bindings[binding].initialization,
                bc::BindingInitialization::Explicit
            );
        }
    }
    let later = outer
        .bindings
        .iter()
        .find(|b| p.symbols[b.name.index()] == "later")
        .unwrap();
    assert!(!later.mutable);
    assert!(matches!(later.storage, bc::BindingStorage::Cell(_)));
    let first = function(p, "first");
    assert_eq!(first.captures, [bc::CaptureKind::Cell]);
    let new_pc = outer
        .code
        .indexed_iter()
        .map(Result::unwrap)
        .find(|(_, i)| {
            i.opcode == bc::Opcode::CellNew as u16
                && outer.bindings[i.operands[0] as usize].name == later.name
        })
        .map(|(pc, _)| pc)
        .unwrap();
    let closure_pc = outer
        .code
        .indexed_iter()
        .map(Result::unwrap)
        .find(|(_, i)| i.opcode == bc::Opcode::MakeClosure as u16)
        .map(|(pc, _)| pc)
        .unwrap();
    let init_pc = outer
        .code
        .indexed_iter()
        .map(Result::unwrap)
        .find(|(_, i)| {
            i.opcode == bc::Opcode::BindInit as u16
                && outer.bindings[i.operands[0] as usize].name == later.name
        })
        .map(|(pc, _)| pc)
        .unwrap();
    assert!(new_pc < closure_pc && closure_pc < init_pc);
    assert!(p.constants.contains(&bc::Const::Null));
}

#[test]
fn try_catch_finally_uses_shared_region_contract_and_cleanup_queries() {
    let result = compiled(
        "fn f(x) { try { try { if x { return 1; } throw x; } finally { let y=2; } } catch error { try { throw; } finally { let z=3; } } finally { let w=4; } }",
    );
    let p = result.program.package();
    let id = bc::FunctionId(
        p.functions
            .iter()
            .position(|f| p.symbols[f.name.index()] == "f")
            .unwrap() as u32,
    );
    let f = &p.functions[id.index()];
    assert_eq!(f.regions.len(), 3);
    let pc = bc::Pc(
        f.code
            .indexed_iter()
            .map(Result::unwrap)
            .find(|(_, i)| i.opcode == bc::Opcode::Return as u16)
            .map(|(pc, _)| pc)
            .unwrap() as u32,
    );
    let first = result
        .program
        .cleanup_handler(id, pc, bc::CleanupDestination::Return, None)
        .unwrap()
        .unwrap();
    let second = result
        .program
        .cleanup_handler(id, pc, bc::CleanupDestination::Return, Some(first.region))
        .unwrap()
        .unwrap();
    assert_ne!(first.region, second.region);
    assert!(
        result
            .program
            .cleanup_handler(id, pc, bc::CleanupDestination::Return, Some(second.region))
            .unwrap()
            .is_none()
    );
    assert!(
        f.code
            .iter()
            .map(Result::unwrap)
            .any(|i| i.opcode == bc::Opcode::Rethrow as u16)
    );
}

#[test]
fn sequential_temporaries_are_reused_and_limits_fail_before_growth() {
    let mut source = String::from("fn f() {");
    for _ in 0..500 {
        source.push_str("1+2;");
    }
    source.push_str("3 }");
    let result = compiled(&source);
    assert!(function(result.program.package(), "f").register_count <= 4);
    let module = analyzed(&source);
    for limits in [
        lowering::Limits {
            max_work: 1,
            ..Default::default()
        },
        lowering::Limits {
            max_capacity_bytes: 64,
            ..Default::default()
        },
        lowering::Limits {
            max_instructions: 5,
            ..Default::default()
        },
        lowering::Limits {
            max_registers: 1,
            ..Default::default()
        },
        lowering::Limits {
            max_blocks: 0,
            ..Default::default()
        },
        lowering::Limits {
            max_records: 0,
            ..Default::default()
        },
        lowering::Limits {
            max_depth: 0,
            ..Default::default()
        },
    ] {
        let error = lowering::lower(
            &module,
            &Linkage::default(),
            Options {
                limits,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.kind, lowering::ErrorKind::Limit);
        assert!(error.stats.capacity_peak_bytes <= limits.max_capacity_bytes);
        assert!(error.stats.work_units <= limits.max_work);
        assert!(error.stats.records <= limits.max_records);
        assert!(error.stats.blocks <= limits.max_blocks);
        assert!(error.stats.instructions <= limits.max_instructions);
    }
}

#[test]
fn unsupported_nodes_report_original_range() {
    let source = "fn f() { let [a,b]=[1,2]; }";
    let module = analyzed(source);
    let error = lowering::lower(&module, &Linkage::default(), Options::default()).unwrap_err();
    assert_eq!(error.kind, lowering::ErrorKind::Unsupported);
    assert_eq!(&source[aside_compiler::byte_range(error.span)], "[a,b]");
}

#[test]
fn invalid_default_control_and_shared_emission_limits_are_located() {
    let source = "fn f(x=if true{return 1;}else{2}){x}";
    let analysis = analyze_module(source, &AnalysisOptions::default(), &mut NoModules).unwrap();
    let error = analysis
        .diagnostics
        .iter()
        .find(|d| d.code == aside_compiler::DiagnosticCode::InvalidDefault)
        .unwrap();
    assert_eq!(
        &source[aside_compiler::byte_range(error.range)],
        "return 1;"
    );
    assert!(analysis.into_module().is_err());

    let source = "fn f(){1}";
    let module = analyzed(source);
    let error = lowering::compile(
        &module,
        &Linkage::default(),
        &[],
        Options {
            bytecode_limits: bc::Limits {
                max_records: 1,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.kind, lowering::ErrorKind::Bytecode);
    assert_eq!(&source[aside_compiler::byte_range(error.span)], source);
}

#[test]
fn default_block_tails_nested_returns_loops_and_throw_still_compile() {
    let result = compiled(
        "fn block(x=if true{let local=2;local}else{3}){x} \
         fn closure(x=||{return 1;}){x()} \
         fn named(x=if true{fn nested(){return 1;}nested()}else{2}){x} \
         fn internal_loop(x=if true{loop{break;}2}else{3}){x} \
         fn condition_loop(x=if true{while if true{break;}else{true}{}2}else{3}){x} \
         fn throwing(x=if true{throw 1;}else{2}){x}",
    );
    let p = result.program.package();
    for name in [
        "block",
        "closure",
        "named",
        "internal_loop",
        "condition_loop",
        "throwing",
    ] {
        let f = function(p, name);
        let default = function_default(&p.signatures[f.signature.index()].parameters[0]);
        let thunk = &p.functions[default.index()];
        assert_eq!(thunk.context, bc::Context::NoWait);
        assert!(thunk.code.iter().map(Result::unwrap).any(|i| i.opcode
            == if name == "throwing" {
                bc::Opcode::Throw
            } else {
                bc::Opcode::Return
            } as u16));
    }
    assert!(
        function(p, "nested")
            .code
            .iter()
            .map(Result::unwrap)
            .any(|i| i.opcode == bc::Opcode::Return as u16)
    );
}

#[test]
fn source_entry_context_and_fragment_identity_survive_lowering() {
    use aside_compiler::{AnalysisContext, EntryPoint, ParseOptions, analyze_source};
    for context in [
        AnalysisContext::Function,
        AnalysisContext::Generator,
        AnalysisContext::NoWait,
        AnalysisContext::Module,
    ] {
        let options = AnalysisOptions {
            context,
            module: "game::scene".into(),
            version: "revision-7".into(),
            ..Default::default()
        };
        let source = match context {
            AnalysisContext::Function => "let x=7; return x;",
            AnalysisContext::Generator => "let x=7; yield x; return x;",
            _ => "7;",
        };
        let analysis = analyze_source(
            source,
            ParseOptions {
                entry: EntryPoint::Block,
                ..Default::default()
            },
            &options,
            &mut NoModules,
        )
        .unwrap();
        assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
        let module = analysis.into_module().unwrap();
        let result =
            lowering::compile(&module, &Linkage::default(), &[], Options::default()).unwrap();
        assert_eq!(result.metadata.entry_context, context);
        assert_eq!(result.metadata.canonical_path, "game::scene");
        assert_eq!(result.metadata.source_version, "revision-7");
        let entry = &result.program.package().functions[result.entry.index()];
        assert_eq!(
            entry.context,
            match context {
                AnalysisContext::Function => bc::Context::Ordinary,
                AnalysisContext::NoWait | AnalysisContext::Generator => bc::Context::NoWait,
                _ => bc::Context::Init,
            }
        );
        assert_eq!(
            result.program.package().modules[entry.module.index()]
                .initializer
                .is_some(),
            context == AnalysisContext::Module
        );
        if matches!(
            context,
            AnalysisContext::Function | AnalysisContext::Generator
        ) {
            assert!(!entry.bindings.is_empty());
            assert_eq!(
                entry.kind,
                if context == AnalysisContext::Generator {
                    bc::FunctionKind::Generator
                } else {
                    bc::FunctionKind::Plain
                }
            );
        } else {
            // Module fragments use initializer locals; NoWait alone supplies
            // no author frame/environment and still rejects declarations.
            let source = "let x=7;x;";
            let module = analyze_source(
                source,
                ParseOptions {
                    entry: EntryPoint::Block,
                    ..Default::default()
                },
                &options,
                &mut NoModules,
            )
            .unwrap()
            .into_module()
            .unwrap();
            if context == AnalysisContext::Module {
                let result =
                    lowering::compile(&module, &Linkage::default(), &[], Options::default())
                        .unwrap();
                assert!(result.program.package().modules[0].globals.is_empty());
                assert_eq!(
                    result.program.package().functions[result.entry.index()]
                        .bindings
                        .len(),
                    1
                );
                assert_eq!(module.symbols()[0].origin, hir::BindingOrigin::EntryLocal);
            } else {
                let error =
                    lowering::compile(&module, &Linkage::default(), &[], Options::default())
                        .unwrap_err();
                assert_eq!(error.kind, lowering::ErrorKind::Unsupported);
                assert_eq!(&source[aside_compiler::byte_range(error.span)], "x");
                assert_eq!(error.stats.capacity_bytes, 0);
            }
        }
    }
}

#[test]
fn inferred_function_types_do_not_become_structural_author_contracts() {
    let result = compiled("fn f() { let mut next=||1; next=||2; next() } fn g() { f() }");
    let p = result.program.package();
    assert!(
        !p.types
            .iter()
            .any(|ty| matches!(ty, bc::TypeDef::Function { .. }))
    );
    let f = function(p, "f");
    let next = f
        .bindings
        .iter()
        .find(|b| p.symbols[b.name.index()] == "next")
        .unwrap();
    assert_eq!(
        p.types[next.ty.index()],
        bc::TypeDef::Builtin(bc::BuiltinType::Any)
    );
}

#[test]
fn parameter_cell_defaults_use_body_prefix_and_definition_captures() {
    let module = analyzed(
        "fn outer() { let mut captured=1; fn f(mut x=2, y=if true { x+=captured; || { x+=1; x } } else { ||x }) { y(); x } f }",
    );
    let ir = lowering::lower(&module, &Linkage::default(), Options::default()).unwrap();
    let p = ir.tables();
    let f = function(p, "f");
    assert_eq!(f.bindings[0].storage, bc::BindingStorage::Cell(bc::Reg(0)));
    let s = &p.signatures[f.signature.index()];
    let default = &p.functions[function_default(&s.parameters[1]).index()];
    assert_eq!(default.bindings[0], f.bindings[0]);
    assert_eq!(default.captures, f.captures);
    assert_eq!(default.module, f.module);
    let body_identity = ir
        .metadata()
        .bindings
        .iter()
        .find(|i| {
            i.function
                == p.functions
                    .iter()
                    .position(|a| std::ptr::eq(a, f))
                    .map(|i| bc::FunctionId(i as u32))
                    .unwrap()
                && i.binding == bc::BindingId(0)
        })
        .unwrap();
    let default_identity = ir
        .metadata()
        .bindings
        .iter()
        .find(|i| i.function == function_default(&s.parameters[1]) && i.binding == bc::BindingId(0))
        .unwrap();
    assert_eq!(body_identity.source, default_identity.source);
    assert_eq!(
        ir.metadata()
            .symbol_reference(body_identity.source)
            .unwrap(),
        module.symbol_reference(body_identity.source).unwrap()
    );
    assert!(ir.functions().iter().any(|f| matches!(
        f.origin,
        lowering::ir::Origin::Default { parameter: 1, .. }
    ) && f.output == function_default(&s.parameters[1])));
    let result = ir.emit(&[]).unwrap();
    let encoded = bc::encode(result.program.package(), bc::Limits::default()).unwrap();
    let decoded = bc::decode(&encoded, bc::Limits::default()).unwrap();
    bc::verify(decoded, bc::Limits::default()).unwrap();
    let f = function(result.program.package(), "f");
    assert!(
        !f.code
            .iter()
            .map(Result::unwrap)
            .any(|i| i.opcode == bc::Opcode::CellNew as u16 && i.operands[0] == 0)
    );
}

fn native_seed() -> bc::VerifiedProgram {
    let p = bc::BytecodePackage {
        symbols: vec![
            "registered.native".into(),
            "a".into(),
            "io.test".into(),
            "native_module".into(),
            "native-default".into(),
        ],
        types: vec![bc::TypeDef::Builtin(bc::BuiltinType::Int)],
        constants: vec![bc::Const::Int(17)],
        signatures: vec![
            bc::Signature {
                parameters: vec![],
                rest: None,
                return_type: bc::TypeId(0),
                effects: bc::Effects(0),
                property_bag_tail: None,
            },
            bc::Signature {
                parameters: vec![bc::Parameter {
                    name: bc::SymbolId(1),
                    ty: bc::TypeId(0),
                    mutable: false,
                    default: Some(bc::ParameterDefault::Function(bc::FunctionId(0))),
                }],
                rest: None,
                return_type: bc::TypeId(0),
                effects: bc::Effects(bc::Effects::HOST),
                property_bag_tail: None,
            },
        ],
        natives: vec![bc::NativeDef {
            symbol: bc::SymbolId(0),
            signature: bc::SignatureId(1),
            abi_version: 1,
            capabilities: vec![bc::SymbolId(2)],
        }],
        capabilities: vec![bc::SymbolId(2)],
        modules: vec![bc::ModuleDef {
            path: bc::SymbolId(3),
            imports: vec![],
            exports: vec![],
            globals: vec![],
            initializer: None,
        }],
        functions: vec![bc::FunctionDef {
            module: bc::ModuleId(0),
            name: bc::SymbolId(4),
            kind: bc::FunctionKind::Plain,
            signature: bc::SignatureId(0),
            context: bc::Context::NoWait,
            captures: vec![],
            register_count: 1,
            bindings: vec![],
            descriptors: vec![],
            code: vec![
                bc::Instruction::new(bc::Opcode::LoadConst, 0, 0, 0),
                bc::Instruction::new(bc::Opcode::Return, 0, 0, 0),
            ],
            regions: vec![],
            source_map: vec![],
        }],
        ..Default::default()
    };
    bc::verify(p, bc::Limits::default()).unwrap()
}
#[test]
fn direct_native_uses_trusted_identity_abi_capabilities_and_real_default() {
    use aside_compiler::semantic::{
        CallEffect, ExternalParameter, ExternalSignature, ExternalSymbol, SymbolReference, TypeHint,
    };
    let reference = SymbolReference {
        module: "registry::test".into(),
        version: "native-v19".into(),
        export: Some("run".into()),
    };
    let host = ExternalSymbol::function(
        "host",
        ExternalSignature {
            result_parameter: None,
            result_cases: Vec::new(),
            parameters: vec![ExternalParameter {
                name: "a".into(),
                ty: TypeHint::Int,
                mutable: false,
                default: true,
                rest: false,
            }],
            result: TypeHint::Int,
            effect: CallEffect::NoWait,
            init_safe: Some(true),
            derived_safe: Some(false),
        },
    )
    .with_reference(reference.clone());
    let options = AnalysisOptions {
        hosts: vec![host],
        ..Default::default()
    };
    let source = "fn f() { host() + (host)(a: 3) }";
    let analysis = analyze_module(source, &options, &mut NoModules).unwrap();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let module = analysis.into_module().unwrap();
    let symbol = module
        .external_bindings()
        .iter()
        .find_map(|e| e.symbol)
        .unwrap();
    let seed = native_seed();
    let native_links = [lowering::NativeLink {
        source_signature: None,
        target: lowering::ExternalTarget::Symbol(symbol),
        native: bc::NativeId(0),
        reference: &reference,
    }];
    let linkage = Linkage {
        package: Some(&seed),
        natives: &native_links,
        expansions: &[],
    };
    let result = lowering::compile(&module, &linkage, &[], Options::default()).unwrap();
    let p = result.program.package();
    assert_eq!(p.natives[0], seed.package().natives[0]);
    assert_eq!(p.natives[0].abi_version, 1);
    assert_eq!(p.functions[0], seed.package().functions[0]);
    assert_eq!(
        p.signatures[1].parameters[0].default,
        Some(bc::ParameterDefault::Function(bc::FunctionId(0)))
    );
    let f = function(p, "f");
    let calls: Vec<_> = f
        .descriptors
        .iter()
        .filter_map(|d| match d {
            bc::Descriptor::Call(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|c| c.target == bc::CallTarget::Native(bc::NativeId(0)))
    );
    assert!(std::sync::Arc::ptr_eq(
        &module.external_bindings()[0].descriptor,
        &result.metadata.external_bindings[0].descriptor
    ));
    // A host's complete registration list can name both the imported/host
    // symbol and each resolved use. They must agree, not become ambiguous just
    // because the same NativeId is encountered more than once.
    let mut aliases = Vec::new();
    for external in module.external_bindings() {
        for target in external
            .symbol
            .map(lowering::ExternalTarget::Symbol)
            .into_iter()
            .chain(external.node.map(lowering::ExternalTarget::Node))
        {
            aliases.push(lowering::NativeLink {
                source_signature: None,
                target,
                ..native_links[0]
            });
        }
    }
    assert!(
        aliases
            .iter()
            .any(|l| matches!(l.target, lowering::ExternalTarget::Node(_)))
    );
    let aliased = lowering::compile(
        &module,
        &Linkage {
            package: Some(&seed),
            natives: &aliases,
            expansions: &[],
        },
        &[],
        Options::default(),
    )
    .unwrap();
    assert_eq!(aliased.program.package(), p);
    let mut conflicting = seed.to_unpacked().unwrap();
    let mut other = conflicting.natives[0].clone();
    other.symbol = bc::SymbolId(conflicting.symbols.len() as u32);
    conflicting.symbols.push("different::native".into());
    conflicting.natives.push(other);
    let conflicting = bc::verify(conflicting, bc::Limits::default()).unwrap();
    aliases.last_mut().unwrap().native = bc::NativeId(1);
    let conflict = lowering::compile(
        &module,
        &Linkage {
            package: Some(&conflicting),
            natives: &aliases,
            expansions: &[],
        },
        &[],
        Options::default(),
    )
    .unwrap_err();
    assert_eq!(conflict.kind, lowering::ErrorKind::LinkContract);
    assert!(conflict.detail.contains("ambiguous native binding"));
    let no_link =
        lowering::compile(&module, &Linkage::default(), &[], Options::default()).unwrap_err();
    assert_eq!(no_link.kind, lowering::ErrorKind::MissingLink);
    assert_eq!(&source[aside_compiler::byte_range(no_link.span)], "host");
    let changed = SymbolReference {
        version: "wrong-version".into(),
        ..reference.clone()
    };
    let wrong_links = [lowering::NativeLink {
        source_signature: None,
        reference: &changed,
        ..native_links[0]
    }];
    let wrong = lowering::compile(
        &module,
        &Linkage {
            package: Some(&seed),
            natives: &wrong_links,
            expansions: &[],
        },
        &[],
        Options::default(),
    )
    .unwrap_err();
    assert_eq!(wrong.kind, lowering::ErrorKind::LinkContract);
    let mut missing_default = seed.to_unpacked().unwrap();
    missing_default.signatures[1].parameters[0].default = None;
    let missing_default = bc::verify(missing_default, bc::Limits::default()).unwrap();
    let mismatch = lowering::compile(
        &module,
        &Linkage {
            package: Some(&missing_default),
            natives: &native_links,
            expansions: &[],
        },
        &[],
        Options::default(),
    )
    .unwrap_err();
    assert_eq!(mismatch.kind, lowering::ErrorKind::LinkContract);

    let value_analysis = analyze_module(
        "fn f() { let value=host; value() }",
        &options,
        &mut NoModules,
    )
    .unwrap()
    .into_module()
    .unwrap();
    let value_symbol = value_analysis
        .external_bindings()
        .iter()
        .find_map(|e| e.symbol)
        .unwrap();
    let value_links = [lowering::NativeLink {
        source_signature: None,
        target: lowering::ExternalTarget::Symbol(value_symbol),
        ..native_links[0]
    }];
    let value = lowering::compile(
        &value_analysis,
        &Linkage {
            package: Some(&seed),
            natives: &value_links,
            expansions: &[],
        },
        &[],
        Options::default(),
    )
    .unwrap();
    assert!(
        value
            .program
            .package()
            .functions
            .iter()
            .any(|f| f.kind == bc::FunctionKind::Native(bc::NativeId(0))
                && f.name == value.program.package().natives[0].symbol)
    );
}

#[test]
fn cleanup_aware_loop_edges_stay_in_their_lexical_regions() {
    let result = compiled(
        "fn f(x) { outer: while x { try { if x { break outer; } continue outer; } finally { loop { break; } } } }",
    );
    let f = function(result.program.package(), "f");
    assert!(
        f.code
            .iter()
            .map(Result::unwrap)
            .filter(|i| i.opcode == bc::Opcode::Leave as u16)
            .count()
            >= 2
    );
}

#[test]
fn while_condition_targets_cover_labels_terminal_paths_and_cleanup() {
    let source = "fn choose(x)->bool{true} fn marker(){9} \
        fn condition_break(flag){outer:while if flag{break outer;}else{true}{} 7} \
        fn condition_continue(flag){outer:while if flag{continue outer;}else{true}{break outer;} 7} \
        fn anonymous(flag){while if flag{break;}else{true}{break;} 7} \
        fn terminal(flag){outer:while choose(if flag{break outer;}else{continue outer;}){marker();} 7} \
        fn terminal_return(flag){while choose(if flag{return 1;}else{return 2;}){marker();} marker()} \
        fn outer_transfer(flag){outer:while true{while choose(if flag{break outer;}else{continue outer;}){marker();} marker();} 7} \
        fn cleanup(flag){outer:while if flag{try{break outer;}finally{1;} true}else{true}{break outer;} 7} \
        fn forever(){loop{continue;} marker()}";
    let result = compiled(source);
    let p = result.program.package();
    for name in ["terminal", "terminal_return", "outer_transfer", "forever"] {
        let f = function(p, name);
        assert!(
            !f.code
                .iter()
                .map(Result::unwrap)
                .any(|i| i.opcode == bc::Opcode::Call as u16),
            "unreachable call in {name}"
        );
    }
    let f = function(p, "condition_continue");
    let head = f.code.instruction(0).unwrap().unwrap().operands[0];
    let jump = f
        .code
        .iter()
        .map(Result::unwrap)
        .zip(&f.source_map)
        .find(|(_, map)| {
            &source[map.span.start as usize..map.span.end as usize] == "continue outer;"
        })
        .unwrap()
        .0;
    assert_eq!(jump.opcode, bc::Opcode::Jump as u16);
    assert_eq!(jump.operands[0], head);
    for name in ["condition_break", "terminal"] {
        let f = function(p, name);
        let jump = f
            .code
            .iter()
            .map(Result::unwrap)
            .zip(&f.source_map)
            .find(|(_, map)| {
                &source[map.span.start as usize..map.span.end as usize] == "break outer;"
            })
            .unwrap()
            .0;
        let next = f
            .code
            .instruction(jump.operands[0] as usize)
            .unwrap()
            .unwrap();
        assert_eq!(next.opcode, bc::Opcode::LoadConst as u16);
        assert_eq!(p.constants[next.operands[1] as usize], bc::Const::Int(7));
    }
    let cleanup = function(p, "cleanup");
    assert!(
        cleanup
            .code
            .iter()
            .map(Result::unwrap)
            .any(|i| i.opcode == bc::Opcode::Leave as u16)
    );
    assert_eq!(cleanup.regions.len(), 1);
    let id = bc::FunctionId(
        p.functions
            .iter()
            .position(|f| p.symbols[f.name.index()] == "cleanup")
            .unwrap() as u32,
    );
    let (pc, leave) = cleanup
        .code
        .indexed_iter()
        .map(Result::unwrap)
        .find(|(_, i)| i.opcode == bc::Opcode::Leave as u16)
        .unwrap();
    let handler = result
        .program
        .cleanup_handler(
            id,
            bc::Pc(pc as u32),
            bc::CleanupDestination::Leave(bc::Pc(leave.operands[0])),
            None,
        )
        .unwrap()
        .unwrap();
    assert_eq!(handler.kind, bc::HandlerKind::Finally);
    assert_eq!(
        handler.pc,
        cleanup.regions[handler.region.index()]
            .finally
            .unwrap()
            .start
    );
    assert!(
        !function(p, "forever")
            .code
            .iter()
            .map(Result::unwrap)
            .any(|i| i.opcode == bc::Opcode::Return as u16)
    );
}

#[test]
fn module_nested_bindings_roundtrip_with_initializer_identity() {
    for (source, name, globals) in [
        ("let mut i=0;while i<2{let x=i;i+=1;}", "x", 1),
        ("let x=1;if true{let x=2;x;}", "x", 1),
        ("if true{fn hidden(){1} hidden();}", "hidden", 0),
        ("try{throw 1;}catch error{null;}", "error", 0),
        (
            "outer:while if true{let x=true;x}else{false}{break outer;}",
            "x",
            0,
        ),
    ] {
        let module = analyzed(source);
        let result = compiled(source);
        let p = result.program.package();
        assert_eq!(p.modules[0].globals.len(), globals);
        let (index, _) = module
            .symbols()
            .iter()
            .enumerate()
            .find(|(_, s)| {
                module.hir().name(s.name) == name && s.origin == hir::BindingOrigin::EntryLocal
            })
            .unwrap();
        let identity = result
            .metadata
            .bindings
            .iter()
            .find(|b| b.source == hir::SymbolId(index as u32))
            .unwrap();
        assert_eq!(identity.function, result.entry);
        assert!(
            !result
                .metadata
                .globals
                .iter()
                .any(|g| g.source == identity.source)
        );
        assert!(
            p.functions[result.entry.index()]
                .code
                .iter()
                .map(Result::unwrap)
                .any(|i| {
                    i.opcode == bc::Opcode::BindInit as u16 && i.operands[0] == identity.binding.0
                })
        );
        initializer_locals::assert_local_structure(&module, &result);
    }
    let result = compiled("let mut i=0;while i<2{i+=1;}");
    let p = result.program.package();
    assert_eq!(p.modules[0].globals.len(), 1);
    let initializer = &p.functions[result.entry.index()];
    assert_eq!(
        initializer
            .code
            .iter()
            .map(Result::unwrap)
            .filter(|i| i.opcode == bc::Opcode::GlobalInit as u16)
            .count(),
        1
    );
    compiled("fn f(){let x=1;while false{let x=2;x;}x}");
}

#[test]
fn imported_script_bindings_fail_instead_of_skipping_module_initialization() {
    use aside_compiler::semantic::{ExternalSymbol, MemoryResolver, ModuleInterface, TypeHint};
    let mut resolver = MemoryResolver::default();
    resolver.insert(ModuleInterface {
        canonical_path: "game::dependency".into(),
        version: "revision-2".into(),
        exports: vec![ExternalSymbol::value("value", TypeHint::Int)],
    });
    let source = "use game::dependency::{value}; fn f(){1}";
    let m = analyze_module(source, &AnalysisOptions::default(), &mut resolver)
        .unwrap()
        .into_module()
        .unwrap();
    let error = lowering::compile(&m, &Linkage::default(), &[], Options::default()).unwrap_err();
    assert_eq!(error.kind, lowering::ErrorKind::MissingLink);
    assert_eq!(
        &source[aside_compiler::byte_range(error.span)],
        "use game::dependency::{value};"
    );
}

#[test]
fn mutable_prefix_write_without_a_closure_and_readonly_captured_prefix_roundtrip() {
    let result = compiled(
        "fn mutate(mut x=1,y=if true {x+=1;x} else {0}) { x } fn readonly(x=1,y=||x) { y() }",
    );
    let f = function(result.program.package(), "mutate");
    assert_eq!(f.bindings[0].storage, bc::BindingStorage::Cell(bc::Reg(0)));
    let f = function(result.program.package(), "readonly");
    let p = &result.program.package().signatures[f.signature.index()].parameters;
    let default = &result.program.package().functions[function_default(&p[1]).index()];
    assert_eq!(default.bindings[0].storage, f.bindings[0].storage);
    assert!(matches!(
        f.bindings[0].storage,
        bc::BindingStorage::Value(bc::Reg(0))
    ));
}

#[test]
fn native_default_prefix_cells_and_fast_value_slots_come_from_shared_verifier() {
    use aside_compiler::semantic::{
        CallEffect, ExternalParameter, ExternalSignature, ExternalSymbol, SymbolReference, TypeHint,
    };
    let reference = SymbolReference {
        module: "registry::host".into(),
        version: "v1".into(),
        export: Some("parameter_defaults".into()),
    };
    let parameter = |name: &str, mutable, default, rest| ExternalParameter {
        name: name.into(),
        ty: if rest { TypeHint::Any } else { TypeHint::Int },
        mutable,
        default,
        rest,
    };
    let host = ExternalSymbol::function(
        "host",
        ExternalSignature {
            result_parameter: None,
            result_cases: Vec::new(),
            parameters: vec![
                parameter("x", true, false, false),
                parameter("y", false, true, false),
                parameter("z", true, true, false),
                parameter("rest", true, false, true),
            ],
            result: TypeHint::Any,
            effect: CallEffect::MayWait,
            init_safe: Some(true),
            derived_safe: Some(false),
        },
    )
    .with_reference(reference.clone());
    let analysis = analyze_module(
        "fn f(){host(41)}",
        &AnalysisOptions {
            hosts: vec![host],
            ..Default::default()
        },
        &mut NoModules,
    )
    .unwrap()
    .into_module()
    .unwrap();
    let symbol = analysis
        .external_bindings()
        .iter()
        .find_map(|e| e.symbol)
        .unwrap();
    let seed = bc::verify(
        bc::fixtures::native_parameter_defaults(),
        bc::Limits::default(),
    )
    .unwrap();
    let links = [lowering::NativeLink {
        source_signature: None,
        target: lowering::ExternalTarget::Symbol(symbol),
        native: bc::NativeId(0),
        reference: &reference,
    }];
    let result = lowering::compile(
        &analysis,
        &Linkage {
            package: Some(&seed),
            natives: &links,
            expansions: &[],
        },
        &[],
        Options::default(),
    )
    .unwrap();
    assert!(
        result
            .program
            .native_parameters_need_cells(bc::NativeId(0))
            .unwrap()
    );
    for i in 0..4 {
        assert_eq!(
            result
                .program
                .native_parameter_storage(bc::NativeId(0), i)
                .unwrap(),
            seed.native_parameter_storage(bc::NativeId(0), i).unwrap()
        );
    }
    assert_eq!(
        result
            .program
            .native_parameter_storage(bc::NativeId(0), 0)
            .unwrap(),
        bc::BindingStorage::Cell(bc::Reg(0))
    );
    assert_eq!(
        result
            .program
            .native_parameter_storage(bc::NativeId(0), 1)
            .unwrap(),
        bc::BindingStorage::Cell(bc::Reg(1))
    );
    assert_eq!(
        result
            .program
            .native_parameter_storage(bc::NativeId(0), 2)
            .unwrap(),
        bc::BindingStorage::Value(bc::Reg(2))
    );
    assert_eq!(
        result
            .program
            .native_parameter_storage(bc::NativeId(0), 3)
            .unwrap(),
        bc::BindingStorage::Value(bc::Reg(3))
    );
    assert!(result.program.stats().derived_capacity_bytes > 0);
    assert_eq!(result.metadata.natives[0].reference.as_ref(), &reference);
    let bytes = bc::encode(result.program.package(), bc::Limits::default()).unwrap();
    let decoded = bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap();
    assert_eq!(
        decoded
            .native_parameter_storage(bc::NativeId(0), 0)
            .unwrap(),
        bc::BindingStorage::Cell(bc::Reg(0))
    );
}

#[test]
fn qualified_native_import_retains_namespace_and_registration_identity() {
    use aside_compiler::semantic::{
        CallEffect, ExternalParameter, ExternalSignature, ExternalSymbol, MemoryResolver,
        ModuleInterface, SymbolReference, TypeHint,
    };
    let reference = SymbolReference {
        module: "registry::test".into(),
        version: "native-v19".into(),
        export: Some("run".into()),
    };
    let native = ExternalSymbol::function(
        "run",
        ExternalSignature {
            result_parameter: None,
            result_cases: Vec::new(),
            parameters: vec![ExternalParameter {
                name: "a".into(),
                ty: TypeHint::Int,
                mutable: false,
                default: true,
                rest: false,
            }],
            result: TypeHint::Int,
            effect: CallEffect::NoWait,
            init_safe: Some(true),
            derived_safe: Some(false),
        },
    )
    .with_reference(reference.clone());
    let mut resolver = MemoryResolver::default();
    resolver.insert(ModuleInterface {
        canonical_path: "reg::exports".into(),
        version: "metadata-2".into(),
        exports: vec![native],
    });
    let source = "use reg::exports as native; fn f(){native.run()}";
    let analysis = analyze_module(source, &AnalysisOptions::default(), &mut resolver).unwrap();
    assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
    let module = analysis.into_module().unwrap();
    let node = module
        .external_bindings()
        .iter()
        .find_map(|e| {
            e.node
                .filter(|&node| module.hir().node(node).kind == hir::HirKind::Member)
        })
        .unwrap();
    let links = [lowering::NativeLink {
        source_signature: None,
        target: lowering::ExternalTarget::Node(node),
        native: bc::NativeId(0),
        reference: &reference,
    }];
    let seed = native_seed();
    let result = lowering::compile(
        &module,
        &Linkage {
            package: Some(&seed),
            natives: &links,
            expansions: &[],
        },
        &[],
        Options::default(),
    )
    .unwrap();
    let namespace = result
        .metadata
        .external_bindings
        .iter()
        .find(|e| e.descriptor.namespace.is_some())
        .unwrap();
    assert!(
        namespace
            .descriptor
            .reference
            .as_ref()
            .unwrap()
            .export
            .is_none()
    );
    assert_eq!(result.metadata.imports[0].module, "reg::exports");
    assert_eq!(result.metadata.imports[0].version, "metadata-2");
    assert_eq!(result.metadata.natives[0].reference.as_ref(), &reference);
    let f = function(result.program.package(), "f");
    assert!(f.descriptors.iter().any(
        |d| matches!(d,bc::Descriptor::Call(c) if c.target==bc::CallTarget::Native(bc::NativeId(0)))
    ));
}

#[test]
fn source_and_constants_use_the_package_budget() {
    let source = format!("fn f(){{\"{}\"}}", "x".repeat(1024 * 1024 + 1));
    let module = analyzed(&source);
    let limits = bc::Limits::default();
    let compiled = lowering::compile(
        &module,
        &Linkage::default(),
        &[],
        Options {
            bytecode_limits: limits,
            ..Default::default()
        },
    )
    .unwrap();
    let bytes = bc::encode(compiled.program.package(), limits).unwrap();
    let decoded = bc::VerifiedProgram::decode(&bytes, limits).unwrap();
    assert_eq!(decoded.package().sources[0].text, source);
    let extension = analyze_module(
        "fn extend(){null}",
        &AnalysisOptions {
            module: "extension".into(),
            ..Default::default()
        },
        &mut NoModules,
    )
    .unwrap()
    .into_module()
    .unwrap();
    let links = Linkage {
        package: Some(&compiled.program),
        ..Default::default()
    };
    let cost = compiled.program.editable_clone_cost().unwrap();
    assert!(cost.scratch_peak_bytes > 0);
    for limits in [
        lowering::Limits {
            max_capacity_bytes: cost.retained_capacity_bytes + cost.scratch_peak_bytes - 1,
            ..Default::default()
        },
        lowering::Limits {
            max_work: cost.work_units - 1,
            ..Default::default()
        },
    ] {
        let error = lowering::lower(
            &extension,
            &links,
            Options {
                limits,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.kind, lowering::ErrorKind::Limit);
        assert_eq!(
            error.stats.capacity_bytes, 0,
            "the editable clone was allocated before admission"
        );
    }
}

/// Definite initialization of temporary registers over the actual shared CFG.
/// Fixed binding slots have independent runtime initialization checks, so an
/// intentional forward BindRead remains valid compiler output.
fn assert_temporary_initialization(f: &bc::RuntimeFunction) {
    let mut incoming: Vec<Option<Vec<bool>>> = vec![None; f.code.end_pc()];
    let entry = vec![false; f.register_count as usize];
    incoming[0] = Some(entry.clone());
    let mut changed = true;
    while changed {
        changed = false;
        for (pc, i) in f.code.indexed_iter().map(Result::unwrap) {
            let Some(mut state) = incoming[pc].clone() else {
                continue;
            };
            let spec = bc::instruction_spec(i.opcode).unwrap();
            // Cleanup inherits registers at the exiting instruction. In
            // particular, an owned for iterator was initialized before entering
            // its region and remains live in the synthetic finally.
            let mut handlers = Vec::new();
            for region in &f.regions {
                let position = bc::Pc(pc as u32);
                if region.protected.contains(position) {
                    if spec.may_throw {
                        if let Some(catch) = &region.catch {
                            let mut caught = state.clone();
                            caught[catch.exception.index()] = true;
                            handlers.push((catch.range.start.index(), caught));
                        } else if let Some(finally) = region.finally {
                            handlers.push((finally.start.index(), state.clone()));
                        }
                    }
                    if matches!(spec.flow, bc::Flow::Return | bc::Flow::Leave)
                        && let Some(finally) = region.finally
                    {
                        handlers.push((finally.start.index(), state.clone()));
                    }
                } else if region
                    .catch
                    .as_ref()
                    .is_some_and(|catch| catch.range.contains(position))
                    && (spec.may_throw || matches!(spec.flow, bc::Flow::Return | bc::Flow::Leave))
                    && let Some(finally) = region.finally
                {
                    handlers.push((finally.start.index(), state.clone()));
                }
            }
            spec.visit_registers(f, &i, |reg, access| {
                if matches!(access, bc::Access::Write | bc::Access::ReadWrite) {
                    state[reg.index()] = !matches!(
                        spec.opcode,
                        bc::Opcode::Clear | bc::Opcode::Clear2 | bc::Opcode::Clear3
                    );
                }
            })
            .unwrap();
            let targets: Vec<usize> = match spec.flow {
                bc::Flow::Next => vec![f.code.next_pc(pc).unwrap().unwrap()],
                bc::Flow::Jump | bc::Flow::Leave => vec![i.operands[0] as usize],
                bc::Flow::Branch => vec![i.operands[1] as usize, i.operands[2] as usize],
                _ => vec![],
            };
            for (target, state) in targets
                .into_iter()
                .map(|target| (target, state.clone()))
                .chain(handlers)
            {
                let next = match &incoming[target] {
                    Some(old) => old.iter().zip(&state).map(|(a, b)| *a && *b).collect(),
                    None => state.clone(),
                };
                if incoming[target].as_ref() != Some(&next) {
                    incoming[target] = Some(next);
                    changed = true;
                }
            }
        }
    }
    for (pc, state) in incoming.iter().enumerate() {
        let Some(state) = state else {
            continue;
        };
        let instruction = f.code.instruction(pc).unwrap().unwrap();
        let spec = bc::instruction_spec(instruction.opcode).unwrap();
        spec.visit_registers(f, &instruction, |reg, access| {
            if reg.index() >= f.bindings.len()
                && matches!(access, bc::Access::Read | bc::Access::ReadWrite)
            {
                assert!(
                    state[reg.index()],
                    "temporary r{} not initialized at PC {} {:?}",
                    reg.0,
                    pc,
                    spec.opcode
                );
            }
        })
        .unwrap();
    }
}

fn function_default(parameter: &bc::Parameter) -> bc::FunctionId {
    let Some(bc::ParameterDefault::Function(function)) = parameter.default else {
        panic!("expected dependent default function")
    };
    function
}
