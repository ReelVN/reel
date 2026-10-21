use aside_compiler::{hir::*, semantic::*, *};

#[path = "semantics/call_arguments.rs"]
mod call_arguments;
#[path = "semantics/initializer_locals.rs"]
mod initializer_locals;
#[path = "semantics/result_use.rs"]
mod result_use;
fn analyze_code(source: &str) -> Analysis {
    analyze_module(source, &AnalysisOptions::default(), &mut NoModules).unwrap()
}
fn valid(source: &str) -> AnalyzedModule {
    let r = analyze_code(source);
    assert!(r.is_ok(), "{source}\n{:?}", r.diagnostics);
    r.into_module().unwrap()
}
fn rejects(source: &str, code: DiagnosticCode) {
    let r = analyze_code(source);
    assert!(
        r.diagnostics.iter().any(|d| d.code == code),
        "{source}\n{:?}",
        r.diagnostics
    );
    assert!(r.module().is_none());
}
fn symbols<'a>(m: &'a AnalyzedModule, name: &str) -> Vec<(SymbolId, &'a Symbol)> {
    m.symbols()
        .iter()
        .enumerate()
        .filter(|(_, s)| m.hir().name(s.name) == name)
        .map(|(i, s)| (SymbolId(i as u32), s))
        .collect()
}
#[test]
fn finally_await_is_lexically_forbidden_even_on_short_circuit_paths() {
    for source in [
        "fn f(p){try{}finally{false && await p;}}",
        "fn f(p){try{}finally{true || await p;}}",
        "fn f(p){try{}finally{1 ?? await p;}}",
        "fn f(p){try{}finally{null?[await p];}}",
    ] {
        rejects(source, DiagnosticCode::NoWait);
    }
    rejects(
        "fn f(p){try{}finally{if false {await p;}}}",
        DiagnosticCode::NoWait,
    );
    rejects(
        "fn f(xs){try{}finally{if false {for await x in xs {}}}}",
        DiagnosticCode::NoWait,
    );
    valid("fn f(p){try{}finally{let callback=||{await p;};}}");
    valid(
        "fn f(callback){try{}finally{false && callback(); true || callback(); 1 ?? callback(); null?[callback()];}}",
    );
}
#[test]
fn pattern_alternatives_and_nonmatching_arms_are_valid_source() {
    let m = valid("let (x,_) | (x,_,_) = (1,2); let y:int=x;");
    assert!(matches!(
        m.types()[symbols(&m, "x")[0].1.ty.0 as usize],
        Type::Int
    ));
    valid("match 1 {(a,b)=>0,_=>1,}");
}
#[test]
fn unknown_spreads_do_not_claim_positional_parameter_slots() {
    let m = valid("fn f(a){} fn g(xs){f(...xs,a:1);}");
    assert!(
        m.facts()
            .iter()
            .any(|f| f.checks.0 & RuntimeChecks::CALL != 0)
    );
    valid("fn f(a,b){} fn g(xs){f(...xs,2,a:1);}");
    valid("fn f(a){} f(...(),a:1);");
    rejects("fn f(a){} f(...(1,),a:1);", DiagnosticCode::InvalidCall);
    rejects(
        "fn f(a){} fn g(xs){f(1,...xs,a:1);}",
        DiagnosticCode::InvalidCall,
    );
    rejects(
        "fn f(a:int){} fn g(xs){f(...xs,a:\"bad\");}",
        DiagnosticCode::TypeMismatch,
    );
}
#[test]
fn grouped_and_destructured_assignments_check_leaf_contracts() {
    rejects(
        "let mut x:int=1; (x)=\"bad\";",
        DiagnosticCode::TypeMismatch,
    );
    rejects(
        "let mut a:int=1; let mut b:int=2; (a,b)=(\"bad\",2);",
        DiagnosticCode::TypeMismatch,
    );
    rejects("let mut x=1; (x,x)=(1,2);", DiagnosticCode::PatternBindings);
}
#[test]
fn nested_if_expressions_keep_condition_and_body_roles() {
    valid("if if true { true } else { false } {}");
    valid("while if true { false } else { true } {break;}");
    valid("fn f(xs){for x in if true {xs} else {[]} {x;}}");
}
#[test]
fn forward_function_signatures_are_independent_of_source_order() {
    valid("fn f(cb:fn(int)->int=g){cb(1)} fn g(x:int)->int{x}");
    valid("fn f(){let cb:fn(int)->int=g;} fn g(x:int)->int{x}");
    rejects(
        "fn f(){let cb:fn(int)->int=g;} fn g(x:bool)->bool{x}",
        DiagnosticCode::TypeMismatch,
    );
    rejects(
        "let mut (a,b):(int,int)=(1,2); (a,b)=(1,\"bad\");",
        DiagnosticCode::TypeMismatch,
    );
    let m = valid("fn f(value){let mut x:int=1; (x)=value;}");
    assert!(
        m.facts()
            .iter()
            .any(|facts| facts.checks.0 & RuntimeChecks::TYPE != 0)
    );
}
#[test]
fn forward_return_contracts_reach_bodies_defaults_and_locals() {
    for source in [
        "fn f()->int{g()} fn g()->bool{true}",
        "fn g()->bool{true} fn f()->int{g()}",
        "fn f(x:int=g()){} fn g()->bool{true}",
        "fn f(){let x:int=g();} fn g()->bool{true}",
        "fn f()->int{g()} fn g()->bool{f()==0}",
    ] {
        rejects(source, DiagnosticCode::TypeMismatch);
    }
    valid("fn f()->int{g()} fn g()->int{f()}");
    valid("fn f(x:int=g())->int{x} fn g()->int{1}");
    valid("fn f(){let x:int=g();} fn g()->int{1}");
}
#[test]
fn declaration_work_is_admitted_before_symbol_allocation() {
    let mut source = String::from("fn consumer()->int{last()} ");
    for i in 0..10_000 {
        source.push_str(&format!("fn unused_{i}(){{}} "));
    }
    source.push_str("fn last()->int{1}");
    for max_work in [0, 1, 5, 100, 10_004, 10_010, 20_000] {
        let options = AnalysisOptions {
            limits: AnalysisLimits {
                max_work,
                ..Default::default()
            },
            ..Default::default()
        };
        let result = analyze_module(&source, &options, &mut NoModules).unwrap();
        assert!(result.exhausted);
        assert!(result.module().is_none());
        assert!(
            result.stats.symbols <= max_work,
            "work={max_work}, stats={:?}",
            result.stats
        );
        if max_work <= 10_004 {
            assert_eq!(result.stats.symbol_bytes, 0);
        }
        // After seeded names, each named declaration admits its own metadata
        // work and symbol work separately before either allocation.
        assert!(
            result.stats.symbols <= max_work.saturating_sub(10_004) / 2,
            "predeclaration admission: {max_work}, {:?}",
            result.stats
        );
    }
}
#[test]
fn short_circuit_effects_inherit_conditional_execution() {
    let options = AnalysisOptions {
        hosts: vec![ExternalSymbol::function(
            "effect",
            ExternalSignature {
                result_parameter: None,
                result_cases: Vec::new(),
                parameters: vec![],
                result: TypeHint::Bool,
                effect: CallEffect::NoWait,
                init_safe: Some(false),
                derived_safe: Some(false),
            },
        )],
        ..Default::default()
    };
    for source in [
        "false && effect();",
        "true || effect();",
        "1 ?? effect();",
        "null?[effect()];",
    ] {
        let result = analyze_module(source, &options, &mut NoModules).unwrap();
        assert!(result.is_ok(), "{source}: {:?}", result.diagnostics);
        assert!(
            result
                .module()
                .unwrap()
                .facts()
                .iter()
                .any(|facts| facts.checks.0 & RuntimeChecks::INIT_SAFE != 0)
        );
    }
    let result = analyze_module("true && effect();", &options, &mut NoModules).unwrap();
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::InvalidEffect)
    );
}
#[test]
fn shared_large_signatures_are_built_once_and_charge_parameter_work() {
    let parameters = (0..1000)
        .map(|i| format!("p{i}=0"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!("fn f({parameters}){{}}{}", "f();".repeat(1000));
    let result = analyze_code(&source);
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    assert_eq!(result.stats.signature_builds, 1);
    assert_eq!(result.stats.signature_parameters, 1000);
    assert!(result.stats.work_units < 20_000);
    let options = AnalysisOptions {
        limits: AnalysisLimits {
            max_work: result.stats.work_units - 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let result = analyze_module(&source, &options, &mut NoModules).unwrap();
    assert!(result.exhausted);
    assert!(result.module().is_none());
    assert_eq!(
        result.stats.signature_builds, 0,
        "signature scan must be admitted before allocation"
    );
    let parameters = (0..10_000)
        .map(|i| format!("p{i}=0"))
        .collect::<Vec<_>>()
        .join(",");
    let source = format!("fn f({parameters}){{}}{}", "f();".repeat(10_000));
    let options = AnalysisOptions {
        limits: AnalysisLimits {
            max_work: 100_000,
            ..Default::default()
        },
        ..Default::default()
    };
    let result = analyze_module(&source, &options, &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    assert_eq!(result.stats.signature_builds, 1);
    assert_eq!(result.stats.signature_parameters, 10_000);
    assert!(result.stats.work_units < 100_000);
    let options = AnalysisOptions {
        hosts: vec![ExternalSymbol::function(
            "native",
            ExternalSignature {
                result_parameter: None,
                result_cases: Vec::new(),
                parameters: (0..100)
                    .map(|i| ExternalParameter {
                        name: format!("p{i}"),
                        ty: TypeHint::Int,
                        mutable: false,
                        default: true,
                        rest: false,
                    })
                    .collect(),
                result: TypeHint::Null,
                effect: CallEffect::NoWait,
                init_safe: Some(true),
                derived_safe: Some(true),
            },
        )],
        ..Default::default()
    };
    let result = analyze_module(&"native();".repeat(100), &options, &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    assert_eq!(result.stats.signature_builds, 1);
    assert_eq!(result.stats.signature_parameters, 100);
}
#[test]
fn returning_try_and_loop_do_not_add_an_implicit_null() {
    valid("fn f()->int {try{return 1;}finally{}}");
    valid("fn f()->int {loop{return 1;}}");
    rejects("fn f()->int {loop{break;}}", DiagnosticCode::TypeMismatch);
}
#[test]
fn lexical_scope_shadowing_and_precise_reads() {
    let m = valid("let x=1; fn f(){ let x=2; { let y=x; } x } f();");
    assert_eq!(symbols(&m, "x").len(), 2);
    for (i, n) in m.hir().nodes.iter().enumerate() {
        if n.kind == HirKind::Name && m.hir().text(n.span).trim() == "x" {
            assert!(m.facts()[i].symbol.is_some());
        }
    }
    rejects("let x=x;", DiagnosticCode::UninitializedBinding);
    rejects(
        "fn f(){ y; let y=1; }",
        DiagnosticCode::UninitializedBinding,
    );
    rejects("{ let private=1; } private;", DiagnosticCode::UnknownName);
    rejects("let x=1; let x=2;", DiagnosticCode::DuplicateBinding);
    rejects("let x=1; x=2;", DiagnosticCode::ImmutableBinding);
    valid("let t={x:1}; t.x=2;");
}
#[test]
fn parameter_defaults_have_their_own_control_boundary() {
    for (source, text) in [
        ("fn f(x=if true{return 1;}else{2}){x}", "return 1;"),
        ("fn f(x=if false{return 1;}else{2}){x}", "return 1;"),
        (
            "fn f(x=true||(if true{return true;}else{false})){x}",
            "return true;",
        ),
        ("fn f(x=1??(if true{return 1;}else{2})){x}", "return 1;"),
        ("fn f(x=if true{loop{return 1;} 2}else{3}){x}", "return 1;"),
        (
            "fn f(x=||{return 1;},y=if true{return 2;}else{3}){x}",
            "return 2;",
        ),
    ] {
        let analysis = analyze_code(source);
        let errors: Vec<_> = analysis
            .diagnostics
            .iter()
            .filter(|d| d.code == DiagnosticCode::InvalidDefault)
            .collect();
        assert_eq!(errors.len(), 1, "{source}: {:?}", analysis.diagnostics);
        assert_eq!(&source[byte_range(errors[0].range)], text);
        assert!(analysis.into_module().is_err());
    }
    for source in [
        "fn f(x=if true{let y=2;y}else{3}){x}",
        "fn f(x=||{return 1;}){x()}",
        "fn f(x=if true{fn nested(){return 1;} nested()}else{2}){x}",
        "fn f(x=if true{loop{break;} 2}else{3}){x}",
        "fn f(x=if true{while true{continue;} 2}else{3}){x}",
        "fn f(x=if true{throw 1;}else{2}){x}",
        "fn f(x=if true{inner:while if true{break inner;}else{true}{} 2}else{3}){x}",
    ] {
        valid(source);
    }
    for source in [
        "fn outer(){loop{fn f(x=if true{break;}else{2}){x} break;}}",
        "fn outer(){loop{fn f(x=if true{continue;}else{2}){x} break;}}",
    ] {
        rejects(source, DiagnosticCode::InvalidControl);
    }
}

#[test]
fn defaults_capture_definition_environment_and_previous_parameters() {
    let m = valid("fn outer(){let value=2; fn f(a=value,b=a){b} return f;}");
    let value = symbols(&m, "value")[0].0;
    let f = m
        .functions()
        .iter()
        .find(|f| f.parameters.len() == 2)
        .unwrap();
    assert!(
        f.captures
            .iter()
            .any(|c| c.symbol == value && c.used_in_default)
    );
    rejects("fn f(a=b,b=1){a}", DiagnosticCode::InvalidDefault);
    rejects("fn f(a=a){a}", DiagnosticCode::InvalidDefault);
    rejects("fn f(a=await null){a}", DiagnosticCode::NoWait);
    valid("fn f(children=[]){children} f();");
}
#[test]
fn shared_cells_and_forward_readonly_capture_initialization() {
    let m = valid("fn outer(){fn read(){x} let x=1; return read;}");
    let (x, s) = symbols(&m, "x")[0];
    assert!(!s.mutable);
    assert_eq!(s.storage, Storage::SharedCell);
    assert!(m.functions().iter().any(|f| {
        f.captures
            .iter()
            .any(|c| c.symbol == x && c.mode == CaptureMode::Cell)
    }));
    let m = valid("fn outer(){let mut x=0; let a=||{x+=1;}; let b=||x; return (a,b);}");
    let (x, s) = symbols(&m, "x")[0];
    assert!(s.mutable);
    assert_eq!(
        m.functions()
            .iter()
            .flat_map(|f| &f.captures)
            .filter(|c| c.symbol == x && c.mode == CaptureMode::Cell)
            .count(),
        2
    );
    let m = valid("fn outer(){fn a(){b()} fn b(){a()} return a;}");
    assert!(
        m.functions()
            .iter()
            .flat_map(|f| &f.captures)
            .all(|c| c.mode == CaptureMode::Cell)
    );
    valid("fn outer(){let x=1; let read=||x; return read;}");
}
#[test]
fn loop_iteration_bindings_and_nested_capture_chain() {
    let m = valid("fn f(){for i in 0..4 {let mut n=i; let x=|| {||{n+=1; n}};}} ");
    let (_, n) = symbols(&m, "n")[0];
    assert!(n.per_iteration);
    assert_eq!(n.storage, Storage::SharedCell);
    assert!(symbols(&m, "i")[0].1.per_iteration);
    assert_eq!(
        m.functions()
            .iter()
            .flat_map(|f| &f.captures)
            .filter(|c| m.hir().name(m.symbols()[c.symbol.0 as usize].name) == "n")
            .count(),
        2
    );
}
#[test]
fn class_receivers_and_plain_table_closures() {
    let m = valid(
        "class Base {fn init(self,x=1){} fn get(self){1} static fn make(){1}} class Child:Base {fn get(self){super.get()}} let plain={method:|x|x}; plain.method(2); let c=Child(); c.get();",
    );
    assert_eq!(
        m.functions()
            .iter()
            .filter(|f| f.receiver == Receiver::ExplicitSelf)
            .count(),
        3
    );
    assert_eq!(
        m.functions()
            .iter()
            .filter(|f| f.receiver == Receiver::Static)
            .count(),
        1
    );
    assert!(
        m.functions()
            .iter()
            .any(|f| f.receiver == Receiver::None && m.hir().node(f.node).kind == HirKind::Closure)
    );
    rejects("class A { fn bad(x){} }", DiagnosticCode::InvalidReceiver);
    rejects(
        "class A { static fn bad(self){} }",
        DiagnosticCode::InvalidReceiver,
    );
    rejects("let t={method:||self};", DiagnosticCode::UnknownName);
}
#[test]
fn patterns_or_atomic_defaults_and_assignment() {
    valid("fn f(event){match event {(x,_)|(_,x)=>x,_=>null,}}");
    rejects(
        "fn f(v){match v {(x,_)|(_,y)=>x,_=>null,}}",
        DiagnosticCode::PatternBindings,
    );
    rejects("let (x,x)=(1,2);", DiagnosticCode::PatternBindings);
    rejects("let (a,b)=(1,);", DiagnosticCode::TypeMismatch);
    valid("let mut a=0; let mut b=0; (a,b)=(1,2);");
    rejects(
        "let a=0; let mut b=0; (a,b)=(1,2);",
        DiagnosticCode::ImmutableBinding,
    );
    valid("let {name, score:points=0,...rest}={name:1};");
    valid("fn f(v){if let (x,y)=v {x+y;}else{null;} while let (a,b)=v {break;}}");
    rejects(
        "fn f(v){if let x=v {} else {x;}}",
        DiagnosticCode::UnknownName,
    );
}
#[test]
fn finally_transfers_and_dynamic_no_wait() {
    let m = valid("fn f(){outer:loop{try {return 1;} finally {loop{break;}}}}");
    assert!(
        m.facts()
            .iter()
            .filter_map(|f| f.control.as_ref())
            .any(|c| !c.finally.is_empty())
    );
    rejects(
        "fn f(){try {} finally {return;}}",
        DiagnosticCode::InvalidControl,
    );
    rejects(
        "fn f(){outer:loop{try {} finally {break outer;}}}",
        DiagnosticCode::InvalidControl,
    );
    rejects("throw;", DiagnosticCode::InvalidControl);
    valid("fn f(){try {throw 1;}catch e{throw;}finally{1;}}");
    rejects(
        "fn f(){try {} finally {await null;}}",
        DiagnosticCode::NoWait,
    );
    // A possibility summary must never reject a concrete nonwaiting path.
    let m =
        valid("fn maybe(wait,p){if wait {await p;}} fn f(p){try {} finally {maybe(false,p);}} ");
    assert!(m.functions().iter().any(|f| f.known_wait));
    assert!(
        m.facts()
            .iter()
            .any(|f| f.checks.0 & RuntimeChecks::NO_WAIT != 0)
    );
    valid("fn f(callback){try{}finally{callback();}}");
    // Defining a callback in a restricted context does not execute its body.
    valid("fn create(p){let callback=||{await p;};}");
}
#[test]
fn static_types_signatures_and_dynamic_unknowns() {
    rejects("if 1 {}", DiagnosticCode::TypeMismatch);
    rejects("let x:bool=1;", DiagnosticCode::TypeMismatch);
    rejects(
        "fn f(a:int)->int {a} f(true);",
        DiagnosticCode::TypeMismatch,
    );
    rejects("fn f(a,b=1){a} f(z:2);", DiagnosticCode::InvalidCall);
    rejects("fn f(a,b){a} f(1,a:2,b:3);", DiagnosticCode::InvalidCall);
    valid("fn f(a,b=1,...rest){a} f(1,2,3,4);");
    valid("fn f(x){if x {x;} x(unknown:x); let y:int=x;}");
    rejects("fn f(){await 1;}", DiagnosticCode::TypeMismatch);
    rejects("fn f()->int {true}", DiagnosticCode::TypeMismatch);
    valid("fn f()->int {return 1;}");
}
#[test]
fn diagnostics_and_cst_recovery_gate_are_bounded() {
    let source = "let x = ;";
    let r = analyze_code(source);
    assert!(!r.is_ok());
    assert!(r.module().is_none());
    let source = "bad;".repeat(1000);
    let options = AnalysisOptions {
        limits: AnalysisLimits {
            max_diagnostics: 3,
            ..Default::default()
        },
        ..Default::default()
    };
    let r = analyze_module(&source, &options, &mut NoModules).unwrap();
    assert_eq!(r.diagnostics.len(), 3);
    let options = AnalysisOptions {
        limits: AnalysisLimits {
            max_nodes: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    let r = analyze_module("let x=1;", &options, &mut NoModules).unwrap();
    assert!(r.exhausted);
    assert!(r.module().is_none());
}
#[test]
fn forms_preserve_real_operators_and_normalize_trailing_children() {
    let m = valid("fn panel(x,children=[]){children} panel(1){null;};");
    let calls = m
        .hir()
        .nodes
        .iter()
        .filter_map(|n| {
            if let Form::Call {
                callee,
                arguments,
                children: Some(children),
                ..
            } = n.form
            {
                Some((callee, arguments, children))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(m.hir().node(calls[0].0).kind, HirKind::Name);
    assert!(calls[0].1.is_some());
    rejects(
        "fn panel(x,children=[]){children} panel(){null;};",
        DiagnosticCode::InvalidCall,
    );
    valid("fn f(a,b){a} fn forward(values){f(...values);}");
    valid("let n=null; n?(1);");
    rejects(
        "fn f(p,flag){try{}finally{if flag{await p;}}}",
        DiagnosticCode::NoWait,
    );
    let m = valid("fn f(){let x:fn(int)->int=g; fn g(v:int)->int{v} x(1);} ");
    assert!(
        m.facts()
            .iter()
            .any(|f| matches!(m.types()[f.ty.0 as usize], Type::Callable(..)))
    );
    let m = valid("let x:int=0Xdead;");
    assert!(!m.hir().nodes.is_empty());
}
#[test]
fn field_initializer_has_its_own_receiver_and_definition_capture() {
    let m = valid(
        "fn factory(){let seed=1; return class {let value=seed; let callback=||self.value; fn init(self){}};}",
    );
    assert_eq!(
        m.functions()
            .iter()
            .filter(|f| f.role == FunctionRole::FieldInitializer)
            .count(),
        2
    );
    let seed = symbols(&m, "seed")[0].0;
    assert!(
        m.functions()
            .iter()
            .filter(|f| f.role == FunctionRole::FieldInitializer)
            .any(|f| f.captures.iter().any(|c| c.symbol == seed))
    );
    assert!(
        m.functions()
            .iter()
            .flat_map(|f| &f.captures)
            .any(|c| m.hir().name(m.symbols()[c.symbol.0 as usize].name) == "self")
    );
}
#[test]
fn bound_method_aliases_constructor_arguments_and_inherited_receivers() {
    valid(
        "class A{fn init(self,x:int){} fn get(self,x:int){x}} class B:A{} let obj=B(1); let method=obj.get; method(2);",
    );
    rejects(
        "class A{fn init(self,x:int){}} A(true);",
        DiagnosticCode::TypeMismatch,
    );
    rejects(
        "class A{fn init(self,x:int){}} A();",
        DiagnosticCode::InvalidCall,
    );
    valid("class A{fn get(self,x:int){x}} let a=A(); let bound:fn(int)->int=a.get; bound(2);");
}
#[test]
fn registered_intrinsics_observable_locations_and_derived_write_effects() {
    let options = AnalysisOptions {
        hosts: vec![
            ExternalSymbol::intrinsic("bind", Intrinsic::Bind),
            ExternalSymbol::intrinsic("derive", Intrinsic::Derive),
            ExternalSymbol::dynamic("model"),
            ExternalSymbol::dynamic("unknown"),
        ],
        ..Default::default()
    };
    let run = |source: &str| analyze_module(source, &options, &mut NoModules).unwrap();
    let result = run("let t={x:1}; bind(t.x);");
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    assert!(
        result
            .module()
            .unwrap()
            .facts()
            .iter()
            .any(|f| f.checks.0 & RuntimeChecks::PROPERTY != 0)
    );
    let result = run("bind(1+2);");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::TypeMismatch)
    );
    let result = run("fn f(){let t=model({x:1}); derive(||{t.x=2;});}");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::InvalidEffect)
    );
    let result = run("derive(||{let tmp={x:1};tmp.x=2;tmp.x});");
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let result = run("derive(||unknown());");
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    assert!(
        result
            .module()
            .unwrap()
            .facts()
            .iter()
            .any(|f| f.checks.0 & RuntimeChecks::DERIVED_SAFE != 0)
    );
    let result = run("fn f(){let value=1;bind(model({x:value}).x);} ");
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    // The registration applies by binding identity; local functions can shadow names.
    let result = run("fn f(){fn bind(x){x} bind(3);}");
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}
#[test]
fn closure_and_class_value_categories_accept_all_legal_signatures() {
    valid("fn f(x){x} let callable:closure=f; callable(1); let c:class=class{};");
    rejects("let t=(1,2); t[0]=3;", DiagnosticCode::ImmutableBinding);
}
#[test]
fn async_generator_yield_and_for_await_keep_distinct_function_roles() {
    let m = valid(
        "async fn task(p){await p} gen fn values(n){let received=yield n; return received;} fn consume(source){for await item in source{item;}} let pending=task(null); let generator=values(1);",
    );
    assert!(m.functions().iter().any(|f| f.kind == FunctionKind::Async));
    assert!(
        m.functions()
            .iter()
            .any(|f| f.kind == FunctionKind::Generator)
    );
    rejects("fn f(){yield 1;}", DiagnosticCode::InvalidControl);
    rejects("gen fn f(p){await p;}", DiagnosticCode::NoWait);
    rejects(
        "gen fn f(){try{}finally{yield 1;}}",
        DiagnosticCode::InvalidControl,
    );
    let r = analyze_source(
        "let input=yield 1;",
        ParseOptions {
            entry: EntryPoint::Block,
            ..Default::default()
        },
        &AnalysisOptions {
            context: AnalysisContext::Generator,
            ..Default::default()
        },
        &mut NoModules,
    )
    .unwrap();
    assert!(r.is_ok(), "{:?}", r.diagnostics);
    assert_eq!(
        r.module().unwrap().functions()[0].kind,
        FunctionKind::Generator
    );
}

#[test]
fn contract_preparation_preserves_default_visibility_and_method_order() {
    rejects("fn f(x=||later,later=1){}", DiagnosticCode::UnknownName);
    rejects("fn f(x=||x){}", DiagnosticCode::UnknownName);
    let m = valid("fn owner(){let later=1; fn f(x=||later,later=2){}}");
    let outer = symbols(&m, "later")[0].0;
    assert!(
        m.functions()
            .iter()
            .any(|f| f.captures.iter().any(|c| c.symbol == outer))
    );
    valid("fn f(mut x=1,y=||{x+=1;x},z=y()){x}");
    valid("fn f(x=if true {let later=1;later}else{0},later=2){}");
    rejects(
        "class C{fn f(self)->int{self.g()} fn g(self)->bool{true}}",
        DiagnosticCode::TypeMismatch,
    );
    valid("class C{fn f(self)->int{self.g()} fn g(self)->int{self.f()}}");
    valid("class C{} fn f(C:C)->int{1}");
    rejects("class C{} fn f(C:C)->C{C}", DiagnosticCode::InvalidType);
}

#[test]
fn lowering_metadata_retains_entry_and_declared_function_contexts() {
    let source = "fn plain(){} async fn task(){} gen fn stream(){yield 1;} class C{let field=1; fn init(self){} fn _tostring(self)->string{\"C\"} fn _call(self){}}";
    let m = valid(source);
    assert_eq!(m.entry_context(), AnalysisContext::Module);
    for f in m.functions() {
        let name = m
            .hir()
            .tokens(f.node)
            .find_map(|t| t.name)
            .map(|n| m.hir().name(n));
        let no_wait = f.kind == FunctionKind::Generator
            || f.role == FunctionRole::FieldInitializer
            || matches!(name, Some("init" | "_tostring"));
        assert_eq!(f.declared_context.no_wait, no_wait, "{name:?}");
        assert!(!f.declared_context.init);
        assert!(!f.declared_context.derived);
        assert_eq!(
            f.declared_context.effect(),
            if no_wait {
                CallEffect::NoWait
            } else {
                CallEffect::MayWait
            }
        );
    }
    for context in [
        AnalysisContext::Function,
        AnalysisContext::Generator,
        AnalysisContext::NoWait,
    ] {
        let result = analyze_module(
            "1;",
            &AnalysisOptions {
                context,
                ..Default::default()
            },
            &mut NoModules,
        )
        .unwrap();
        let m = result.into_module().unwrap();
        assert_eq!(m.entry_context(), context);
        if context == AnalysisContext::Generator {
            assert!(m.functions()[0].declared_context.no_wait);
        }
    }
    let options = AnalysisOptions {
        hosts: vec![ExternalSymbol::intrinsic("derive", Intrinsic::Derive)],
        ..Default::default()
    };
    let m = analyze_module("let d=derive(||1);", &options, &mut NoModules)
        .unwrap()
        .into_module()
        .unwrap();
    let derived = m
        .functions()
        .iter()
        .find(|f| f.declared_context.derived)
        .unwrap();
    assert!(derived.declared_context.no_wait);
    let identity = m.symbol_reference(SymbolId(0)).unwrap();
    assert_eq!(
        (identity.module, identity.version),
        (m.canonical_path(), m.version())
    );
    assert!(m.symbol_reference(SymbolId(u32::MAX)).is_none());
}

#[test]
fn trusted_native_reference_survives_aliases_without_guessed_linkage() {
    let reference = SymbolReference {
        module: "reel::trusted".into(),
        version: "native-abi-v3".into(),
        export: Some("registered_function_id".into()),
    };
    let host = ExternalSymbol::dynamic("native").with_reference(reference.clone());
    let options = AnalysisOptions {
        hosts: vec![host, ExternalSymbol::dynamic("unresolved")],
        ..Default::default()
    };
    let m = analyze_module(
        "let alias=native; alias(); unresolved();",
        &options,
        &mut NoModules,
    )
    .unwrap()
    .into_module()
    .unwrap();
    let resolved: Vec<_> = m
        .external_bindings()
        .iter()
        .filter(|b| b.descriptor.name == "native")
        .collect();
    assert!(!resolved.is_empty());
    assert!(
        resolved
            .iter()
            .all(|b| b.descriptor.reference.as_deref() == Some(&reference))
    );
    assert!(
        m.external_bindings()
            .iter()
            .filter(|b| b.descriptor.name == "unresolved")
            .all(|b| b.descriptor.reference.is_none())
    );
}
