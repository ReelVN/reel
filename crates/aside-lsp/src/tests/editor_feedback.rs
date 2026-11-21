use super::*;

#[test]
fn imported_effect_diagnostic_points_to_its_original_file() {
    let (_project, root) = project_sources();
    let source = "use game::helpers::{animate};\ngen fn story() { animate(); yield 0; }";
    let helper = "pub fn animate() { launch(); }";
    let file = root.join("helpers.aside");
    std::fs::write(&file, helper).unwrap();
    let original = file_uri(&file).unwrap();
    let main = file_uri(&root.join("main.aside")).unwrap();
    let mut options = project_options();
    options.host.globals.push(ExternalSymbol::function(
        "launch",
        ExternalSignature {
            parameters: vec![],
            result: TypeHint::Null,
            result_cases: vec![],
            result_parameter: None,
            effect: CallEffect::StartsTask,
            init_safe: Some(false),
            derived_safe: Some(false),
        },
    ));
    let mut workspace = Workspace::new(options);
    workspace.open(main.clone(), 1, source.into()).unwrap();
    let diagnostics = workspace.diagnostics(&main, Encoding::Utf16).unwrap();
    let report = diagnostics
        .iter()
        .find(|results| results.uri == main)
        .and_then(|results| results.diagnostics.first())
        .unwrap();
    let related = &report.related_information.as_ref().unwrap()[0];
    assert_eq!(related.location.uri, original);
    assert_eq!(
        related.location.range.start,
        position(helper, "launch", false)
    );

    workspace
        .open(original, 1, "pub fn animate() {}".into())
        .unwrap();
    let results = workspace.diagnostics(&main, Encoding::Utf16).unwrap();
    assert!(results.iter().all(|file| file.diagnostics.is_empty()));
}

#[test]
fn generator_keywords_follow_function_and_cleanup_boundaries() {
    for (source, yielded, awaited) in [
        ("gen fn values(){let item = ", true, false),
        (
            "gen fn values(){try {yield 1;} finally {let item = ",
            false,
            false,
        ),
        ("gen fn values(first = ", false, false),
        ("gen fn values(){let inner = || {let item = ", false, true),
        ("fn main(){let item = ", false, true),
    ] {
        let mut workspace = Workspace::new(ServerOptions::default());
        workspace.open(uri(), 1, source.into()).unwrap();
        let end = LineIndex::new(source)
            .position(source, source.len(), Encoding::Utf16)
            .unwrap();
        let items = workspace.complete(&uri(), end, Encoding::Utf16).unwrap();
        assert_eq!(
            items.iter().any(|item| item.label == "yield"),
            yielded,
            "{source}: {items:?}"
        );
        assert_eq!(
            items.iter().any(|item| item.label == "await"),
            awaited,
            "{source}: {items:?}"
        );
        assert!(
            !items.iter().any(|item| item.label == "async"),
            "{source}: {items:?}"
        );
    }
}

