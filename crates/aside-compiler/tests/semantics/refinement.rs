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
                .rfind(|(_, n)| n.kind == HirKind::Name && module.hir().text(n.span) == "value")
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

fn distinct_guards(length: usize) -> String {
    use std::fmt::Write;
    let mut source = String::from("fn take(value:int)->bool {true} fn f(){");
    for index in 0..length {
        write!(source, "let value{index}:any={index};").unwrap();
    }
    source.push_str("if ");
    for index in 0..length {
        if index != 0 {
            source.push_str(" && ");
        }
        write!(source, "value{index} is int && take(value{index})").unwrap();
    }
    write!(
        source,
        "{{let first=value0; let last=value{};}} let outside=value0;}}",
        length - 1
    )
    .unwrap();
    source
}

#[test]
fn distinct_short_circuit_guards_keep_all_names_and_restore_the_outer_path() {
    let mut work = Vec::new();
    for length in [256, 512] {
        let analysis = analyze_code(&distinct_guards(length));
        assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
        work.push(analysis.stats.work_units);
        let module = analysis.into_module().unwrap();
        for name in ["first", "last"] {
            let binding = symbols(&module, name)[0].1;
            assert_eq!(module.display_type(binding.ty).to_string(), "int");
        }
        let outside = symbols(&module, "outside")[0].1;
        assert_eq!(module.display_type(outside.ty).to_string(), "any");
    }
    assert!(work[1] <= work[0] * 3, "{work:?}");
}

#[test]
fn nested_guard_scopes_restore_prior_facts_and_preserve_stable_captures() {
    let module = valid(
        r#"
        class Base {}
        class Child:Base {}
        fn f(value:any, other:any) {
            if value is Base {
                if value is Child {
                    let captured=||{let child=value;};
                }
                let parent=value;
                if other is int {let sibling=value;}
                let later=||{let captured_parent=value;};
            } else {let negative=value;}
            let outside=value;
            let mixed=(value is Child && true) || (other is int && true);
            let after_mixed=value;
        }
        "#,
    );
    for (name, expected) in [
        ("child", "Child"),
        ("parent", "Base"),
        ("sibling", "Base"),
        ("captured_parent", "Base"),
        ("negative", "any"),
        ("outside", "any"),
        ("after_mixed", "any"),
    ] {
        let binding = symbols(&module, name)[0].1;
        assert_eq!(
            module.display_type(binding.ty).to_string(),
            expected,
            "{name}"
        );
    }
}

#[test]
#[ignore = "reports host compiler cost for distinct-name guard chains"]
fn measure_distinct_guard_chain_analysis() {
    for length in [128, 256, 512, 1024] {
        let source = distinct_guards(length);
        assert!(analyze_code(&source).is_ok());
        let mut samples = Vec::new();
        for _ in 0..21 {
            let start = std::time::Instant::now();
            let analysis = analyze_code(std::hint::black_box(&source));
            samples.push(start.elapsed().as_nanos());
            assert!(analysis.is_ok(), "{:?}", analysis.diagnostics);
        }
        samples.sort_unstable();
        println!(
            "guards={length} median_analysis_ns={}",
            samples[samples.len() / 2]
        );
    }
}
