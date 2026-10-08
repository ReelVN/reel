use aside_compiler::{hir::*, semantic::*, *};
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
    valid("fn f(p,flag){try{}finally{if flag{await p;}}}");
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
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::TypeMismatch)
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
