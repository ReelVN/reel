use super::*;

#[test]
fn is_guards_refine_stable_names_through_branches_short_circuit_and_closures() {
    let source = r#"
        fn take(value:int)->int {value}
        fn guarded(value:int|string) {
            if value is int {
                let captured=||take(value);
                take(value);
            } else {
                let text:string=value;
            }
            if !(value is int) { let text:string=value; }
            else { take(value); }
            if (value is int) && take(value)>0 {take(value);}
            if value is string || take(value)>0 {} else {take(value);}
            while value is int {take(value);break;}
        }
    "#;
    let module = valid(source);
    let captured = source.find("||take(value)").unwrap() + "||take(".len();
    let (id, _) = module
        .hir()
        .nodes
        .iter()
        .enumerate()
        .find(|(_, node)| {
            node.kind == HirKind::Name && u32::from(node.span.start()) as usize == captured
        })
        .unwrap();
    assert_eq!(
        module.display_type(module.facts()[id].ty).to_string(),
        "int"
    );
    let reads: Vec<_> = module
        .hir()
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == HirKind::Name && module.hir().text(node.span) == "value")
        .map(|(i, _)| module.display_type(module.facts()[i].ty).to_string())
        .collect();
    assert!(reads.iter().any(|ty| ty == "int"), "{reads:?}");
    assert!(reads.iter().any(|ty| ty == "string"), "{reads:?}");
    assert!(reads.iter().any(|ty| ty == "int | string"), "{reads:?}");
}

#[test]
fn guard_facts_do_not_escape_paths_or_follow_rebindable_names() {
    for code in [
        "fn f(value:int|string){if value is int {let bad:string=value;}}",
        "fn f(value:int|string){if value is int {let value=\"shadow\";let bad:int=value;}}",
        "fn f(value:int|string){if value is int {} let outside:string=value;}",
    ] {
        // Union compatibility is checked dynamically, so inspect the outside
        // fact separately rather than demanding a stronger union policy here.
        if code.contains("outside") {
            let module = valid(code);
            let (id, _) = module
                .hir()
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| n.kind == HirKind::Name && module.hir().text(n.span) == "value")
                .next_back()
                .unwrap();
            assert_eq!(
                module.display_type(module.facts()[id].ty).to_string(),
                "int | string"
            );
        } else {
            rejects(code, DiagnosticCode::TypeMismatch);
        }
    }
    let module =
        valid("fn f(mut value:int|string){if value is int {value=\"changed\";let kept=value;}}");
    let binding = symbols(&module, "kept")[0].1;
    assert_eq!(module.display_type(binding.ty).to_string(), "int | string");
}

#[test]
fn long_short_circuit_guards_accumulate_linear_semantic_work() {
    let mut work = Vec::new();
    for length in [256, 512] {
        let source = format!(
            "fn f(value){{if value is int {} {{let result:int=value;}}}}",
            "&& true ".repeat(length)
        );
        let analysis = analyze_code(&source);
        assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
        work.push(analysis.stats.work_units);
    }
    assert!(work[1] <= work[0] * 3, "{work:?}");
}
