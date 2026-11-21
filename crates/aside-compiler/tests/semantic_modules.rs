use aside_compiler::{hir::*, semantic::*, *};
fn input(path: &str, source: &str) -> SourceModule {
    SourceModule {
        source_path: None,
        canonical_path: path.into(),
        version: "test-v1".into(),
        source: source.into(),
    }
}

#[test]
fn imported_plain_call_uses_inferred_await_diagnostic_at_the_use() {
    let source = "use model::load; fn inspect() { await load(); }";
    let result = analyze_project(
        &[input("main", source), input("model", "pub fn load() { 42 }")],
        &AnalysisOptions::default(),
        &mut NoModules,
    )
    .unwrap();
    assert_eq!(result.diagnostics.len(), 1, "{:?}", result.diagnostics);
    let diagnostic = &result.diagnostics[0];
    assert_eq!(diagnostic.module, "main");
    assert_eq!(diagnostic.diagnostic.code, DiagnosticCode::TypeMismatch);
    assert_eq!(&source[diagnostic.diagnostic.range], "await");
}

#[test]
fn imported_class_member_diagnostics_share_the_export_contract() {
    let code = "use model::Item; fn inspect(item:Item){item.missing();}";
    let result = analyze_project(
        &[
            input("main", code),
            input("model", "pub class Item{fn read(self){1}}"),
        ],
        &AnalysisOptions::default(),
        &mut NoModules,
    )
    .unwrap();
    assert_eq!(result.diagnostics.len(), 1, "{:?}", result.diagnostics);
    let diagnostic = &result.diagnostics[0];
    assert_eq!(diagnostic.module, "main");
    assert_eq!(diagnostic.diagnostic.code, DiagnosticCode::UnknownMember);
    assert_eq!(&code[diagnostic.diagnostic.range], "missing");
}

