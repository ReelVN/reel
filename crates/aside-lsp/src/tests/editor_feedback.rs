use super::*;

#[test]
fn hover_selects_import_identifiers_and_preserves_source_declarations() {
    let (_project, root) = project_sources();
    let declarations = "/// A documented counter.\npub class Counter {fn init(self, start:int=4){}}\n/// A documented greeting.\npub fn greet(name:string=\"World\")->string {name}\n/// A documented value.\npub let answer:int=42;\n/// A narrator value.\npub let 旁白:string=\"Narrator\";";
    std::fs::write(root.join("declarations.aside"), declarations).unwrap();
    let source = "use game::declarations::{Counter as Count, greet as hello, answer as result, 旁白};\nuse game::declarations as story;\nfn main(){let counter=Count(); hello(); result; 旁白; story.greet();}";
    let main = file_uri(&root.join("main.aside")).unwrap();
    let mut workspace = Workspace::new(project_options());
    workspace.open(main.clone(), 1, source.into()).unwrap();
    for needle in [
        "use game",
        "game::declarations",
        "declarations::{",
        "as Count",
        "fn main",
        "let counter",
    ] {
        let offset = source.find(needle).unwrap();
        let position = LineIndex::new(source)
            .position(source, offset, Encoding::Utf16)
            .unwrap();
        assert!(
            workspace
                .hover(&main, position, Encoding::Utf16)
                .unwrap()
                .is_none(),
            "unexpected symbol at {needle}"
        );
    }
    for (needle, label, docs) in [
        ("Counter as", "class Counter", "A documented counter."),
        ("Count,", "class Counter", "A documented counter."),
        (
            "greet as",
            "fn greet(name: string = \"World\") -> string",
            "A documented greeting.",
        ),
        ("answer as", "let answer: int", "A documented value."),
        ("旁白};", "let 旁白: string", "A narrator value."),
    ] {
        let offset = source.find(needle).unwrap();
        let start = LineIndex::new(source)
            .position(source, offset, Encoding::Utf16)
            .unwrap();
        let hover = workspace
            .hover(&main, start, Encoding::Utf16)
            .unwrap()
            .unwrap();
        let token = needle.split([' ', ',', '}']).next().unwrap();
        let end = LineIndex::new(source)
            .position(source, offset + token.len(), Encoding::Utf16)
            .unwrap();
        assert_eq!(hover.range, Some(Range::new(start, end)), "{needle}");
        let text = hover_text(hover);
        assert!(text.contains(label), "{needle}: {text}");
        assert!(text.contains(docs), "{needle}: {text}");
    }
    let namespace = hover_text(
        workspace
            .hover(&main, position(source, "story;", false), Encoding::Utf16)
            .unwrap()
            .unwrap(),
    );
    assert!(
        namespace.contains("use game::declarations as story"),
        "{namespace}"
    );
    let definition = workspace
        .definition(&main, position(source, "story;", false), Encoding::Utf16)
        .unwrap()
        .unwrap();
    assert_eq!(
        definition.uri,
        file_uri(&root.join("declarations.aside")).unwrap()
    );
    assert_eq!(
        definition.range,
        Range::new(Position::default(), Position::default())
    );
    for (needle, original) in [
        ("Count,", "Counter"),
        ("hello,", "greet"),
        ("result,", "answer"),
    ] {
        let location = workspace
            .definition(&main, position(source, needle, false), Encoding::Utf16)
            .unwrap()
            .unwrap();
        assert_eq!(
            location.uri,
            file_uri(&root.join("declarations.aside")).unwrap()
        );
        assert_eq!(
            location.range.start,
            position(declarations, original, false)
        );
        assert_eq!(location.range.end, position(declarations, original, true));
    }
    let items = workspace
        .complete(&main, position(source, "result;", false), Encoding::Utf16)
        .unwrap();
    for (name, kind) in [
        ("Count", CompletionItemKind::CLASS),
        ("hello", CompletionItemKind::FUNCTION),
        ("result", CompletionItemKind::VARIABLE),
        ("story", CompletionItemKind::MODULE),
    ] {
        let item = items
            .iter()
            .find(|item| item.label == name)
            .unwrap_or_else(|| panic!("missing {name}: {items:?}"));
        assert_eq!(item.kind, Some(kind), "{name}");
    }
    let items = workspace
        .complete(
            &main,
            position(source, "story.greet", true),
            Encoding::Utf16,
        )
        .unwrap();
    let class = items.iter().find(|item| item.label == "Counter").unwrap();
    assert_eq!(class.kind, Some(CompletionItemKind::CLASS));
    assert!(class.detail.as_deref().unwrap().contains("class Counter"));
    assert!(
        matches!(&class.documentation, Some(Documentation::String(text)) if text.contains("A documented counter."))
    );
}

