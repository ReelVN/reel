//! Independent source/IR/B01 structure checks. Never choose branches by values.
use super::collections_locations::{assert_cleared, assert_live, calls, pcs, text};
use super::initializer_locals::dominators;
use super::*;
use aside_compiler::byte_range;

fn descriptor(f: &bc::FunctionDef, pc: usize) -> &bc::CallDesc {
    let bc::Descriptor::Call(c) = &f.descriptors[f.code[pc].operands[1] as usize] else {
        panic!("call descriptor")
    };
    c
}
fn reachable(f: &bc::FunctionDef, start: usize) -> Vec<bool> {
    let mut seen = vec![false; f.code.len()];
    let mut todo = vec![start];
    while let Some(pc) = todo.pop() {
        if std::mem::replace(&mut seen[pc], true) {
            continue;
        }
        match bc::instruction_spec(f.code[pc].opcode).unwrap().flow {
            bc::Flow::Next => todo.push(pc + 1),
            bc::Flow::Jump | bc::Flow::Leave => todo.push(f.code[pc].operands[0] as usize),
            bc::Flow::Branch => todo.extend([
                f.code[pc].operands[1] as usize,
                f.code[pc].operands[2] as usize,
            ]),
            _ => (),
        }
    }
    seen
}

#[test]
fn packed_arguments_capture_in_source_order_and_keep_callee_builder_live() {
    let c = compiled(
        "fn touch(x){x}fn f(g,a,b){g(touch(1),...touch(a),touch(2),...touch(b),中文:touch(3))}",
    );
    let p = c.program.package();
    let f = function(p, "f");
    assert_eq!(
        calls(p, f),
        [
            "touch(1)",
            "touch(a)",
            "touch(2)",
            "touch(b)",
            "touch(3)",
            "g(touch(1),...touch(a),touch(2),...touch(b),中文:touch(3))"
        ]
    );
    let new = pcs(f, bc::Opcode::ArgsNew);
    assert_eq!(new.len(), 1);
    let builder = f.code[new[0]].operands[0];
    let cp = pcs(f, bc::Opcode::Call);
    let final_pc = *cp.last().unwrap();
    let desc = descriptor(f, final_pc);
    assert_eq!(desc.arguments, bc::Arguments::Packed(bc::Reg(builder)));
    assert!(desc.names.is_empty());
    let bc::CallTarget::Register(callee) = desc.target else {
        panic!("dynamic callee")
    };
    assert_live(f, callee.0, new[0] - 1, final_pc);
    assert_live(f, builder, new[0], final_pc);
    assert_cleared(f, builder, final_pc);
    let append: Vec<_> = f
        .code
        .iter()
        .enumerate()
        .filter(|(_, i)| {
            [
                bc::Opcode::ArgsPush as u16,
                bc::Opcode::ArgsSpread as u16,
                bc::Opcode::ArgsNamed as u16,
            ]
            .contains(&i.opcode)
        })
        .collect();
    assert_eq!(
        append.iter().map(|(_, i)| i.opcode).collect::<Vec<_>>(),
        [
            bc::Opcode::ArgsPush,
            bc::Opcode::ArgsSpread,
            bc::Opcode::ArgsPush,
            bc::Opcode::ArgsSpread,
            bc::Opcode::ArgsNamed
        ]
        .map(|o| o as u16)
    );
    for (index, (pc, i)) in append.iter().enumerate() {
        assert_eq!(i.operands[0], builder);
        assert_eq!(i.operands[1], f.code[cp[index]].operands[0]);
        assert!(cp[index] < *pc && *pc < cp[index + 1]);
    }
    assert_eq!(p.symbols[append[4].1.operands[2] as usize], "中文");
}

#[test]
fn source_mutation_follows_capture_and_no_spread_keeps_window() {
    let c = compiled(
        "fn change(xs){xs[0]=3;7}fn f(g,xs){g(...xs,change(xs))}fn take(...r){r}fn ordinary(){take(1,2)}",
    );
    let p = c.program.package();
    let f = function(p, "f");
    let cp = pcs(f, bc::Opcode::Call);
    let spread = pcs(f, bc::Opcode::ArgsSpread)[0];
    assert!(spread < cp[0]);
    assert_eq!(text(p, f, spread), "...xs");
    assert_eq!(calls(p, f), ["change(xs)", "g(...xs,change(xs))"]);
    let ordinary = function(p, "ordinary");
    assert!(pcs(ordinary, bc::Opcode::ArgsNew).is_empty());
    assert!(matches!(
        descriptor(ordinary, pcs(ordinary, bc::Opcode::Call)[0]).arguments,
        bc::Arguments::Window(bc::RegWindow { len: 2, .. })
    ));
    let take = function(p, "take");
    let sig = &p.signatures[take.signature.index()];
    assert!(sig.parameters.is_empty() && sig.rest.is_some());
}

