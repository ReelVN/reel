//! L07 inspects compilation and the shared CFG; it never evaluates bytecode.
use super::collections_locations::{assert_cleared, assert_live, calls, definition, pcs, text};
use super::*;
use aside_compiler::{DiagnosticCode, byte_range};

#[test]
fn member_symbols_and_string_indices_remain_distinct() {
    let c = compiled("fn f(){let t={分数:0};t.分数=1;t.分数+=2;(t.分数,t[\"分数\"])}");
    let p = c.program.package();
    let f = function(p, "f");
    let gets = pcs(f, bc::Opcode::MemberGet);
    let sets = pcs(f, bc::Opcode::MemberSet);
    assert_eq!((gets.len(), sets.len()), (2, 2));
    assert_eq!(pcs(f, bc::Opcode::IndexGet).len(), 1);
    assert!(pcs(f, bc::Opcode::IndexSet).is_empty());
    for pc in gets.into_iter().chain(sets) {
        assert_eq!(
            p.symbols[f.code.instruction(pc).unwrap().unwrap().operands[2] as usize],
            "分数"
        );
        assert_eq!(text(p, f, pc), "t.分数");
    }
    assert_eq!(
        bc::Opcode::MemberGet.spec().semantic_rule,
        bc::SemanticRule::MemberLookup
    );
}

#[test]
fn nested_member_index_receivers_are_evaluated_once_in_source_order() {
    let c = compiled(
        "fn receiver(x){x}fn key(){0}fn rhs(){2}fn f(a){receiver(a).分组[key()].尾=rhs();}",
    );
    let p = c.program.package();
    let f = function(p, "f");
    assert_eq!(calls(p, f), ["receiver(a)", "key()", "rhs()"]);
    let cp = pcs(f, bc::Opcode::Call);
    let member = pcs(f, bc::Opcode::MemberGet)[0];
    let index = pcs(f, bc::Opcode::IndexGet)[0];
    let set = pcs(f, bc::Opcode::MemberSet)[0];
    assert_eq!(pcs(f, bc::Opcode::MemberGet).len(), 1);
    assert!(cp[0] < member && member < cp[1] && cp[1] < index && index < cp[2]);
    assert_eq!(
        f.code.instruction(member).unwrap().unwrap().operands[1],
        f.code.instruction(cp[0]).unwrap().unwrap().operands[0]
    );
    assert_eq!(
        f.code.instruction(index).unwrap().unwrap().operands[1..],
        [
            f.code.instruction(member).unwrap().unwrap().operands[0],
            f.code.instruction(cp[1]).unwrap().unwrap().operands[0]
        ]
    );
    assert_eq!(
        f.code.instruction(set).unwrap().unwrap().operands[..2],
        [
            f.code.instruction(index).unwrap().unwrap().operands[0],
            f.code.instruction(cp[2]).unwrap().unwrap().operands[0]
        ]
    );
    assert_eq!(
        p.symbols[f.code.instruction(member).unwrap().unwrap().operands[2] as usize],
        "分组"
    );
    assert_eq!(
        p.symbols[f.code.instruction(set).unwrap().unwrap().operands[2] as usize],
        "尾"
    );
    assert_live(
        f,
        f.code.instruction(index).unwrap().unwrap().operands[0],
        index,
        set,
    );
    assert_cleared(
        f,
        f.code.instruction(index).unwrap().unwrap().operands[0],
        set,
    );
}