#[test]
fn keyword_completion_recovers_incomplete_code_and_filters_syntax_context() {
    for source in [
        "",
        "cla",
        "class",
        "fn main(){\n    cla",
        "class Example {\n    fn work(self) { cla",
    ] {
        let mut workspace = Workspace::new(ServerOptions::default());
        workspace.open(uri(), 1, source.into()).unwrap();
        let end = LineIndex::new(source)
            .position(source, source.len(), Encoding::Utf16)
            .unwrap();
        let items = workspace.complete(&uri(), end, Encoding::Utf16).unwrap();
        assert!(items.iter().any(|item| item.label == "class" && item.kind == Some(CompletionItemKind::KEYWORD)), "{source:?}: {items:?}");
    }
    for (source, needle) in [
        ("// cla", "cla"),
        ("/* cla */", "cla"),
        ("let text=\"cla\";", "cla"),
        ("let text=r\"cla\";", "cla"),
        ("let text=\"\"\"cla\"\"\";", "cla"),
        ("fn main(){let object={field:1}; object.cla}", "cla"),
        ("fn main(){unknown?.cla}", "cla"),
        ("use game::cla", "cla"),
    ] {
        let mut workspace = Workspace::new(ServerOptions::default());
        workspace.open(uri(), 1, source.into()).unwrap();
        let items = workspace
            .complete(&uri(), position(source, needle, true), Encoding::Utf16)
            .unwrap();
        assert!(
            !items
                .iter()
                .any(|item| item.kind == Some(CompletionItemKind::KEYWORD)),
            "{source:?}: {items:?}"
        );
    }
    let source = "fn main(){let text=f\"value {cla}\";}";
    let mut workspace = Workspace::new(ServerOptions::default());
    workspace.open(uri(), 1, source.into()).unwrap();
    assert!(
        workspace
            .complete(&uri(), position(source, "cla", true), Encoding::Utf16)
            .unwrap()
            .iter()
            .any(|item| item.label == "class")
    );
}

#[test]
fn incomplete_sources_keep_host_globals_and_lexical_shadowing() {
    for source in [
        "",
        "tex",
        "fn main(){tex",
        "fn main(){\n    tex\n}",
        "class Scene {fn show(self){tex",
    ] {
        let mut workspace = Workspace::new(ServerOptions {
            host: HostContext {
                globals: vec![ExternalSymbol::function(
                    "text",
                    signature(
                        vec![parameter("value", TypeHint::String, false)],
                        TypeHint::String,
                    ),
                )],
                ..Default::default()
            },
            ..Default::default()
        });
        workspace.open(uri(), 1, source.into()).unwrap();
        let offset = if source.contains("tex") {
            source.rfind("tex").unwrap() + 3
        } else {
            source.len()
        };
        let cursor = LineIndex::new(source)
            .position(source, offset, Encoding::Utf16)
            .unwrap();
        let items = workspace.complete(&uri(), cursor, Encoding::Utf16).unwrap();
        assert!(items.iter().any(|item| item.label == "text" && item.kind == Some(CompletionItemKind::FUNCTION)), "{source:?}: {items:?}");
    }
    let source = "fn main(){let text=\"local\"; tex}";
    let mut workspace = Workspace::new(ServerOptions {
        host: HostContext {
            globals: vec![ExternalSymbol::function(
                "text",
                signature(Vec::new(), TypeHint::String),
            )],
            ..Default::default()
        },
        ..Default::default()
    });
    workspace.open(uri(), 1, source.into()).unwrap();
    let items = workspace
        .complete(&uri(), position(source, "tex}", false), Encoding::Utf16)
        .unwrap();
    let text: Vec<_> = items.iter().filter(|item| item.label == "text").collect();
    assert_eq!(text.len(), 1);
    assert_eq!(text[0].kind, Some(CompletionItemKind::VARIABLE));
    assert_eq!(text[0].detail.as_deref(), Some("string"));
}