#[test]
fn rest_cells_default_prefixes_and_definition_captures_are_exact() {
    let c = compiled(
        "fn outer(){let mut x=[];fn take(mut a:int=0,b=||{a+=1;x},...mut r:array){let c=||r;(b,c)}take(...x)}fn readonly(...r:array){||r}",
    );
    let p = c.program.package();
    let take = function(p, "take");
    let sig = &p.signatures[take.signature.index()];
    assert_eq!(sig.parameters.len(), 2);
    let rest = sig.rest.as_ref().unwrap();
    assert_eq!(p.symbols[rest.name.index()], "r");
    assert!(rest.mutable && rest.default.is_none());
    assert_eq!(
        p.types[rest.ty.index()],
        bc::TypeDef::Builtin(bc::BuiltinType::Array)
    );
    let binding = take
        .bindings
        .iter()
        .find(|b| b.initialization == bc::BindingInitialization::Parameter(2))
        .unwrap();
    assert_eq!(binding.storage, bc::BindingStorage::Cell(bc::Reg(2)));
    assert_eq!(
        (binding.name, binding.ty, binding.mutable),
        (rest.name, rest.ty, rest.mutable)
    );
    assert!(pcs(take, bc::Opcode::CellNew).iter().all(|&pc| {
        take.code[pc].operands[0] as usize
            != take
                .bindings
                .iter()
                .position(|b| std::ptr::eq(b, binding))
                .unwrap()
    }));
    for (index, param) in sig.parameters.iter().enumerate() {
        let Some(bc::ParameterDefault::Function(default)) = param.default else {
            assert!(matches!(
                param.default,
                Some(bc::ParameterDefault::Constant(_))
            ));
            continue;
        };
        let d = &p.functions[default.index()];
        let ds = &p.signatures[d.signature.index()];
        assert_eq!(ds.parameters.len(), index);
        assert!(ds.rest.is_none());
        assert_eq!(d.captures, take.captures);
        for prefix in &d.bindings {
            if let bc::BindingInitialization::Parameter(i) = prefix.initialization {
                let owner = take
                    .bindings
                    .iter()
                    .find(|b| b.initialization == bc::BindingInitialization::Parameter(i))
                    .unwrap();
                assert_eq!(
                    (prefix.name, prefix.ty, prefix.mutable, prefix.storage),
                    (owner.name, owner.ty, owner.mutable, owner.storage)
                );
            }
        }
    }
    let rest_id = bc::BindingId(
        take.bindings
            .iter()
            .position(|b| std::ptr::eq(b, binding))
            .unwrap() as u32,
    );
    assert!(take.descriptors.iter().any(|d|matches!(d,bc::Descriptor::Closure{captures,..}if captures.contains(&bc::CaptureSource::Binding(rest_id)))));
    let readonly = function(p, "readonly");
    assert_eq!(
        readonly.bindings[0].storage,
        bc::BindingStorage::Value(bc::Reg(0))
    );
}

#[test]
fn optional_calls_guard_builder_and_all_argument_operations() {
    for expr in ["g?(...xs)", "g?.run?(...xs,touch(xs)).x", "(g?(...xs))?.x"] {
        let c = compiled(&format!("fn touch(x){{x}}fn f(g,xs){{{expr}}}"));
        let p = c.program.package();
        let f = function(p, "f");
        let new = pcs(f, bc::Opcode::ArgsNew)[0];
        let guards = pcs(f, bc::Opcode::BranchNull);
        let guard = *guards.iter().filter(|&&pc| pc < new).max().unwrap();
        let null = reachable(f, f.code[guard].operands[1] as usize);
        let nonnull = reachable(f, f.code[guard].operands[2] as usize);
        for pc in pcs(f, bc::Opcode::ArgsNew)
            .into_iter()
            .chain(pcs(f, bc::Opcode::ArgsSpread))
            .chain(pcs(f, bc::Opcode::ArgsPush))
            .chain(pcs(f, bc::Opcode::Call))
        {
            assert!(
                !null[pc] && nonnull[pc],
                "argument outside null guard at {pc}"
            );
        }
    }
}