#[test]
fn mixed_chain_keeps_all_positions_and_reverses_member_index_cell_global_writes() {
    let c = compiled(
        "let mut global=0;fn obj(x){x}fn key(){0}fn rhs(){9}\
         fn f(a,b){let mut local=0;let c=||{global=local=obj(a).左=obj(b)[key()]=rhs();};c}",
    );
    let p = c.program.package();
    let f = function(p, "$closure");
    assert_eq!(calls(p, f), ["obj(a)", "obj(b)", "key()", "rhs()"]);
    let cp = pcs(f, bc::Opcode::Call);
    let index = pcs(f, bc::Opcode::IndexSet)[0];
    let member = pcs(f, bc::Opcode::MemberSet)[0];
    let cell = pcs(f, bc::Opcode::CellWrite)[0];
    let global = pcs(f, bc::Opcode::GlobalWrite)[0];
    let rhs = f.code.instruction(cp[3]).unwrap().unwrap().operands[0];
    assert!(cp[3] < index && index < member && member < cell && cell < global);
    assert_eq!(
        f.code.instruction(index).unwrap().unwrap().operands,
        [
            f.code.instruction(cp[1]).unwrap().unwrap().operands[0],
            f.code.instruction(cp[2]).unwrap().unwrap().operands[0],
            rhs
        ]
    );
    assert_eq!(
        f.code.instruction(member).unwrap().unwrap().operands[..2],
        [f.code.instruction(cp[0]).unwrap().unwrap().operands[0], rhs]
    );
    assert_eq!(
        p.symbols[f.code.instruction(member).unwrap().unwrap().operands[2] as usize],
        "左"
    );
    assert_eq!(f.code.instruction(cell).unwrap().unwrap().operands[1], rhs);
    assert_eq!(
        f.code.instruction(global).unwrap().unwrap().operands[0],
        rhs
    );
    assert!(pcs(f, bc::Opcode::MemberGet).is_empty());
    assert!(pcs(f, bc::Opcode::IndexGet).is_empty());
    for (from, to) in [
        (cp[0], member),
        (cp[1], index),
        (cp[2], index),
        (cp[3], global),
    ] {
        let reg = f.code.instruction(from).unwrap().unwrap().operands[0];
        assert_live(f, reg, from, to);
        assert_cleared(f, reg, to);
    }
}

#[test]
fn rhs_rebinding_uses_the_saved_object_value_for_simple_and_compound_writes() {
    for operator in ["=", "+="] {
        let c = compiled(&format!(
            "fn f(){{let mut receiver={{x:1}};let other={{x:2}};\
             let rhs=||{{receiver=other;3}};receiver.x{operator}rhs();}}"
        ));
        let p = c.program.package();
        let f = function(p, "f");
        let receiver = f
            .bindings
            .iter()
            .position(|b| p.symbols[b.name.index()] == "receiver")
            .unwrap();
        assert!(matches!(
            f.bindings[receiver].storage,
            bc::BindingStorage::Cell(_)
        ));
        let call = pcs(f, bc::Opcode::Call)[0];
        let set = pcs(f, bc::Opcode::MemberSet)[0];
        let saved = f.code.instruction(set).unwrap().unwrap().operands[0];
        let prepare = definition(f, saved, call);
        assert_eq!(
            f.code.instruction(prepare).unwrap().unwrap().opcode,
            bc::Opcode::BindRead as u16
        );
        assert_eq!(
            f.code.instruction(prepare).unwrap().unwrap().operands[1],
            receiver as u32
        );
        assert!(prepare < call && call < set);
        assert_live(f, saved, prepare, set);
        assert_cleared(f, saved, set);
        let gets = pcs(f, bc::Opcode::MemberGet);
        assert_eq!(gets.len(), usize::from(operator == "+="));
        if let Some(&get) = gets.first() {
            assert!(prepare < get && get < call);
            assert_eq!(f.code.instruction(get).unwrap().unwrap().operands[1], saved);
            let add = pcs(f, bc::Opcode::Add)[0];
            assert_eq!(
                f.code.instruction(add).unwrap().unwrap().operands[1],
                f.code.instruction(get).unwrap().unwrap().operands[0]
            );
            assert_eq!(
                f.code.instruction(set).unwrap().unwrap().operands[1],
                f.code.instruction(add).unwrap().unwrap().operands[0]
            );
        } else {
            assert_eq!(
                f.code.instruction(set).unwrap().unwrap().operands[1],
                f.code.instruction(call).unwrap().unwrap().operands[0]
            );
        }
        let closure = function(p, "$closure");
        assert_eq!(pcs(closure, bc::Opcode::CellWrite).len(), 1);
        let captures = f
            .descriptors
            .iter()
            .find_map(|d| match d {
                bc::Descriptor::Closure { function, captures }
                    if p.functions[function.index()].name == closure.name =>
                {
                    Some(captures)
                }
                _ => None,
            })
            .unwrap();
        let slot = captures
            .iter()
            .position(|&capture| {
                capture == bc::CaptureSource::Binding(bc::BindingId(receiver as u32))
            })
            .unwrap();
        assert_eq!(closure.captures[slot], bc::CaptureKind::Cell);
    }
}

