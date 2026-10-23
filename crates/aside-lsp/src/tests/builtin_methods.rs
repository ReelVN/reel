use super::*;

#[test]
fn generators_keep_item_types_and_navigable_method_declarations() {
    let (_project, root) = project_sources();
    let declarations = r#"pub class Item { fn label(self)->string { "Item" } }
/// Produce items on demand.
pub gen fn items() -> int { yield Item(); return 7; }
"#;
    std::fs::write(root.join("items.aside"), declarations).unwrap();
    let source = r#"use game::items::items;
fn main() {
    let pending = items();
    let (done, value) = pending.next();
    done; value;
    pending.close();
    for item in items() { let caption = item.label(); caption; }
}
"#;
    let main = file_uri(&root.join("main.aside")).unwrap();
    let mut workspace = Workspace::new(project_options());
    workspace.open(main.clone(), 1, source.into()).unwrap();
    let diagnostics = workspace.diagnostics(&main, Encoding::Utf16).unwrap();
    assert!(
        diagnostics.iter().all(|entry| entry.diagnostics.is_empty()),
        "{diagnostics:?}"
    );
    for (needle, expected) in [
        ("items();", "gen fn items() -> int"),
        ("pending.next", "pending: generator"),
        ("done;", "done: bool"),
        ("caption;", "caption: string"),
    ] {
        let hover = hover_text(
            workspace
                .hover(&main, position(source, needle, false), Encoding::Utf16)
                .unwrap()
                .unwrap(),
        );
        assert!(hover.contains(expected), "{needle}: {hover}");
    }
    let value = hover_text(
        workspace
            .hover(&main, position(source, "value;", false), Encoding::Utf16)
            .unwrap()
            .unwrap(),
    );
    for ty in ["Item", "int", "null"] {
        assert!(value.contains(ty), "{value}");
    }
    assert!(!value.contains("any"), "{value}");
    let next = hover_text(
        workspace
            .hover(&main, position(source, "next();", false), Encoding::Utf16)
            .unwrap()
            .unwrap(),
    );
    for ty in ["bool", "Item", "int", "null"] {
        assert!(next.contains(ty), "{next}");
    }
    assert!(!next.contains("any"), "{next}");
    let members = workspace
        .complete(&main, position(source, "pending.", true), Encoding::Utf16)
        .unwrap();
    for name in ["next", "close"] {
        let item = members.iter().find(|item| item.label == name).unwrap();
        assert_eq!(item.kind, Some(CompletionItemKind::METHOD));
        assert!(item.documentation.is_some());
        let definition = workspace
            .definition(
                &main,
                position(source, &format!("{name}();"), false),
                Encoding::Utf16,
            )
            .unwrap()
            .unwrap();
        assert_eq!(definition.uri.scheme().unwrap().as_str(), "aside-native");
        let document = workspace
            .host
            .documents
            .get(definition.uri.as_str())
            .unwrap();
        let lines = LineIndex::new(&document.text);
        let start = lines
            .offset(&document.text, definition.range.start, Encoding::Utf16)
            .unwrap();
        let end = lines
            .offset(&document.text, definition.range.end, Encoding::Utf16)
            .unwrap();
        assert_eq!(&document.text[start..end], name);
    }
    let method = workspace
        .definition(&main, position(source, "label();", false), Encoding::Utf16)
        .unwrap()
        .unwrap();
    assert_eq!(method.uri, file_uri(&root.join("items.aside")).unwrap());
    assert_eq!(
        method.range.start,
        position(declarations, "label(self)", false)
    );
    assert_eq!(workspace.analyses, 1);
}

#[test]
fn primitive_methods_share_completion_signature_hover_and_readonly_definitions() {
    for (annotation, initializer) in [
        ("array", "[1,2]"),
        ("tuple", "(1,2)"),
        ("(int,int)", "(1,2)"),
    ] {
        let source = format!(
            "fn main(){{let values:{annotation}={initializer};let find=values.find; find(value:2);values.contains(2);values. }}"
        );
        let mut workspace = Workspace::new(ServerOptions::default());
        workspace.open(uri(), 1, source.clone()).unwrap();
        let completion = workspace
            .complete(&uri(), position(&source, "values. ", true), Encoding::Utf16)
            .unwrap();
        for name in ["find", "contains", "slice", "len"] {
            let item = completion
                .iter()
                .find(|item| item.label == name)
                .unwrap_or_else(|| panic!("missing {name}: {annotation}"));
            assert_eq!(item.kind, Some(CompletionItemKind::METHOD));
            assert!(item.documentation.is_some());
        }
        if annotation != "array" {
            assert!(!completion.iter().any(|item| item.label == "append"));
        }
        let hover = hover_text(
            workspace
                .hover(&uri(), position(&source, "find;", false), Encoding::Utf16)
                .unwrap()
                .unwrap(),
        );
        assert!(
            hover.contains("fn find(value: any) -> int | null"),
            "{hover}"
        );
        assert!(hover.contains("first index"), "{hover}");
        let signature = workspace
            .signature(&uri(), position(&source, "value:2", true), Encoding::Utf16)
            .unwrap()
            .unwrap();
        assert!(signature.signatures[0].label.contains("value: any"));
        let definition = workspace
            .definition(&uri(), position(&source, "find;", false), Encoding::Utf16)
            .unwrap()
            .unwrap();
        assert_eq!(definition.uri.scheme().unwrap().as_str(), "aside-native");
        let document = workspace
            .host
            .documents
            .get(definition.uri.as_str())
            .unwrap();
        let lines = LineIndex::new(&document.text);
        let start = lines
            .offset(&document.text, definition.range.start, Encoding::Utf16)
            .unwrap();
        let end = lines
            .offset(&document.text, definition.range.end, Encoding::Utf16)
            .unwrap();
        assert_eq!(&document.text[start..end], "find");
        assert_eq!(workspace.analyses, 1, "all queries share recoverable facts");
    }
}

#[test]
fn builtin_calls_keep_diagnostics_and_facts_after_a_missing_argument() {
    let source = "fn main(values:array){values.find();let ok=values.contains(value:1);ok;}";
    let mut workspace = Workspace::new(ServerOptions::default());
    workspace.open(uri(), 1, source.into()).unwrap();
    assert!(
        workspace
            .diagnostics(&uri(), Encoding::Utf16)
            .unwrap()
            .iter()
            .any(|p| !p.diagnostics.is_empty())
    );
    let hover = hover_text(
        workspace
            .hover(&uri(), position(source, "ok;", false), Encoding::Utf16)
            .unwrap()
            .unwrap(),
    );
    assert!(hover.contains("bool"), "{hover}");
    assert!(
        workspace
            .definition(&uri(), position(source, "contains", false), Encoding::Utf16)
            .unwrap()
            .is_some()
    );
}