#[test]
fn terminal_arguments_stop_append_call_and_preserve_shared_cleanup() {
    let c = compiled(
        "fn later(){1}fn clean(){null}fn f(g,xs){try{g(...xs,if true{throw 1;}else{return 2;},later())}finally{clean();}}",
    );
    let p = c.program.package();
    let f = function(p, "f");
    assert_eq!(calls(p, f), ["clean()"]);
    assert_eq!(pcs(f, bc::Opcode::ArgsSpread).len(), 1);
    assert!(pcs(f, bc::Opcode::ArgsPush).is_empty());
    let c = compiled(
        "fn clean(){null}fn f(g,xs,flag){try{g(...xs,if flag{throw 1;}else{4},...xs)}finally{clean();}}",
    );
    let p = c.program.package();
    let f = function(p, "f");
    let id = bc::FunctionId(p.functions.iter().position(|v| std::ptr::eq(v, f)).unwrap() as u32);
    let new = pcs(f, bc::Opcode::ArgsNew)[0];
    let cp = pcs(f, bc::Opcode::Call)[0];
    let dom = dominators(&c, id);
    assert!(dom[cp][new]);
    for pc in pcs(f, bc::Opcode::ArgsSpread) {
        assert!(dom[pc][new]);
        assert_eq!(
            c.program
                .exception_handler(id, bc::Pc(pc as u32), None)
                .unwrap()
                .unwrap()
                .pc,
            f.regions[0].finally.unwrap().start
        );
    }
}

#[test]
fn terminal_spread_sources_omit_pending_append_call_and_preserve_cleanup() {
    for expression in [
        "if flag{return [];}else{return [];}",
        "(if flag{return [];}else{return [];})",
        "if flag{throw 1;}else{throw 2;}",
        "if flag{try{throw 1;}finally{0;}}else{try{throw 2;}finally{0;}}",
    ] {
        let source = format!(
            "fn target(...rest){{rest}}fn later(){{2}}fn clean(){{null}}fn f(flag){{try{{target(1,...{expression},later())}}finally{{clean();}}}}"
        );
        let c = compiled(&source);
        let p = c.program.package();
        let f = function(p, "f");
        assert_eq!(calls(p, f), ["clean()"]);
        assert_eq!(pcs(f, bc::Opcode::ArgsNew).len(), 1);
        assert_eq!(pcs(f, bc::Opcode::ArgsPush).len(), 1);
        assert!(pcs(f, bc::Opcode::ArgsSpread).is_empty());
        let id =
            bc::FunctionId(p.functions.iter().position(|v| std::ptr::eq(v, f)).unwrap() as u32);
        let finally = f.regions[0].finally.unwrap().start;
        for pc in pcs(f, bc::Opcode::Return) {
            assert_eq!(
                c.program
                    .cleanup_handler(id, bc::Pc(pc as u32), bc::CleanupDestination::Return, None)
                    .unwrap()
                    .unwrap()
                    .pc,
                finally
            );
        }
        for pc in pcs(f, bc::Opcode::Throw) {
            let mut consumed = None;
            let mut handlers = Vec::new();
            while let Some(h) = c
                .program
                .exception_handler(id, bc::Pc(pc as u32), consumed)
                .unwrap()
            {
                handlers.push(h.pc);
                consumed = Some(h.region);
            }
            assert_eq!(handlers.last(), Some(&finally));
        }
    }
    for expression in [
        "if flag{return [];}else{[]}",
        "if flag{[]}else{return [];}",
        "if flag{throw 1;}else{()}",
        "if flag{()}else{throw 2;}",
    ] {
        let c = compiled(&format!(
            "fn target(...rest){{rest}}fn later(){{2}}fn clean(){{null}}fn f(flag){{try{{target(1,...{expression},later())}}finally{{clean();}}}}"
        ));
        let p = c.program.package();
        let f = function(p, "f");
        assert_eq!(
            calls(p, f),
            [
                "later()",
                format!("target(1,...{expression},later())").as_str(),
                "clean()"
            ]
        );
        let spread = pcs(f, bc::Opcode::ArgsSpread)[0];
        let id =
            bc::FunctionId(p.functions.iter().position(|v| std::ptr::eq(v, f)).unwrap() as u32);
        let dom = dominators(&c, id);
        let cp = pcs(f, bc::Opcode::Call);
        assert!(dom[cp[0]][spread] && dom[cp[1]][spread]);
        let branch = &f.code[pcs(f, bc::Opcode::BranchBool)[0]];
        let terminal_then =
            expression.starts_with("if flag{return") || expression.starts_with("if flag{throw");
        let terminal = reachable(
            f,
            branch.operands[if terminal_then { 1 } else { 2 }] as usize,
        );
        let continued = reachable(
            f,
            branch.operands[if terminal_then { 2 } else { 1 }] as usize,
        );
        for pc in [spread, cp[0], cp[1]] {
            assert!(!terminal[pc] && continued[pc]);
        }
        assert_eq!(
            c.program
                .exception_handler(id, bc::Pc(spread as u32), None)
                .unwrap()
                .unwrap()
                .pc,
            f.regions[0].finally.unwrap().start
        );
        let bc::Arguments::Packed(builder) = descriptor(f, cp[1]).arguments else {
            panic!("packed")
        };
        assert_cleared(f, builder.0, cp[1]);
    }
}

