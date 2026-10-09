use super::*;

#[test]
fn binding_origin_is_independent_of_storage_and_requires_a_module_root() {
    let m = valid(
        "pub let root=0;pub fn entry(){root} while if true{let condition=true;condition}else{false}{let mut body=1;let capture=||body;try{throw 1;}catch error{fn inspect(){error}inspect();}break;}fn f(){let local=1;local}",
    );
    for name in ["root", "entry", "f"] {
        assert_eq!(symbols(&m, name)[0].1.origin, BindingOrigin::ModuleGlobal);
    }
    for name in ["condition", "body", "capture", "error", "inspect"] {
        let s = symbols(&m, name)[0].1;
        assert_eq!(s.origin, BindingOrigin::EntryLocal);
        assert!(
            s.per_iteration,
            "condition and handler scopes are reentered too"
        );
        assert_eq!(s.owner, None);
    }
    assert_eq!(symbols(&m, "body")[0].1.storage, Storage::SharedCell);
    assert_eq!(symbols(&m, "error")[0].1.storage, Storage::SharedCell);
    assert!(matches!(
        symbols(&m, "local")[0].1.origin,
        BindingOrigin::FunctionLocal(_)
    ));
    for s in m.symbols() {
        assert_eq!(s.owner, s.origin.function());
    }
    let no_wait = analyze_module(
        "let x=1;fn f(){x}",
        &AnalysisOptions {
            context: AnalysisContext::NoWait,
            ..Default::default()
        },
        &mut NoModules,
    )
    .unwrap()
    .into_module()
    .unwrap();
    assert!(
        no_wait
            .symbols()
            .iter()
            .all(|s| s.origin != BindingOrigin::ModuleGlobal)
    );
    assert_eq!(
        symbols(&no_wait, "x")[0].1.origin,
        BindingOrigin::EntryLocal
    );
    let block = analyze_source(
        "let x=1;x;",
        ParseOptions {
            entry: EntryPoint::Block,
            ..Default::default()
        },
        &AnalysisOptions::default(),
        &mut NoModules,
    )
    .unwrap()
    .into_module()
    .unwrap();
    assert_eq!(symbols(&block, "x")[0].1.origin, BindingOrigin::EntryLocal);
    assert!(block.exports().is_empty());
}

#[test]
fn initializer_forward_reads_and_recursive_captures_keep_initialization_contracts() {
    rejects("if true{x;let x=1;}", DiagnosticCode::UninitializedBinding);
    rejects("if true{let x=x;}", DiagnosticCode::UninitializedBinding);
    rejects(
        "if true{let x=1;let write=||{x=2;};}",
        DiagnosticCode::ImmutableBinding,
    );
    let m = valid("if true{fn first(){later}fn a(){b()}fn b(){a()}first();let later=null;}");
    let (later, s) = symbols(&m, "later")[0];
    assert_eq!(s.origin, BindingOrigin::EntryLocal);
    assert_eq!(s.initialization, Initialization::Declaration);
    assert_eq!(s.storage, Storage::SharedCell);
    assert!(!s.mutable);
    assert!(m.functions().iter().any(|f| {
        f.captures
            .iter()
            .any(|c| c.symbol == later && c.mode == CaptureMode::Cell)
    }));
    assert!(
        m.facts()
            .iter()
            .any(|f| f.symbol == Some(later) && f.checks.0 & RuntimeChecks::INITIALIZED != 0)
    );
    let promoted =
        valid("if true{let x=1;let first=||x;if true{fn later(){x}later();}let last=||x;}");
    let (x, s) = symbols(&promoted, "x")[0];
    assert_eq!(s.origin, BindingOrigin::EntryLocal);
    assert_eq!(s.storage, Storage::SharedCell);
    let captures: Vec<_> = promoted
        .functions()
        .iter()
        .flat_map(|f| &f.captures)
        .filter(|c| c.symbol == x)
        .collect();
    assert_eq!(captures.len(), 3);
    assert!(captures.iter().all(|c| c.mode == CaptureMode::Cell));
}