#[test]
fn all_ten_member_compounds_read_before_rhs_and_write_the_original_receiver() {
    for (operator, opcode) in [
        ("+=", bc::Opcode::Add),
        ("-=", bc::Opcode::Sub),
        ("*=", bc::Opcode::Mul),
        ("/=", bc::Opcode::Div),
        ("%=", bc::Opcode::Rem),
        ("&=", bc::Opcode::BitAnd),
        ("|=", bc::Opcode::BitOr),
        ("^=", bc::Opcode::BitXor),
        ("<<=", bc::Opcode::Shl),
        (">>=", bc::Opcode::Shr),
    ] {
        let c = compiled(&format!(
            "fn obj(x){{x}}fn rhs(){{2}}fn f(a){{obj(a).x{operator}rhs();}}"
        ));
        let p = c.program.package();
        let f = function(p, "f");
        assert_eq!(calls(p, f), ["obj(a)", "rhs()"]);
        let cp = pcs(f, bc::Opcode::Call);
        let get = pcs(f, bc::Opcode::MemberGet)[0];
        let op = pcs(f, opcode)[0];
        let set = pcs(f, bc::Opcode::MemberSet)[0];
        assert!(cp[0] < get && get < cp[1] && cp[1] < op && op < set);
        assert_eq!(
            f.code.instruction(get).unwrap().unwrap().operands[1],
            f.code.instruction(cp[0]).unwrap().unwrap().operands[0]
        );
        assert_eq!(
            f.code.instruction(set).unwrap().unwrap().operands[0],
            f.code.instruction(get).unwrap().unwrap().operands[1]
        );
        assert_eq!(
            f.code.instruction(set).unwrap().unwrap().operands[2],
            f.code.instruction(get).unwrap().unwrap().operands[2]
        );
        assert_eq!(
            f.code.instruction(op).unwrap().unwrap().operands[1..],
            [
                f.code.instruction(get).unwrap().unwrap().operands[0],
                f.code.instruction(cp[1]).unwrap().unwrap().operands[0]
            ]
        );
        assert_eq!(
            f.code.instruction(set).unwrap().unwrap().operands[1],
            f.code.instruction(op).unwrap().unwrap().operands[0]
        );
        assert_live(
            f,
            f.code.instruction(get).unwrap().unwrap().operands[1],
            cp[0],
            set,
        );
        assert_live(
            f,
            f.code.instruction(get).unwrap().unwrap().operands[0],
            get,
            op,
        );
        assert_cleared(
            f,
            f.code.instruction(set).unwrap().unwrap().operands[0],
            set,
        );
    }
}

#[test]
fn member_callees_precede_arguments_and_do_not_inject_self() {
    let source = "fn first(){1}fn second(){2}\
                  fn f(){let t={method:|self,x|x};t.method(first(),x:second());\
                  let alias=t.method;alias(3,4)}";
    let c = compiled(source);
    let p = c.program.package();
    let f = function(p, "f");
    let cp = pcs(f, bc::Opcode::Call);
    let gets = pcs(f, bc::Opcode::MemberGet);
    assert_eq!(cp.len(), 4);
    assert_eq!(gets.len(), 2);
    assert!(gets[0] < cp[0] && cp[0] < cp[1] && cp[1] < cp[2] && cp[2] < gets[1]);
    let bc::Descriptor::Call(call) =
        &f.descriptors[f.code.instruction(cp[2]).unwrap().unwrap().operands[1] as usize]
    else {
        panic!()
    };
    assert_eq!(
        call.target,
        bc::CallTarget::Register(bc::Reg(
            f.code.instruction(gets[0]).unwrap().unwrap().operands[0]
        ))
    );
    let bc::Arguments::Window(window) = call.arguments else {
        panic!()
    };
    assert_eq!(window.len, 2);
    assert_eq!(call.names.len(), 1);
    assert_eq!(
        (
            p.symbols[call.names[0].name.index()].as_str(),
            call.names[0].offset
        ),
        ("x", 1)
    );
    assert_live(
        f,
        f.code.instruction(gets[0]).unwrap().unwrap().operands[0],
        gets[0],
        cp[2],
    );
    assert_cleared(
        f,
        f.code.instruction(gets[0]).unwrap().unwrap().operands[0],
        cp[2],
    );
    let bc::Descriptor::Call(alias) =
        &f.descriptors[f.code.instruction(cp[3]).unwrap().unwrap().operands[1] as usize]
    else {
        panic!()
    };
    let bc::Arguments::Window(window) = alias.arguments else {
        panic!()
    };
    assert_eq!(window.len, 2);
    let closure = function(p, "$closure");
    let signature = &p.signatures[closure.signature.index()];
    assert_eq!(signature.parameters.len(), 2);
    assert_eq!(p.symbols[signature.parameters[0].name.index()], "self");
    assert!(
        analyzed(source)
            .functions()
            .iter()
            .all(|f| f.receiver == hir::Receiver::None)
    );
    let c = compiled("fn f(obj){obj.callback=|self,x|x;obj.callback(1,2)}");
    let f = function(c.program.package(), "f");
    assert_eq!(pcs(f, bc::Opcode::MemberSet).len(), 1);
    let get = pcs(f, bc::Opcode::MemberGet)[0];
    let pc = pcs(f, bc::Opcode::Call)[0];
    let bc::Descriptor::Call(call) =
        &f.descriptors[f.code.instruction(pc).unwrap().unwrap().operands[1] as usize]
    else {
        panic!()
    };
    let bc::Arguments::Window(window) = call.arguments else {
        panic!()
    };
    assert_eq!(window.len, 2);
    assert_eq!(
        call.target,
        bc::CallTarget::Register(bc::Reg(
            f.code.instruction(get).unwrap().unwrap().operands[0]
        ))
    );
}

