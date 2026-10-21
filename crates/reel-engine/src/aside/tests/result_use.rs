use super::*;

#[test]
fn discarded_node_results_report_the_author_constructor_location() {
    for (code, expression) in [
        (
            r#"fn heading(){text("heading");} fn opening(){stage.replace(column {heading();});}"#,
            r#"text("heading")"#,
        ),
        (
            r#"use reel::text as label; fn opening(){label("heading");}"#,
            r#"label("heading")"#,
        ),
        (
            r#"fn opening(){let label=text; label("heading");}"#,
            r#"label("heading")"#,
        ),
        (
            r#"fn opening(){column {text("child");} narrator("ready");}"#,
            r#"column {text("child");}"#,
        ),
    ] {
        let error = source::compile_story(code, "components", 1, "opening")
            .err()
            .expect("discarded construction must fail");
        let source::SourceError::Analysis(failure) = error else {
            panic!("{error:?}");
        };
        assert_eq!(failure.diagnostics.len(), 1, "{:?}", failure.diagnostics);
        let diagnostic = &failure.diagnostics[0];
        assert_eq!(
            diagnostic.code,
            aside_compiler::DiagnosticCode::UnusedResult
        );
        assert_eq!(
            &code[aside_compiler::byte_range(diagnostic.range)],
            expression
        );
    }
}

#[test]
fn component_tail_values_optional_nodes_and_children_execute_after_bytecode_roundtrip() {
    let code = r#"
        fn heading(value) {text(value)}
        fn optional(show) {
            if !show {return null;}
            text("optional")
        }
        fn attach(parent) {parent.add(text("attached"));}
        fn opening() {
            let _=text("explicit discard");
            let panel=column {
                heading("heading");
                optional(false);
                optional(true);
                if true {text("condition");}
                for label in ["first", "second"] {text(label);}
                match 1 {1=>text("match"),_=>null}
            };
            attach(panel);
            stage.replace(panel);
        }
    "#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, true);
        let mut text = vm
            .stage()
            .nodes()
            .filter(|node| node.parent == vm.stage().root())
            .filter_map(|node| match &node.kind {
                reel_ui::stage::NodeKind::Text(value) => Some(value.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>();
        text.sort();
        assert_eq!(
            text,
            [
                "attached",
                "condition",
                "first",
                "heading",
                "match",
                "optional",
                "second"
            ],
            "encoded={encoded}"
        );
        vm.finish(&mut session).unwrap();
    }
}

#[test]
fn public_stage_example_uses_returned_and_optional_components() {
    source::compile_story(
        include_str!("../../../../../examples/author_stage/src/main.aside"),
        "author_stage",
        1,
        "main",
    )
    .unwrap();
}