#[test]
fn imports_complete_discovered_modules_and_real_export_metadata() {
    let (_project, root) = project_sources();
    std::fs::write(root.join("helpers.aside"), "/// An exported actor.\npub class Actor {}\n/// A greeting.\npub fn greet()->string {\"hello\"}\npub let count:int=1;").unwrap();
    std::fs::create_dir(root.join("ui")).unwrap();
    std::fs::write(
        root.join("ui").join("buttons.aside"),
        "pub let button:int=1;",
    )
    .unwrap();
    let main = file_uri(&root.join("main.aside")).unwrap();
    let unsaved = file_uri(&root.join("new_module.aside")).unwrap();
    let mut workspace = Workspace::new(project_options());
    workspace
        .open(unsaved, 1, "pub let value=1;".into())
        .unwrap();
    for (source, expected) in [
        ("use ", vec!["game"]),
        ("use game::hel", vec!["helpers", "ui", "new_module"]),
        ("use game::ui::but", vec!["buttons"]),
        ("use game::helpers::{Act", vec!["Actor", "greet", "count"]),
    ] {
        workspace.open(main.clone(), 1, source.into()).unwrap();
        let cursor = LineIndex::new(source)
            .position(source, source.len(), Encoding::Utf16)
            .unwrap();
        let items = workspace.complete(&main, cursor, Encoding::Utf16).unwrap();
        for label in expected {
            assert!(
                items.iter().any(|item| item.label == label),
                "{source:?}, missing {label}: {items:?}"
            );
        }
        assert!(
            items
                .iter()
                .all(|item| item.kind != Some(CompletionItemKind::KEYWORD)),
            "{source:?}: {items:?}"
        );
    }
}