#[test]
fn defaults_and_initializer_captures_keep_member_operations_and_binding_identity() {
    let c = compiled("{let t={x:1};fn f(a=t.x){let c=||t.x;c()}f();t.x=2;}");
    let p = c.program.package();
    let f = function(p, "f");
    let default =
        &p.functions[function_default(&p.signatures[f.signature.index()].parameters[0]).index()];
    assert_eq!(default.context, bc::Context::NoWait);
    assert_eq!(default.captures, f.captures);
    assert_eq!(pcs(default, bc::Opcode::MemberGet).len(), 1);
    assert_eq!(pcs(function(p, "$closure"), bc::Opcode::MemberGet).len(), 1);
    assert_eq!(pcs(function(p, "$module"), bc::Opcode::MemberSet).len(), 1);
    assert!(p.modules[0].globals.is_empty());
}

#[test]
fn terminal_member_receiver_or_rhs_has_no_pending_writes_or_calls() {
    for statement in [
        "(if flag{return 1;}else{throw 2;}).x=after();",
        "a.x=(if flag{return 1;}else{throw 2;}).y=after();",
        "a.x=if flag{return 1;}else{throw 2;};",
        "a.x+=if flag{return 1;}else{throw 2;};",
        "(if flag{return 1;}else{throw 2;}).method(after());",
        "a.method(if flag{return 1;}else{throw 2;},after());",
    ] {
        let c = compiled(&format!(
            "fn after(){{0}}fn cleanup(){{1}}fn f(a,flag){{try{{{statement}}}finally{{cleanup();}}}}"
        ));
        let p = c.program.package();
        let f = function(p, "f");
        assert!(pcs(f, bc::Opcode::MemberSet).is_empty());
        assert!(pcs(f, bc::Opcode::IndexSet).is_empty());
        assert!(calls(p, f).iter().all(|&s| s == "cleanup()"));
        assert_eq!(
            pcs(f, bc::Opcode::MemberGet).len(),
            usize::from(statement.contains("+=") || statement.starts_with("a.method"))
        );
        let id =
            bc::FunctionId(p.functions.iter().position(|d| std::ptr::eq(d, f)).unwrap() as u32);
        for opcode in [bc::Opcode::Return, bc::Opcode::Throw] {
            let pc = pcs(f, opcode)[0];
            let handler = if opcode == bc::Opcode::Return {
                c.program
                    .cleanup_handler(id, bc::Pc(pc as u32), bc::CleanupDestination::Return, None)
                    .unwrap()
            } else {
                c.program
                    .exception_handler(id, bc::Pc(pc as u32), None)
                    .unwrap()
            };
            assert_eq!(handler.unwrap().pc, f.regions[0].finally.unwrap().start);
        }
    }
}