#[test]
fn type_only_dependencies_preserve_value_reexports_and_unused_imports() {
    for (source, expected) in [
        ("use z::{Item}; fn accept(x:Item){}", vec!["a", "z"]),
        ("use z::{Item}; fn accept(x:Item){Item()}", vec!["z", "a"]),
        ("pub use z::{Item}; fn accept(x:Item){}", vec!["z", "a"]),
        ("use z::{Item};", vec!["z", "a"]),
        (
            "use z::{Item,make}; fn accept(x:Item){make()}",
            vec!["z", "a"],
        ),
    ] {
        let result = analyze_project(
            &[
                input("a", source),
                input("z", "pub class Item{} pub fn make(){Item()}"),
            ],
            &AnalysisOptions::default(),
            &mut NoModules,
        )
        .unwrap();
        assert!(result.is_ok(), "{:?}", result.diagnostics);
        assert_eq!(result.plan.initialization_order, expected, "{source}");
        assert_eq!(
            result.modules().unwrap().len(),
            2,
            "type sources remain linked"
        );
    }
}
#[test]
fn exported_destructuring_projects_each_binding_type() {
    let inputs = [
        input("a", "pub let (x,y):(int,int)=(1,2);"),
        input("b", "use a::{x}; let z:int=x;"),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}
#[test]
fn exported_builtin_categories_match_local_annotation_semantics() {
    let inputs = [
        input("a", "pub fn f(x:Map){} pub fn callback(x:closure){}"),
        input("b", "use a::{f,callback}; f({}); fn g(){} callback(g);"),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}
#[test]
fn reexport_dependencies_respect_explicit_symbol_disambiguation() {
    let inputs = [
        input("a", "pub use z::{item};"),
        input("z", "pub use src::{item};"),
        input("z::item", ""),
        input("src", "pub fn item(){}"),
        input("consumer", "use a::{item}; item();"),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    assert!(
        result
            .plan
            .initialization_order
            .iter()
            .position(|p| p == "z")
            .unwrap()
            < result
                .plan
                .initialization_order
                .iter()
                .position(|p| p == "a")
                .unwrap()
    );
}
#[test]
fn source_modules_link_public_signatures_reexports_and_namespaces() {
    let inputs = [
        input(
            "game::a",
            "let secret=7; pub fn add(mut x:int,y:int=1,...rest)->int{x+y}",
        ),
        input("game::b", "pub use game::a::{add as sum};"),
        input("game::c", "use game::b as api; let n=api.sum(1,y:2);"),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    assert_eq!(
        result.plan.initialization_order,
        ["game::a", "game::b", "game::c"]
    );
    let m = &result.modules().unwrap()[2];
    assert!(
        m.external_bindings()
            .iter()
            .filter_map(|e| e.descriptor.signature.as_ref())
            .any(|s| s.parameters[0].mutable && s.parameters[1].default && s.parameters[2].rest)
    );
    let invalid = [
        inputs[0].clone(),
        input("game::c", "use game::a::{secret};"),
    ];
    let result = analyze_project(&invalid, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(!result.is_ok());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.diagnostic.code == DiagnosticCode::ModuleResolution)
    );
    let invalid = [
        inputs[0].clone(),
        input("game::c", "use game::a::{add}; add(z:2);"),
    ];
    let result = analyze_project(&invalid, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(!result.is_ok());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.diagnostic.code == DiagnosticCode::InvalidCall)
    );
}
#[test]
fn declared_import_return_contracts_match_forward_local_calls() {
    for source in [
        "use a::{g}; fn f()->int{g()}",
        "use a::{g}; fn f(x:int=g()){}",
        "use a::{g}; fn f(){let x:int=g();}",
    ] {
        let inputs = [input("a", "pub fn g()->bool{true}"), input("b", source)];
        let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.diagnostic.code == DiagnosticCode::TypeMismatch),
            "{:?}",
            result.diagnostics
        );
    }
    let inputs = [
        input("a", "use b::{f}; pub fn g()->int{f()}"),
        input("b", "use a::{g}; pub fn f()->int{g()}"),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}
#[test]
fn function_scc_and_global_initialization_cycles_keep_runtime_checks() {
    let inputs = [
        input("game::b", "use game::a::{a}; pub fn b(){a()} pub let bv=1;"),
        input(
            "game::a",
            "use game::b::{b,bv}; pub fn a(){b()} pub let av=bv;",
        ),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    assert_eq!(result.plan.components, vec![vec!["game::a", "game::b"]]);
    assert!(
        result
            .modules()
            .unwrap()
            .iter()
            .flat_map(|m| m.facts())
            .any(|f| f.checks.0 & RuntimeChecks::INITIALIZED != 0)
    );
}
#[test]
fn wildcard_collisions_and_explicit_symbol_ambiguity() {
    let mut r = MemoryResolver::default();
    r.insert(ModuleInterface {
        canonical_path: "lib".into(),
        version: "v".into(),
        exports: vec![ExternalSymbol::dynamic("x"), ExternalSymbol::dynamic("m")],
    });
    r.insert(ModuleInterface {
        canonical_path: "lib::m".into(),
        version: "v".into(),
        exports: vec![],
    });
    let result =
        analyze_module("use lib::*; let x=1;", &AnalysisOptions::default(), &mut r).unwrap();
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::DuplicateBinding)
    );
    let result = analyze_module("use lib::m;", &AnalysisOptions::default(), &mut r).unwrap();
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::ModuleResolution)
    );
    let result = analyze_module("use lib::{m}; m();", &AnalysisOptions::default(), &mut r).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}