#[test]
fn type_and_named_argument_completion_keep_their_language_boundaries() {
    let (_project, root) = project_sources();
    std::fs::write(
        root.join("models.aside"),
        "pub class Actor {} pub fn greet(){} pub let value=1;",
    )
    .unwrap();
    let main = file_uri(&root.join("main.aside")).unwrap();
    let mut workspace = Workspace::new(project_options());
    for source in [
        "use game::models::{Actor}; class Local {} let value=1; fn main(){let actor: ",
        "use game::models::{Actor}; class Local {} let value=1; fn main(){let actor: Act",
    ] {
        workspace.open(main.clone(), 1, source.into()).unwrap();
        let cursor = LineIndex::new(source)
            .position(source, source.len(), Encoding::Utf16)
            .unwrap();
        let items = workspace.complete(&main, cursor, Encoding::Utf16).unwrap();
        for name in ["int", "string", "Map", "Future", "Actor", "Local", "fn"] {
            assert!(
                items.iter().any(|item| item.label == name),
                "missing {name} in {source:?}: {items:?}"
            );
        }
        assert!(
            !items
                .iter()
                .any(|item| ["value", "main", "return", "let"].contains(&item.label.as_str())),
            "{source:?}: {items:?}"
        );
    }
    let source = "use game::models as types; fn main(){let actor: types::";
    workspace.open(main.clone(), 1, source.into()).unwrap();
    let cursor = LineIndex::new(source)
        .position(source, source.len(), Encoding::Utf16)
        .unwrap();
    let items = workspace.complete(&main, cursor, Encoding::Utf16).unwrap();
    assert!(items.iter().any(|item| item.label == "Actor"), "{items:?}");
    assert!(
        !items
            .iter()
            .any(|item| ["greet", "value", "int"].contains(&item.label.as_str())),
        "{items:?}"
    );
    let source = "use game::models as types; fn show(actor:types::Actor){}";
    workspace.open(main.clone(), 1, source.into()).unwrap();
    let hover = hover_text(
        workspace
            .hover(&main, position(source, "Actor", false), Encoding::Utf16)
            .unwrap()
            .unwrap(),
    );
    assert!(hover.contains("class Actor"), "{hover}");
    let hover = hover_text(
        workspace
            .hover(
                &main,
                position(source, "types::Actor", false),
                Encoding::Utf16,
            )
            .unwrap()
            .unwrap(),
    );
    assert!(hover.contains("use game::models as types"), "{hover}");

    let source = "fn show(value:string,width:int=1,...rest){} fn main(){show(val);}";
    workspace.open(main.clone(), 1, source.into()).unwrap();
    let items = workspace
        .complete(&main, position(source, "val", true), Encoding::Utf16)
        .unwrap();
    assert!(items.iter().any(|item| item.label == "value:"), "{items:?}");
    assert!(items.iter().any(|item| item.label == "width:"), "{items:?}");
    assert!(!items.iter().any(|item| item.label == "rest:"), "{items:?}");
}

#[test]
fn keyword_candidates_follow_declarations_expressions_and_control_flow() {
    for (source, required, absent) in [
        ("let ", vec!["mut"], vec!["class", "return"]),
        ("pub ", vec!["class", "fn", "use"], vec!["return", "mut"]),
        (
            "class Actor { ",
            vec!["static", "let", "fn"],
            vec!["return", "pub"],
        ),
        (
            "fn main(){let value= ",
            vec!["class", "if", "null"],
            vec!["pub", "let", "return"],
        ),
        (
            "fn main(){loop { ",
            vec!["break", "continue", "return"],
            vec!["static"],
        ),
        (
            "fn main(){if true {} ",
            vec!["else"],
            vec!["catch", "finally"],
        ),
        ("fn main(){try {} ", vec!["catch", "finally"], vec!["else"]),
    ] {
        let mut workspace = Workspace::new(ServerOptions::default());
        workspace.open(uri(), 1, source.into()).unwrap();
        let cursor = LineIndex::new(source)
            .position(source, source.len(), Encoding::Utf16)
            .unwrap();
        let items = workspace.complete(&uri(), cursor, Encoding::Utf16).unwrap();
        for name in required {
            assert!(
                items.iter().any(|item| item.label == name),
                "{source:?}, missing {name}: {items:?}"
            );
        }
        for name in absent {
            assert!(
                !items.iter().any(|item| item.label == name),
                "{source:?}, unexpected {name}: {items:?}"
            );
        }
    }
}