#[test]
fn sequential_builders_clear_and_reuse_registers_without_argument_windows() {
    let statement = "g(1,...xs,中文:2);g?(...xs);";
    let once = compiled(&format!("fn f(g,xs){{{statement}}}"));
    let repeated = compiled(&format!("fn f(g,xs){{{}}}", statement.repeat(300)));
    let f = function(repeated.program.package(), "f");
    assert_eq!(
        f.register_count,
        function(once.program.package(), "f").register_count
    );
    assert_eq!(pcs(f, bc::Opcode::ArgsNew).len(), 600);
    for pc in pcs(f, bc::Opcode::Call) {
        let bc::Arguments::Packed(builder) = descriptor(f, pc).arguments else {
            panic!("packed")
        };
        assert_cleared(f, builder.0, pc);
    }
    let many = compiled(&format!("fn f(g,xs){{g(...xs,{})}}", "1,".repeat(300)));
    let single = compiled("fn f(g,xs){g(...xs,1)}");
    assert_eq!(
        function(many.program.package(), "f").register_count,
        function(single.program.package(), "f").register_count
    );
}

#[test]
fn rest_functions_in_initializer_scopes_preserve_binding_identity() {
    let m = analyzed(
        "let mut x=[];if true{let mut local=[];fn take(a=||local,...mut rest:array){let c=||{rest=[];rest};(a,c)}take(...x);} ",
    );
    let c = lowering::compile(&m, &Linkage::default(), Options::default()).unwrap();
    super::initializer_locals::assert_local_structure(&m, &c);
    let take = function(c.program.package(), "take");
    assert_eq!(take.captures, [bc::CaptureKind::Cell]);
    assert_eq!(
        take.bindings
            .iter()
            .filter(|b| matches!(b.initialization, bc::BindingInitialization::Parameter(_)))
            .count(),
        2
    );
}

fn exact_limits(s: lowering::Stats) -> lowering::Limits {
    lowering::Limits {
        max_work: s.work_units,
        max_capacity_bytes: s.capacity_peak_bytes,
        max_records: s.records,
        max_blocks: s.blocks,
        max_instructions: s.instructions,
        max_registers: s.max_function_registers,
        ..Default::default()
    }
}