#[test]
fn enclosing_bindings_keep_lexical_ownership_and_multilevel_capture_promotion() {
    for context in [
        AnalysisContext::Function,
        AnalysisContext::Module,
        AnalysisContext::NoWait,
    ] {
        let m = analyze_source(
            "let first=||{||external};if true{fn later(){external}later();}",
            ParseOptions {
                entry: EntryPoint::Block,
                ..Default::default()
            },
            &AnalysisOptions {
                context,
                enclosing: vec![EnclosingBinding {
                    symbol: ExternalSymbol::value("external", TypeHint::Int),
                    mutable: false,
                }],
                ..Default::default()
            },
            &mut NoModules,
        )
        .unwrap()
        .into_module()
        .unwrap();
        let (external, s) = symbols(&m, "external")[0];
        assert_eq!(s.kind, SymbolKind::Local);
        assert_eq!(s.initialization, Initialization::External);
        assert_eq!(s.storage, Storage::SharedCell);
        assert!(s.declaration.is_empty());
        assert_eq!(
            s.origin,
            if context == AnalysisContext::Function {
                BindingOrigin::FunctionLocal(FunctionId(0))
            } else {
                BindingOrigin::EntryLocal
            }
        );
        assert_eq!(s.owner, s.origin.function());
        let captures: Vec<_> = m
            .functions()
            .iter()
            .flat_map(|f| &f.captures)
            .filter(|c| c.symbol == external)
            .collect();
        assert_eq!(captures.len(), 3);
        assert!(captures.iter().all(|c| c.mode == CaptureMode::Cell));
        assert!(
            m.external_bindings()
                .iter()
                .any(|b| b.symbol == Some(external))
        );
    }
    let m = valid("if true{let value=1;let first=||value;let next=||{||value};}");
    let (value, s) = symbols(&m, "value")[0];
    assert_eq!(s.storage, Storage::ValueCapture);
    assert_eq!(
        m.functions()
            .iter()
            .flat_map(|f| &f.captures)
            .filter(|c| c.symbol == value && c.mode == CaptureMode::Value)
            .count(),
        3
    );
}

#[test]
fn initializer_captures_and_promotions_use_existing_semantic_quotas() {
    let source = "if true{let x=1;let first=||x;if true{fn next(){x}next();}let last=||{||x};}";
    let normal = analyze_code(source);
    assert!(normal.is_ok());
    for limits in [
        AnalysisLimits {
            max_symbols: 1,
            ..Default::default()
        },
        AnalysisLimits {
            max_captures: 0,
            ..Default::default()
        },
        AnalysisLimits {
            max_work: normal.stats.work_units - 1,
            ..Default::default()
        },
    ] {
        let r = analyze_module(
            source,
            &AnalysisOptions {
                limits,
                ..Default::default()
            },
            &mut NoModules,
        )
        .unwrap();
        assert!(r.module().is_none());
        assert!(r.exhausted);
        let error = r
            .diagnostics
            .iter()
            .find(|d| d.code == DiagnosticCode::BudgetExceeded)
            .unwrap();
        assert!(source.is_char_boundary(usize::from(error.range.start())));
        assert!(source.is_char_boundary(usize::from(error.range.end())));
        assert!(r.stats.symbols <= limits.max_symbols);
        assert!(r.stats.captures <= limits.max_captures);
    }
    let exact = analyze_module(
        source,
        &AnalysisOptions {
            limits: AnalysisLimits {
                max_work: normal.stats.work_units,
                max_symbols: normal.stats.symbols,
                max_captures: normal.stats.captures,
                ..Default::default()
            },
            ..Default::default()
        },
        &mut NoModules,
    )
    .unwrap();
    assert!(exact.is_ok(), "{:?}", exact.diagnostics);
}