#[test]
fn hover_summarizes_only_optional_schema_properties_and_uses_verified_type_aliases() {
    let canonical = "native::types::Node";
    let type_reference = SymbolReference {
        module: "native::types".into(),
        version: "1".into(),
        export: Some("Node".into()),
    };
    let function_reference = SymbolReference {
        module: "native".into(),
        version: "1".into(),
        export: Some("text".into()),
    };
    let mut node = ExternalSymbol::value("Node", TypeHint::Named(canonical.into()))
        .with_reference(type_reference.clone());
    node.type_only = true;
    let properties: Vec<_> = (0..60).map(|index| format!("property_{index}")).collect();
    let mut parameters = vec![
        parameter("value", TypeHint::String, false),
        parameter(
            "owner",
            TypeHint::Union(vec![TypeHint::Named(canonical.into()), TypeHint::Null]),
            false,
        ),
        parameter("source_identity_marker_value", TypeHint::String, true),
    ];
    parameters.extend(
        properties
            .iter()
            .map(|name| parameter(name, TypeHint::Int, true)),
    );
    let mut text = ExternalSymbol::function(
        "text",
        signature(parameters, TypeHint::Named(canonical.into())),
    )
    .with_reference(function_reference.clone());
    text.namespace = Some(NamespaceRef {
        module: "native::text".into(),
        version: "1".into(),
    });
    let context = HostContext {
        globals: vec![node.clone(), text],
        modules: vec![ModuleInterface {
            canonical_path: "native::types".into(),
            version: "1".into(),
            exports: vec![node],
        }],
        declarations: vec![
            HostDeclaration {
                reference: type_reference,
                signature: None,
                markdown: "A node.".into(),
                definition: None,
                parameter_defaults: BTreeMap::new(),
                property_parameters: Vec::new(),
                property_default: None,
            },
            HostDeclaration {
                reference: function_reference,
                signature: None,
                markdown: "Creates a text node.".into(),
                definition: None,
                parameter_defaults: BTreeMap::from([(
                    "source_identity_marker_value".into(),
                    crate::ParameterDefault::Value("\"native::types::Node\"".into()),
                )])
                .into_iter()
                .chain(
                    properties
                        .iter()
                        .cloned()
                        .map(|name| (name, crate::ParameterDefault::Omitted)),
                )
                .collect(),
                property_parameters: std::iter::once("value".into()).chain(properties).collect(),
                property_default: None,
            },
        ],
        ..Default::default()
    };
    for (source, expected_type) in [
        ("fn main(){text(\"Hello\");}", "Node"),
        ("fn main(){let Node=1; text(\"Hello\");}", canonical),
        (
            "use native::types::Node as Handle; class Node {} fn main(){text(\"Hello\");}",
            "Handle",
        ),
    ] {
        let mut workspace = Workspace::new(ServerOptions {
            host: context.clone(),
            ..Default::default()
        });
        workspace.open(uri(), 1, source.into()).unwrap();
        let hover = hover_text(
            workspace
                .hover(&uri(), position(source, "text(", false), Encoding::Utf16)
                .unwrap()
                .unwrap(),
        );
        assert!(hover.contains("fn text(\n"), "{hover}");
        assert!(
            hover.contains("value: string"),
            "required schema parameter: {hover}"
        );
        assert!(
            hover.contains(&format!("owner: {expected_type} | null")),
            "{hover}"
        );
        assert!(
            hover.contains(&format!(") -> {expected_type}\n")),
            "{hover}"
        );
        assert!(
            hover.contains("source_identity_marker_value: string = \"native::types::Node\""),
            "literal defaults retain source spelling: {hover}"
        );
        assert!(
            hover.contains("Additional named properties: 60."),
            "{hover}"
        );
        assert!(!hover.contains("property_59"), "{hover}");
        let help = workspace
            .signature(&uri(), position(source, "\"Hello\"", true), Encoding::Utf16)
            .unwrap()
            .unwrap();
        assert!(help.signatures[0].label.contains("property_59?: int"));
        assert!(help.signatures[0].label.contains(canonical));
        assert_eq!(help.signatures[0].parameters.as_ref().unwrap().len(), 63);
        let items = workspace
            .complete(&uri(), position(source, "text(", true), Encoding::Utf16)
            .unwrap();
        assert!(items.iter().any(|item| item.label == "property_59:"));
        assert!(items.iter().any(|item| item.label == "text" && item.kind == Some(CompletionItemKind::FUNCTION)));
    }
}