#[test]
fn hover_selects_import_identifiers_and_preserves_source_declarations() {
    let (_project, root) = project_sources();
    let declarations = "/// A documented counter.\npub class Counter {fn init(self, start:int=4){}}\n/// A documented greeting.\npub fn greet(name:string=\"World\")->string {name}\n/// A documented value.\npub let answer:int=42;\n/// A narrator value.\npub let 旁白:string=\"Narrator\";";
    std::fs::write(root.join("declarations.aside"), declarations).unwrap();
    let source = "use game::declarations::{Counter as Count, greet as hello, answer as result, 旁白};\nuse game::declarations as story;\nfn main(){let counter=Count(); hello(); result; 旁白; story.greet();}";
    let main = file_uri(&root.join("main.aside")).unwrap();
    let mut workspace = Workspace::new(project_options());
    workspace.open(main.clone(), 1, source.into()).unwrap();
    for needle in ["use game", "as Count", "fn main", "let counter"] {
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
    for (needle, name, module) in [
        ("game::declarations", "game", "game"),
        ("declarations::{", "declarations", "game::declarations"),
    ] {
        let offset = source.find(needle).unwrap();
        let lines = LineIndex::new(source);
        let start = lines.position(source, offset, Encoding::Utf16).unwrap();
        let end = lines
            .position(source, offset + name.len(), Encoding::Utf16)
            .unwrap();
        let hover = workspace
            .hover(&main, start, Encoding::Utf16)
            .unwrap()
            .unwrap();
        assert_eq!(hover.range, Some(Range::new(start, end)));
        assert!(hover_text(hover).contains(&format!("use {module}")));
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
        for name in ["int", "string", "table", "Future", "Actor", "Local", "fn"] {
            assert!(
                items.iter().any(|item| item.label == name),
                "missing {name} in {source:?}: {items:?}"
            );
        }
        assert!(
            !items.iter().any(
                |item| ["value", "main", "return", "let", "Map"].contains(&item.label.as_str())
            ),
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
fn module_type_aliases_complete_and_navigate_to_their_declaration() {
    let (_project, root) = project_sources();
    let model_source = "pub type Context = {caption: string};";
    std::fs::write(root.join("model.aside"), model_source).unwrap();
    let main = file_uri(&root.join("main.aside")).unwrap();
    let mut workspace = Workspace::new(project_options());
    let source = "use game::model as types; type Local = types::Context; fn show(value: Local) { value.caption; }";
    workspace.open(main.clone(), 1, source.into()).unwrap();
    for (needle, expected_uri, declared) in [
        (
            "Context",
            file_uri(&root.join("model.aside")).unwrap(),
            position(model_source, "Context", false),
        ),
        ("Local)", main.clone(), position(source, "Local =", false)),
    ] {
        let hover = hover_text(
            workspace
                .hover(&main, position(source, needle, false), Encoding::Utf16)
                .unwrap()
                .unwrap(),
        );
        assert!(
            hover.contains("type ") && hover.contains("caption: string"),
            "{hover}"
        );
        let location = workspace
            .definition(&main, position(source, needle, false), Encoding::Utf16)
            .unwrap()
            .unwrap();
        assert_eq!(location.uri, expected_uri);
        assert_eq!(location.range.start, declared);
    }
    let source = "use game::model as types; type Local = types::";
    workspace.open(main.clone(), 2, source.into()).unwrap();
    let end = LineIndex::new(source)
        .position(source, source.len(), Encoding::Utf16)
        .unwrap();
    let items = workspace.complete(&main, end, Encoding::Utf16).unwrap();
    assert!(
        items.iter().any(|item| item.label == "Context"),
        "{items:?}"
    );
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
                argument_choices: BTreeMap::new(),
                property_parameters: Vec::new(),
                parameter_documentation: BTreeMap::new(),
                property_groups: Vec::new(),
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
                argument_choices: BTreeMap::new(),
                property_parameters: std::iter::once("value".into()).chain(properties).collect(),
                parameter_documentation: BTreeMap::from([(
                    "property_59".into(),
                    Arc::from("Adjust the **last** property."),
                )]),
                property_groups: Vec::new(),
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
            .signature(
                &uri(),
                position(source, "\"Hello\"", true),
                Encoding::Utf16,
                &MarkupKind::Markdown,
            )
            .unwrap()
            .unwrap();
        assert!(
            help.signatures[0]
                .label
                .contains("\n    property_59?: int,\n)")
        );
        assert!(help.signatures[0].label.contains(canonical));
        assert_eq!(help.signatures[0].parameters.as_ref().unwrap().len(), 63);
        assert_eq!(help.active_parameter, Some(0));
        assert_eq!(
            help.signatures[0].documentation,
            Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: "Creates a text node.".into(),
            }))
        );
        let items = workspace
            .complete(&uri(), position(source, "text(", true), Encoding::Utf16)
            .unwrap();
        let last_property = items
            .iter()
            .find(|item| item.label == "property_59:")
            .unwrap();
        assert_eq!(
            last_property.documentation,
            help.signatures[0].parameters.as_ref().unwrap()[62].documentation
        );
        assert!(matches!(
            &last_property.documentation,
            Some(Documentation::MarkupContent(markup)) if markup.value == "Adjust the **last** property."
        ));
        assert!(items.iter().any(|item| item.label == "text" && item.kind == Some(CompletionItemKind::FUNCTION)));

        let named = source.replace("text(\"Hello\")", "text(\"Hello\", property_59: 1)");
        workspace.open(uri(), 2, named.clone()).unwrap();
        let help = workspace
            .signature(
                &uri(),
                position(&named, "property_59: 1", true),
                Encoding::Utf16,
                &MarkupKind::Markdown,
            )
            .unwrap()
            .unwrap();
        assert_eq!(help.active_parameter, Some(62));
    }
}