#[test]
fn packed_calls_and_rest_have_exact_budget_and_absolute_depth_boundaries() {
    let source = "fn take(a=1,...mut rest:array){||rest}fn f(g,xs){g?(...xs,中文:take(...xs))}";
    let m = analyzed(source);
    for emitted in [false, true] {
        let run = |limits| {
            let options = Options {
                limits,
                ..Default::default()
            };
            if emitted {
                lowering::compile(&m, &Linkage::default(), options).map(|c| c.stats)
            } else {
                lowering::lower(&m, &Linkage::default(), options).map(|ir| ir.stats())
            }
        };
        let s = run(lowering::Limits::default()).unwrap();
        let exact = exact_limits(s);
        assert_eq!(run(exact).unwrap(), s);
        eprintln!(
            "call-budget emitted={emitted} work={} capacity_peak={} records={} blocks={} instructions={} registers={}",
            s.work_units,
            s.capacity_peak_bytes,
            s.records,
            s.blocks,
            s.instructions,
            s.max_function_registers
        );
        for limits in [
            lowering::Limits {
                max_work: s.work_units - 1,
                ..exact
            },
            lowering::Limits {
                max_capacity_bytes: s.capacity_peak_bytes - 1,
                ..exact
            },
            lowering::Limits {
                max_records: s.records - 1,
                ..exact
            },
            lowering::Limits {
                max_blocks: s.blocks - 1,
                ..exact
            },
            lowering::Limits {
                max_instructions: s.instructions - 1,
                ..exact
            },
            lowering::Limits {
                max_registers: s.max_function_registers - 1,
                ..exact
            },
        ] {
            let e = run(limits).unwrap_err();
            assert_eq!(e.kind, lowering::ErrorKind::Limit);
            let range = byte_range(e.span);
            assert!(
                !range.is_empty()
                    && source.is_char_boundary(range.start)
                    && source.is_char_boundary(range.end)
            );
        }
    }
    let source = format!("fn f(g,xs){{{}xs{}}}", "g(...".repeat(12), ")".repeat(12));
    let m = analyzed(&source);
    let minimum = (1..64)
        .find(|&depth| {
            lowering::lower(
                &m,
                &Linkage::default(),
                Options {
                    limits: lowering::Limits {
                        max_depth: depth,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .is_ok()
        })
        .unwrap();
    assert!(minimum > 12);
    let e = lowering::lower(
        &m,
        &Linkage::default(),
        Options {
            limits: lowering::Limits {
                max_depth: minimum - 1,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.kind, lowering::ErrorKind::Limit);
    assert!(e.detail.contains("depth") && byte_range(e.span).len() < source.len());
}

#[test]
fn native_packed_calls_preserve_trusted_rest_defaults_abi_and_identity() {
    use aside_compiler::semantic::{
        CallEffect, ExternalParameter, ExternalSignature, ExternalSymbol, MemoryResolver,
        ModuleInterface, SymbolReference, TypeHint,
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
            parameters: vec![
                ExternalParameter {
                    name: "a".into(),
                    ty: TypeHint::Int,
                    mutable: false,
                    default: true,
                    rest: false,
                },
                ExternalParameter {
                    name: "rest".into(),
                    ty: TypeHint::Array,
                    mutable: false,
                    default: false,
                    rest: true,
                },
            ],
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
    let source = "fn f(xs){host(...xs)+(host)(...xs)}";
    let m = analyze_module(source, &options, &mut NoModules)
        .unwrap()
        .into_module()
        .unwrap();
    let mut package = native_seed().package().clone();
    let rest_name = bc::SymbolId(package.symbols.len() as u32);
    package.symbols.push("rest".into());
    let rest_type = bc::TypeId(package.types.len() as u32);
    package
        .types
        .push(bc::TypeDef::Builtin(bc::BuiltinType::Array));
    package.signatures[1].rest = Some(bc::Parameter {
        name: rest_name,
        ty: rest_type,
        mutable: false,
        default: None,
    });
    let seed = bc::verify(package, bc::Limits::default()).unwrap();
    let target = lowering::ExternalTarget::Symbol(
        m.external_bindings().iter().find_map(|e| e.symbol).unwrap(),
    );
    let links = [lowering::NativeLink {
        source_signature: None,
        target,
        native: bc::NativeId(0),
        reference: &reference,
    }];
    let linkage = Linkage {
        package: Some(&seed),
        natives: &links,
        expansions: &[],
    };
    let c = lowering::compile(&m, &linkage, Options::default()).unwrap();
    let p = c.program.package();
    assert_eq!(p.natives, seed.package().natives);
    assert_eq!(p.capabilities, seed.package().capabilities);
    assert_eq!(p.functions[0], seed.package().functions[0]);
    assert_eq!(p.signatures[1], seed.package().signatures[1]);
    assert_eq!(p.content_version, seed.package().content_version);
    assert_eq!(
        lowering::compile(
            &m,
            &linkage,
            Options {
                content_version: 77,
                ..Default::default()
            }
        )
        .unwrap_err()
        .kind,
        lowering::ErrorKind::LinkContract
    );
    for pc in pcs(function(p, "f"), bc::Opcode::Call) {
        let d = descriptor(function(p, "f"), pc);
        assert_eq!(d.target, bc::CallTarget::Native(bc::NativeId(0)));
        assert!(matches!(d.arguments, bc::Arguments::Packed(_)) && d.names.is_empty());
    }
    assert!(std::sync::Arc::ptr_eq(
        &m.external_bindings()[0].descriptor,
        &c.metadata.external_bindings[0].descriptor
    ));
    let bytes = bc::encode(p, bc::Limits::default()).unwrap();
    assert_eq!(
        bc::VerifiedProgram::decode(&bytes, bc::Limits::default())
            .unwrap()
            .package(),
        p
    );
    let mut asm = String::new();
    bc::disassemble(&c.program, &mut asm).unwrap();
    assert!(asm.contains("ArgsSpread"));
    for changed in [
        SymbolReference {
            module: "other".into(),
            ..reference.clone()
        },
        SymbolReference {
            version: "other".into(),
            ..reference.clone()
        },
        SymbolReference {
            export: Some("other".into()),
            ..reference.clone()
        },
    ] {
        let links = [lowering::NativeLink {
            source_signature: None,
            reference: &changed,
            ..links[0]
        }];
        let e = lowering::compile(
            &m,
            &Linkage {
                package: Some(&seed),
                natives: &links,
                expansions: &[],
            },
            Options::default(),
        )
        .unwrap_err();
        assert_eq!(e.kind, lowering::ErrorKind::LinkContract);
        let lowering::ExternalTarget::Symbol(symbol) = target else {
            unreachable!()
        };
        assert_eq!(e.span, m.symbols()[symbol.0 as usize].declaration);
        assert!(!byte_range(e.span).is_empty());
    }
    let mut package = seed.package().clone();
    package.signatures[1].rest = None;
    let missing_rest = bc::verify(package, bc::Limits::default()).unwrap();
    assert_eq!(
        lowering::compile(
            &m,
            &Linkage {
                package: Some(&missing_rest),
                natives: &links,
                expansions: &[],
            },
            Options::default()
        )
        .unwrap_err()
        .kind,
        lowering::ErrorKind::LinkContract
    );
    let optional = analyze_module("fn f(xs){host?(...xs)}", &options, &mut NoModules)
        .unwrap()
        .into_module()
        .unwrap();
    let links = [lowering::NativeLink {
        source_signature: None,
        target: lowering::ExternalTarget::Symbol(
            optional
                .external_bindings()
                .iter()
                .find_map(|e| e.symbol)
                .unwrap(),
        ),
        ..links[0]
    }];
    let e = lowering::compile(
        &optional,
        &Linkage {
            package: Some(&seed),
            natives: &links,
            expansions: &[],
        },
        Options::default(),
    )
    .unwrap();
    assert_eq!(
        pcs(function(e.program.package(), "f"), bc::Opcode::BranchNull).len(),
        1
    );

    // Resolver-native members use node identities rather than host symbols.
    let mut export = options.hosts[0].clone();
    export.name = "run".into();
    let mut resolver = MemoryResolver::default();
    resolver.insert(ModuleInterface {
        canonical_path: "reg::exports".into(),
        version: "metadata-2".into(),
        exports: vec![export],
    });
    for expression in ["native.run(...xs)", "((native.run))(...xs)"] {
        let source = format!("use reg::exports as native;fn f(xs){{{expression}}}");
        let m = analyze_module(&source, &AnalysisOptions::default(), &mut resolver)
            .unwrap()
            .into_module()
            .unwrap();
        let node = m
            .external_bindings()
            .iter()
            .find_map(|e| {
                e.node
                    .filter(|&n| m.hir().node(n).kind == hir::HirKind::Member)
            })
            .unwrap();
        let links = [lowering::NativeLink {
            source_signature: None,
            target: lowering::ExternalTarget::Node(node),
            native: bc::NativeId(0),
            reference: &reference,
        }];
        let c = lowering::compile(
            &m,
            &Linkage {
                package: Some(&seed),
                natives: &links,
                expansions: &[],
            },
            Options::default(),
        )
        .unwrap();
        let p = c.program.package();
        assert_eq!(p.natives, seed.package().natives);
        assert_eq!(p.signatures[1], seed.package().signatures[1]);
        let f = function(p, "f");
        assert!(pcs(f, bc::Opcode::MemberGet).is_empty());
        let pc = pcs(f, bc::Opcode::Call)[0];
        assert_eq!(text(p, f, pc), expression);
        let d = descriptor(f, pc);
        assert_eq!(d.target, bc::CallTarget::Native(bc::NativeId(0)));
        assert!(matches!(d.arguments, bc::Arguments::Packed(_)) && d.names.is_empty());
        let e = lowering::compile(&m, &Linkage::default(), Options::default()).unwrap_err();
        assert_eq!(e.kind, lowering::ErrorKind::MissingLink);
        assert_eq!(&source[byte_range(e.span)], "native.run");
    }
}