#[test]
fn recursive_namespace_reexports_are_references_not_recursively_copied_trees() {
    let inputs = [
        input("game::a", "pub use game::b as b; pub fn a(){}"),
        input("game::b", "pub use game::a as a; pub fn b(){}"),
        input("game::c", "use game::a as api; api.b.a.b.a.a();"),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
}
#[test]
fn public_destructuring_exports_only_its_own_names() {
    let inputs = [
        input(
            "game::a",
            "pub let {x}={x:1}; pub let callback=||{let hidden=1;hidden};",
        ),
        input("game::b", "use game::a::{x,callback}; callback();"),
    ];
    let r = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(r.is_ok(), "{:?}", r.diagnostics);
    let invalid = [
        inputs[0].clone(),
        input("game::b", "use game::a::{hidden};"),
    ];
    let r = analyze_project(&invalid, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.diagnostic.code == DiagnosticCode::ModuleResolution)
    );
}
#[test]
fn project_resource_budgets_are_total_across_modules() {
    let inputs = [input("a", "let a=1;"), input("b", "let b=1;")];
    let options = AnalysisOptions {
        limits: AnalysisLimits {
            max_symbols: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let r = analyze_project(&inputs, &options, &mut NoModules).unwrap();
    assert!(r.exhausted);
    assert!(!r.is_ok());
    let options = AnalysisOptions {
        limits: AnalysisLimits {
            max_total_source_bytes: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let r = analyze_project(&inputs, &options, &mut NoModules).unwrap();
    assert!(r.exhausted);
}
#[test]
fn qualified_native_or_class_types_follow_resolved_namespace_identity() {
    let inputs = [
        input("game::types", "pub class C{}"),
        input(
            "game::main",
            "use game::types as t; fn f(x:t::C){x} let c=t.C(); f(c);",
        ),
    ];
    let r = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(r.is_ok(), "{:?}", r.diagnostics);
}

#[test]
fn namespace_alias_and_reexports_retain_declaration_and_resolution_identity() {
    let inputs = [
        input("origin", "pub fn f(x=1)->int{x}"),
        input(
            "middle",
            "pub use origin::{f as forwarded}; pub use origin as original;",
        ),
        input(
            "facade",
            "pub use middle::{forwarded as renamed}; pub use middle::{original as namespace};",
        ),
        input(
            "user",
            "use facade as api; use facade::{renamed as local}; let a:int=api.renamed(); let b:int=local(); let c:int=api.namespace.f();",
        ),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let m = result
        .modules()
        .unwrap()
        .iter()
        .find(|m| m.canonical_path() == "user")
        .unwrap();
    let calls: Vec<_> = m
        .external_bindings()
        .iter()
        .filter(|b| b.descriptor.signature.is_some())
        .collect();
    assert!(calls.len() >= 3);
    for binding in calls {
        let origin = binding.descriptor.reference.as_ref().unwrap();
        assert_eq!(
            (&*origin.module, &*origin.version, origin.export.as_deref()),
            ("origin", "test-v1", Some("f"))
        );
        if binding.descriptor.name == "renamed" {
            let source = binding.descriptor.resolved_from.as_ref().unwrap();
            assert_eq!(
                (
                    &*source.reference.module,
                    source.reference.export.as_deref()
                ),
                ("facade", Some("renamed"))
            );
            let middle = source.previous.as_ref().unwrap();
            assert_eq!(
                (
                    &*middle.reference.module,
                    middle.reference.export.as_deref()
                ),
                ("middle", Some("forwarded"))
            );
        }
    }
    let ns = m
        .external_bindings()
        .iter()
        .find(|b| b.descriptor.name == "namespace")
        .unwrap();
    assert_eq!(ns.descriptor.reference.as_ref().unwrap().module, "origin");
    assert_eq!(ns.descriptor.reference.as_ref().unwrap().export, None);
    let source = ns.descriptor.resolved_from.as_ref().unwrap();
    assert_eq!(source.reference.export.as_deref(), Some("namespace"));
}

#[test]
fn injectable_resolver_identity_uses_returned_canonical_module() {
    struct Resolver;
    impl ModuleResolver for Resolver {
        fn resolve(&mut self, _: &ImportRequest) -> Result<ResolvedImport, ResolveError> {
            Ok(ResolvedImport {
                module: "canonical::actual".into(),
                version: "locked-42".into(),
                bindings: vec![ExternalSymbol::value("item", TypeHint::Int)],
            })
        }
    }
    let m = analyze_module(
        "use alias::{item as local}; let x:int=local;",
        &AnalysisOptions::default(),
        &mut Resolver,
    )
    .unwrap()
    .into_module()
    .unwrap();
    for binding in m.external_bindings() {
        let reference = binding.descriptor.reference.as_ref().unwrap();
        assert_eq!(
            (
                &*reference.module,
                &*reference.version,
                reference.export.as_deref()
            ),
            ("canonical::actual", "locked-42", Some("item"))
        );
    }
}

#[test]
fn direct_namespace_resolution_is_idempotent_and_has_no_fabricated_export() {
    let inputs = [
        input("a", "pub let x=1;"),
        input("b", "use a as ns; let z:int=ns.x;"),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let b = result
        .modules()
        .unwrap()
        .iter()
        .find(|m| m.canonical_path() == "b")
        .unwrap();
    let mut namespace_count = 0;
    for binding in b.external_bindings() {
        let descriptor = &binding.descriptor;
        let reference = descriptor.reference.as_ref().unwrap();
        assert_eq!((&*reference.module, &*reference.version), ("a", "test-v1"));
        let source = descriptor.resolved_from.as_ref().unwrap();
        if descriptor.namespace.is_some() {
            namespace_count += 1;
            assert_eq!(reference.export, None);
            assert_eq!(
                source.reference.export, None,
                "namespace must never invent export a"
            );
            assert!(
                source.previous.is_none(),
                "normalization must not add another resolution hop"
            );
        } else {
            assert_eq!(reference.export.as_deref(), Some("x"));
            assert_eq!(source.reference.export.as_deref(), Some("x"));
            assert!(source.previous.is_none());
        }
        assert!(std::sync::Arc::ptr_eq(reference, &source.reference));
    }
    assert!(namespace_count > 0);
}

#[test]
fn namespace_and_symbol_aliases_preserve_identity_through_a_legal_cycle() {
    let inputs = [
        input("origin", "pub let x=1;"),
        input("a", "pub use origin as ns; pub use b::{mirror as alias};"),
        input(
            "b",
            "pub use a::{ns as mirror}; pub use origin::{x as number};",
        ),
        input(
            "consumer",
            "use a::{alias as local}; use b::{number as n}; let x:int=local.x; let y:int=n;",
        ),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    assert!(result.plan.components.iter().any(|c| c == &["a", "b"]));
    let m = result
        .modules()
        .unwrap()
        .iter()
        .find(|m| m.canonical_path() == "consumer")
        .unwrap();
    let alias = m
        .external_bindings()
        .iter()
        .find(|b| b.descriptor.name == "alias")
        .unwrap();
    let declaration = alias.descriptor.reference.as_ref().unwrap();
    assert_eq!(
        (&*declaration.module, declaration.export.as_deref()),
        ("origin", None)
    );
    let expected = [
        ("a", Some("alias")),
        ("b", Some("mirror")),
        ("a", Some("ns")),
        ("origin", None),
    ];
    let mut source = alias.descriptor.resolved_from.as_deref();
    for (module, export) in expected {
        let hop = source.unwrap();
        assert_eq!(
            (&*hop.reference.module, hop.reference.export.as_deref()),
            (module, export)
        );
        source = hop.previous.as_deref();
    }
    assert!(source.is_none());
    let number = m
        .external_bindings()
        .iter()
        .find(|b| b.descriptor.name == "number")
        .unwrap();
    assert_eq!(
        number
            .descriptor
            .reference
            .as_ref()
            .unwrap()
            .export
            .as_deref(),
        Some("x")
    );
    let head = number.descriptor.resolved_from.as_ref().unwrap();
    assert_eq!(
        (&*head.reference.module, head.reference.export.as_deref()),
        ("b", Some("number"))
    );
    assert_eq!(
        head.previous.as_ref().unwrap().reference.export.as_deref(),
        Some("x")
    );
}

#[test]
fn namespace_reexport_of_its_own_module_keeps_the_actual_export_hop() {
    let inputs = [
        input("a", "pub let x=1; pub use a as ns;"),
        input("b", "use a::{ns as local}; let x:int=local.x;"),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let b = result
        .modules()
        .unwrap()
        .iter()
        .find(|m| m.canonical_path() == "b")
        .unwrap();
    let namespace = b
        .external_bindings()
        .iter()
        .find(|b| b.descriptor.name == "ns")
        .unwrap();
    let origin = namespace.descriptor.reference.as_ref().unwrap();
    assert_eq!((&*origin.module, origin.export.as_deref()), ("a", None));
    let head = namespace.descriptor.resolved_from.as_ref().unwrap();
    assert_eq!(
        (&*head.reference.module, head.reference.export.as_deref()),
        ("a", Some("ns"))
    );
    let previous = head.previous.as_ref().unwrap();
    assert_eq!(
        (
            &*previous.reference.module,
            previous.reference.export.as_deref()
        ),
        ("a", None)
    );
    assert!(previous.previous.is_none());
}

#[test]
fn canonical_references_follow_aliases_groups_and_shadowing() {
    let inputs = [
        input("origin", "pub fn f(){0}"),
        input(
            "user",
            "use origin as api; use origin::{f as named}; let alias=named; fn invoke(f){((api.f))(); alias(); f();}",
        ),
    ];
    let result = analyze_project(&inputs, &AnalysisOptions::default(), &mut NoModules).unwrap();
    assert!(result.is_ok(), "{:?}", result.diagnostics);
    let module = result
        .modules()
        .unwrap()
        .iter()
        .find(|module| module.canonical_path() == "user")
        .unwrap();
    let mut calls = 0;
    for node in &module.hir().nodes {
        let aside_compiler::hir::Form::Call { callee, .. } = node.form else {
            continue;
        };
        calls += 1;
        let expression = module.hir().text(module.hir().node(callee).span);
        match expression {
            "((api.f))" | "alias" => {
                let reference = module.canonical_reference(callee).unwrap();
                assert_eq!(
                    (
                        reference.module.as_str(),
                        reference.version.as_str(),
                        reference.export.as_deref()
                    ),
                    ("origin", "test-v1", Some("f"))
                );
            }
            "f" => assert!(module.canonical_reference(callee).is_none()),
            _ => panic!("unexpected source call: {expression}"),
        }
    }
    assert_eq!(calls, 3);
}

#[test]
fn dependency_discovery_projects_real_grouped_aliased_and_nested_uses() {
    let source = r#"// Imports remain source-located after leading trivia.
        pub use root::{types::{Entry as Renamed}, actions::*, empty::{}};
        use root::api as api;
        fn nested(){use nested::{item as local};}
        let text = "use pretend::dependency;";
    "#;
    let found = discover_imports(source, "current", AnalysisLimits::default()).unwrap();
    assert!(found.diagnostics.is_empty(), "{:?}", found.diagnostics);
    let requests = found
        .requests
        .iter()
        .map(|(request, _)| (request.from.as_str(), request.path.join("::"), request.kind))
        .collect::<Vec<_>>();
    assert_eq!(
        requests,
        [
            ("current", "root::types::Entry".into(), ImportKind::Symbol),
            ("current", "root::actions".into(), ImportKind::Wildcard),
            ("current", "root::api".into(), ImportKind::NamespaceOrSymbol),
            ("current", "nested::item".into(), ImportKind::Symbol),
        ]
    );
    for (_, range) in &found.requests {
        let declaration = &source[*range];
        assert!(
            declaration.starts_with("use ") && declaration.ends_with(';'),
            "{declaration}"
        );
    }
    let invalid = "use root::api; fn broken(){let = 1;}";
    let parsed = parse(invalid).unwrap();
    assert!(!parsed.is_ok());
    let found = discover_imports(invalid, "current", AnalysisLimits::default()).unwrap();
    assert!(found.requests.is_empty());
    assert_eq!(found.diagnostics, parsed.diagnostics());
}

#[test]
fn import_projection_does_not_replace_complete_module_admission() {
    let source = "use remote::value; fn main(){let a=(1,2,3,4,5,6); a}";
    let limits = AnalysisLimits {
        max_nodes: 8,
        ..Default::default()
    };
    let found = discover_imports(source, "current", limits).unwrap();
    assert!(found.diagnostics.is_empty());
    assert_eq!(found.requests.len(), 1);
    let analysis = analyze_module(
        source,
        &AnalysisOptions {
            limits,
            ..Default::default()
        },
        &mut NoModules,
    )
    .unwrap();
    assert!(analysis.exhausted);
    assert_eq!(analysis.diagnostics[0].code, DiagnosticCode::BudgetExceeded);
}