#[test]
fn conditional_paths_dominate_member_uses_and_keep_cleanup_edges() {
    let c = compiled(
        "fn obj(x){x}fn rhs(){2}fn cleanup(){0}\
         fn f(a,flag){try{obj(a).x+=if flag{return 1;}else{rhs()};}finally{cleanup();}}",
    );
    let p = c.program.package();
    let f = function(p, "f");
    let id = bc::FunctionId(p.functions.iter().position(|d| std::ptr::eq(d, f)).unwrap() as u32);
    let dom = super::initializer_locals::dominators(&c, id);
    let get = pcs(f, bc::Opcode::MemberGet)[0];
    let add = pcs(f, bc::Opcode::Add)[0];
    let set = pcs(f, bc::Opcode::MemberSet)[0];
    let receiver = f.code.instruction(set).unwrap().unwrap().operands[0];
    let prepare = definition(f, receiver, get);
    assert!(dom[get][prepare] && dom[set][prepare] && dom[add][get]);
    assert_live(f, receiver, prepare, set);
    assert_live(
        f,
        f.code.instruction(get).unwrap().unwrap().operands[0],
        get,
        add,
    );
    assert!(dom[set][add]);
    let ret = pcs(f, bc::Opcode::Return)
        .into_iter()
        .find(|&pc| text(p, f, pc) == "return 1;")
        .unwrap();
    assert_eq!(
        c.program
            .cleanup_handler(id, bc::Pc(ret as u32), bc::CleanupDestination::Return, None)
            .unwrap()
            .unwrap()
            .pc,
        f.regions[0].finally.unwrap().start
    );
    assert_eq!(
        c.program
            .exception_handler(id, bc::Pc(set as u32), None)
            .unwrap()
            .unwrap()
            .pc,
        f.regions[0].finally.unwrap().start
    );
}

#[test]
fn member_deferred_constructs_remain_located_rejections() {
    {
        let (source, expected, detail) = ("fn f(){let [a]=[1];}", "[a]", "pattern");
        let m = analyzed(source);
        let e = lowering::lower(&m, &Linkage::default(), Options::default()).unwrap_err();
        assert_eq!(e.kind, lowering::ErrorKind::Unsupported);
        assert_eq!(&source[byte_range(e.span)], expected);
        assert!(
            e.detail.to_lowercase().contains(&detail.to_lowercase()),
            "{}",
            e.detail
        );
    }
    for source in ["fn f(){let a=0;a=1;}", "fn f(a){let x=(a.x=1);}"] {
        let analysis = analyze_module(source, &AnalysisOptions::default(), &mut NoModules).unwrap();
        assert!(!analysis.is_ok());
        assert!(analysis.diagnostics.iter().any(|d| matches!(
            d.code,
            DiagnosticCode::ImmutableBinding | DiagnosticCode::InvalidAssignment
        )));
    }
}