#[test]
fn literal_alternatives_survive_bindable_types_and_bind_stays_positional() {
    let mode = TypeHint::Union(vec![
        TypeHint::Union(vec![
            TypeHint::StringLiteral("vertical".into()),
            TypeHint::StringLiteral("horizontal".into()),
        ]),
        TypeHint::Named("Binding".into()),
    ]);
    let reference = SymbolReference {
        module: "native".into(),
        version: "1".into(),
        export: Some("host_option".into()),
    };
    let host_option = ExternalSymbol::function(
        "host_option",
        signature(vec![parameter("mode", mode, false)], TypeHint::Null),
    )
    .with_reference(reference.clone());
    let mut bind = ExternalSymbol::function(
        "bind",
        signature(
            vec![parameter("location", TypeHint::Any, false)],
            TypeHint::Named("Binding".into()),
        ),
    );
    bind.intrinsic = Some(Intrinsic::Bind);
    let source = r#"fn local_option(mode: "ondemand" | "continuous" | int) {}
fn main() {
    let state = { value: 3 };
    host_option(mode: "vertical");
    host_option("horizontal");
    local_option(mode: 1);
    bind(state.value);
}"#;
    let mut options = ServerOptions::default();
    options.host.globals = vec![host_option, bind];
    options.host.declarations.push(HostDeclaration {
        reference,
        signature: None,
        markdown: String::new(),
        definition: None,
        argument_choices: BTreeMap::from([("mode".into(), vec!["vertical".into(), "auto".into()])]),
        parameter_defaults: BTreeMap::new(),
        property_parameters: Vec::new(),
        parameter_documentation: BTreeMap::new(),
        property_groups: Vec::new(),
        property_default: None,
    });
    let mut workspace = Workspace::new(options);
    workspace.open(uri(), 1, source.into()).unwrap();
    let complete = |workspace: &mut Workspace, at| {
        workspace
            .complete(&uri(), position(source, at, true), Encoding::Utf16)
            .unwrap()
    };
    for (at, choices) in [
        ("host_option(mode:", ["\"vertical\"", "\"horizontal\""]),
        ("local_option(mode:", ["\"ondemand\"", "\"continuous\""]),
    ] {
        let items = complete(&mut workspace, at);
        assert!(
            choices
                .into_iter()
                .all(|choice| items.iter().any(|item| item.label == choice)),
            "{items:?}"
        );
    }
    for (at, value) in [
        ("host_option(mode: \"vert", "\"vertical\""),
        ("host_option(\"horiz", "\"horizontal\""),
    ] {
        let items = complete(&mut workspace, at);
        assert_eq!(
            items
                .iter()
                .filter(|item| item.label == "\"vertical\"")
                .count(),
            1
        );
        let text_edit = items
            .iter()
            .find(|item| item.label == "\"auto\"")
            .unwrap()
            .text_edit
            .as_ref()
            .unwrap();
        assert_eq!(
            text_edit,
            &CompletionTextEdit::Edit(TextEdit {
                range: Range::new(
                    position(source, value, false),
                    position(source, value, true)
                ),
                new_text: "\"auto\"".into(),
            })
        );
    }
    assert!(
        !complete(&mut workspace, "bind(")
            .iter()
            .any(|item| item.label == "location:")
    );
}