#[test]
fn member_positions_and_symbols_obey_register_work_capacity_and_depth_limits() {
    let source = "fn f(a){a.x=a.y=a.z=1;}";
    let m = analyzed(source);
    let e = lowering::lower(
        &m,
        &Linkage::default(),
        Options {
            limits: lowering::Limits {
                max_registers: 3,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.kind, lowering::ErrorKind::Limit);
    assert_eq!(&source[byte_range(e.span)], "a");
    assert!(byte_range(e.span).start >= source.find("a.z").unwrap());
    let source = format!("fn f(a){{a{}}}", ".x".repeat(20));
    lowering::lower(
        &analyzed(&source),
        &Linkage::default(),
        Options {
            limits: lowering::Limits {
                max_depth: 12,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let source = format!("fn f(a){{{}a.x{}}}", "(".repeat(20), ")".repeat(20));
    let e = lowering::lower(
        &analyzed(&source),
        &Linkage::default(),
        Options {
            limits: lowering::Limits {
                max_depth: 12,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.kind, lowering::ErrorKind::Limit);
    assert!(e.detail.contains("depth"));
    assert!(byte_range(e.span).len() < source.len());
    let m = analyzed("fn f(a){let mut name=0;name=a.中文=a[0]=1;a.中文+=2;a.method(a.中文)}");
    let ir = lowering::lower(&m, &Linkage::default(), Options::default()).unwrap();
    let stats = ir.stats();
    for limits in [
        lowering::Limits {
            max_work: stats.work_units - 1,
            ..Default::default()
        },
        lowering::Limits {
            max_capacity_bytes: stats.capacity_peak_bytes - 1,
            ..Default::default()
        },
    ] {
        let e = lowering::lower(
            &m,
            &Linkage::default(),
            Options {
                limits,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(e.kind, lowering::ErrorKind::Limit);
    }
    lowering::lower(
        &m,
        &Linkage::default(),
        Options {
            limits: lowering::Limits {
                max_work: stats.work_units,
                max_capacity_bytes: stats.capacity_peak_bytes,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let statement = "a.x=a.y=a[0]=1;a.x+=2;a.method(a.x);";
    let once = compiled(&format!("fn f(a){{{statement}}}"));
    let repeated = compiled(&format!("fn f(a){{{}}}", statement.repeat(300)));
    let p = repeated.program.package();
    let f = function(p, "f");
    assert_eq!(
        f.register_count,
        function(once.program.package(), "f").register_count
    );
    assert_eq!(pcs(f, bc::Opcode::MemberSet).len(), 900);
    assert_eq!(pcs(f, bc::Opcode::IndexSet).len(), 300);
    for opcode in [bc::Opcode::MemberGet, bc::Opcode::MemberSet] {
        for pc in pcs(f, opcode) {
            assert_cleared(
                f,
                f.code.instruction(pc).unwrap().unwrap().operands[if opcode == bc::Opcode::MemberGet
                {
                    1
                } else {
                    0
                }],
                pc,
            );
        }
    }
}

#[test]
fn resolved_native_members_keep_trusted_linkage_for_calls_and_values() {
    use aside_compiler::semantic::{
        CallEffect, ExternalParameter, ExternalSignature, ExternalSymbol, MemoryResolver,
        ModuleInterface, SymbolReference, TypeHint,
    };
    let reference = SymbolReference {
        module: "registry::test".into(),
        version: "native-v19".into(),
        export: Some("run".into()),
    };
    let mut resolver = MemoryResolver::default();
    resolver.insert(ModuleInterface {
        canonical_path: "reg::exports".into(),
        version: "metadata-2".into(),
        exports: vec![
            ExternalSymbol::function(
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
            .with_reference(reference.clone()),
        ],
    });
    let seed = native_seed();
    for expression in [
        "native.run()",
        "(native.run)()",
        "((native.run))()",
        "native.run(a:1)",
        "(native.run)(a:1)",
        "((native.run))(a:1)",
        "(((native.run)))(a:1)",
    ] {
        let source = format!("use reg::exports as native;fn f(){{{expression}}}");
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
        let link = lowering::NativeLink {
            source_signature: None,
            target: lowering::ExternalTarget::Node(node),
            native: bc::NativeId(0),
            reference: &reference,
        };
        let native_links = [link];
        let linkage = Linkage {
            package: Some(&seed),
            natives: &native_links,
            expansions: &[],
        };
        let c = lowering::compile(&m, &linkage, Options::default()).unwrap();
        let p = c.program.package();
        assert_eq!(p.natives[0], seed.package().natives[0]);
        assert_eq!(p.functions[0], seed.package().functions[0]);
        assert_eq!(
            p.signatures[1].parameters[0].default,
            Some(bc::ParameterDefault::Function(bc::FunctionId(0)))
        );
        let f = function(p, "f");
        assert!(pcs(f, bc::Opcode::MemberGet).is_empty());
        let call_pcs = pcs(f, bc::Opcode::Call);
        assert_eq!(call_pcs.len(), 1);
        let pc = call_pcs[0];
        assert_eq!(text(p, f, pc), expression);
        let bc::Descriptor::Call(call) =
            &f.descriptors[f.code.instruction(pc).unwrap().unwrap().operands[1] as usize]
        else {
            panic!("missing native call descriptor");
        };
        assert_eq!(call.target, bc::CallTarget::Native(bc::NativeId(0)));
        let bc::Arguments::Window(window) = call.arguments else {
            panic!("native call arguments must use an ordered window");
        };
        let named = expression.ends_with("(a:1)");
        assert_eq!(window.len, u32::from(named));
        assert_eq!(call.names.len(), usize::from(named));
        if named {
            assert_eq!(call.names[0].offset, 0);
            assert_eq!(p.symbols[call.names[0].name.index()], "a");
        }
        let error = lowering::compile(&m, &Linkage::default(), Options::default()).unwrap_err();
        assert_eq!(error.kind, lowering::ErrorKind::MissingLink);
        assert_eq!(&source[byte_range(error.span)], "native.run");
        assert_eq!(error.stats.capacity_bytes, 0);
        let wrong_reference = SymbolReference {
            version: "wrong-version".into(),
            ..reference.clone()
        };
        let error = lowering::compile(
            &m,
            &Linkage {
                package: Some(&seed),
                natives: &[lowering::NativeLink {
                    source_signature: None,
                    reference: &wrong_reference,
                    ..link
                }],
                expansions: &[],
            },
            Options::default(),
        )
        .unwrap_err();
        assert_eq!(error.kind, lowering::ErrorKind::LinkContract);
        assert_eq!(&source[byte_range(error.span)], "native.run");
        if expression.starts_with('(') {
            let group = m.hir().node(node).parent.unwrap();
            assert_eq!(m.hir().node(group).kind, hir::HirKind::Group);
            assert!(m.external_bindings().iter().any(|e| e.node == Some(group)));
            let error = lowering::lower(
                &m,
                &Linkage {
                    package: Some(&seed),
                    natives: &[lowering::NativeLink {
                        source_signature: None,
                        target: lowering::ExternalTarget::Node(group),
                        ..link
                    }],
                    expansions: &[],
                },
                Options::default(),
            )
            .unwrap_err();
            assert_eq!(error.kind, lowering::ErrorKind::MissingLink);
            assert_eq!(&source[byte_range(error.span)], "native.run");
            assert_eq!(error.stats.capacity_bytes, 0);
        }
        if expression == "(((native.run)))(a:1)" {
            let error = lowering::lower(
                &m,
                &linkage,
                Options {
                    limits: lowering::Limits {
                        max_depth: 4,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert_eq!(error.kind, lowering::ErrorKind::Limit);
            assert!(error.detail.contains("qualified callee depth"));
            assert_eq!(&source[byte_range(error.span)], "native.run");
            assert_eq!(error.stats.capacity_bytes, 0);
            let work = lowering::lower(
                &m,
                &linkage,
                Options {
                    limits: lowering::Limits {
                        max_work: error.stats.work_units - 1,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert_eq!(work.kind, lowering::ErrorKind::Limit);
            assert!(work.detail.contains("work budget"));
            assert_eq!(&source[byte_range(work.span)], "native.run");
            assert_eq!(work.stats.capacity_bytes, 0);
            let stats = lowering::lower(&m, &linkage, Options::default())
                .unwrap()
                .stats();
            for limits in [
                lowering::Limits {
                    max_work: stats.work_units - 1,
                    ..Default::default()
                },
                lowering::Limits {
                    max_capacity_bytes: stats.capacity_peak_bytes - 1,
                    ..Default::default()
                },
            ] {
                let error = lowering::lower(
                    &m,
                    &linkage,
                    Options {
                        limits,
                        ..Default::default()
                    },
                )
                .unwrap_err();
                assert_eq!(error.kind, lowering::ErrorKind::Limit);
            }
            lowering::lower(
                &m,
                &linkage,
                Options {
                    limits: lowering::Limits {
                        max_work: stats.work_units,
                        max_capacity_bytes: stats.capacity_peak_bytes,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        }
    }
    for expression in [
        "native.run",
        "native.run=1;",
        "native.run.x",
        "native.run?.x",
        "((native.run))",
        "(((native.run)))=1;",
        "((native.run))+=1;",
        "let alias=((native.run));alias();",
        "((native.run)).x()",
    ] {
        let source = format!("use reg::exports as native;fn f(){{{expression}}}");
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
        let link = lowering::NativeLink {
            source_signature: None,
            target: lowering::ExternalTarget::Node(node),
            native: bc::NativeId(0),
            reference: &reference,
        };
        let result = lowering::compile(
            &m,
            &Linkage {
                package: Some(&seed),
                natives: &[link],
                expansions: &[],
            },
            Options::default(),
        );
        if expression.contains('=') && !expression.starts_with("let ") {
            let error = result.unwrap_err();
            assert_eq!(error.kind, lowering::ErrorKind::Unsupported);
            assert_eq!(error.stats.capacity_bytes, 0);
        } else {
            let c = result.unwrap();
            assert_eq!(
                pcs(function(c.program.package(), "f"), bc::Opcode::MakeClosure).len(),
                1
            );
        }
    }
    let c = compiled("fn f(){let native={run:|a|a};native.run(3)}");
    let f = function(c.program.package(), "f");
    assert_eq!(pcs(f, bc::Opcode::MemberGet).len(), 1);
    let call = pcs(f, bc::Opcode::Call)[0];
    assert!(
        matches!(&f.descriptors[f.code.instruction(call).unwrap().unwrap().operands[1] as usize],
        bc::Descriptor::Call(c) if matches!(c.target, bc::CallTarget::Register(_)))
    );
}
