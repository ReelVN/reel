use super::*;
use crate::{
    position::{Encoding, LineIndex},
    workspace::{Workspace, file_uri},
};
use aside_compiler::semantic::*;
use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::*;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use std::{str::FromStr, time::Duration};

mod builtin_methods;
mod editor_feedback;
mod module_navigation;
mod source_inference;

#[test]
fn is_guard_updates_hover_members_and_definition_in_captured_branch() {
    let source = "class Base{} class Child:Base{fn answer(self)->int{42}} fn inspect(value:Base){if value is Child{let callback=||value.answer();} let outside=value;}";
    let mut workspace = Workspace::new(ServerOptions::default());
    workspace.open(uri(), 1, source.into()).unwrap();
    let hover = hover_text(
        workspace
            .hover(
                &uri(),
                position(source, "value.answer", false),
                Encoding::Utf16,
            )
            .unwrap()
            .unwrap(),
    );
    assert!(hover.contains("value: Child"), "{hover}");
    let items = workspace
        .complete(
            &uri(),
            position(source, "value.answer", true),
            Encoding::Utf16,
        )
        .unwrap();
    assert!(
        items
            .iter()
            .any(|item| item.label == "answer" && item.kind == Some(CompletionItemKind::METHOD)),
        "{items:?}"
    );
    let definition = workspace
        .definition(
            &uri(),
            position(source, "answer();", false),
            Encoding::Utf16,
        )
        .unwrap();
    assert!(definition.is_some());
    let outside = source.rfind("value;").unwrap();
    let at = LineIndex::new(source)
        .position(source, outside, Encoding::Utf16)
        .unwrap();
    let hover = hover_text(
        workspace
            .hover(&uri(), at, Encoding::Utf16)
            .unwrap()
            .unwrap(),
    );
    assert!(hover.contains("value: Base"), "{hover}");
}

#[test]
fn incomplete_guards_keep_recoverable_member_completion() {
    for tail in [
        "if value is Child &&",
        "if value is Child && value.",
        "if value is Child {let callback=||value.",
    ] {
        let source = format!(
            "class Base{{}} class Child:Base{{fn answer(self)->int{{42}}}} fn inspect(value:Base){{{tail}"
        );
        let mut workspace = Workspace::new(ServerOptions::default());
        workspace.open(uri(), 1, source.clone()).unwrap();
        let end = LineIndex::new(&source)
            .position(&source, source.len(), Encoding::Utf16)
            .unwrap();
        let items = workspace.complete(&uri(), end, Encoding::Utf16).unwrap();
        if tail.ends_with('.') {
            assert!(
                items.iter().any(|item| item.label == "answer"),
                "{tail}: {items:?}"
            );
        } else {
            assert!(
                items.iter().any(|item| item.label == "value"),
                "{tail}: {items:?}"
            );
        }
    }
}

#[test]
fn callable_types_offer_real_signature_positions_without_inventing_parameter_names() {
    let source = "fn invoke(callback:fn(int,int)->string){callback(1,2);}";
    let mut workspace = Workspace::new(ServerOptions::default());
    workspace.open(uri(), 1, source.into()).unwrap();
    let help = workspace
        .signature(
            &uri(),
            position(source, "callback(1,", true),
            Encoding::Utf16,
        )
        .unwrap()
        .unwrap();
    assert_eq!(help.signatures[0].label, "fn callback(int, int) -> string");
    assert_eq!(help.active_parameter, Some(1));
    let parameters = help.signatures[0].parameters.as_ref().unwrap();
    assert_eq!(parameters[0].label, ParameterLabel::LabelOffsets([12, 15]));
    assert_eq!(parameters[1].label, ParameterLabel::LabelOffsets([17, 20]));
    let hover = hover_text(
        workspace
            .hover(
                &uri(),
                position(source, "callback(1", false),
                Encoding::Utf16,
            )
            .unwrap()
            .unwrap(),
    );
    assert!(hover.contains("fn callback(int, int) -> string"), "{hover}");
    let items = workspace
        .complete(&uri(), position(source, "callback(", true), Encoding::Utf16)
        .unwrap();
    assert!(items.iter().all(|item| item.label != ":"));
    assert!(items.iter().any(|item| item.label == "callback" && item.kind == Some(CompletionItemKind::FUNCTION)));
    assert!(
        workspace
            .definition(
                &uri(),
                position(source, "callback(1", false),
                Encoding::Utf16
            )
            .unwrap()
            .is_some()
    );
}

fn project_options() -> ServerOptions {
    ServerOptions {
        source_root: Some(|directory, exists| {
            directory
                .ancestors()
                .find(|root| exists(&root.join("src/main.aside")))
                .map(|root| root.join("src"))
        }),
        project_prefix: Some("game".into()),
        ..Default::default()
    }
}
fn project_sources() -> (tempfile::TempDir, std::path::PathBuf) {
    let project = tempfile::tempdir().unwrap();
    let source = project.path().join("src");
    std::fs::create_dir(&source).unwrap();
    (project, source)
}

fn position(source: &str, needle: &str, end: bool) -> Position {
    let offset = source.rfind(needle).unwrap() + if end { needle.len() } else { 0 };
    LineIndex::new(source)
        .position(source, offset, Encoding::Utf16)
        .unwrap()
}
fn uri() -> Uri {
    Uri::from_str("untitled:small.aside").unwrap()
}
fn hover_text(value: Hover) -> String {
    match value.contents {
        HoverContents::Markup(markup) => markup.value,
        _ => panic!("expected Markdown hover"),
    }
}

#[test]
fn local_shadowing_docs_signature_and_recovery_use_original_scope() {
    let source = "/// Adds a value.\nfn add(value:int, step:int=2)->int {value+step}\nfn main(){let value=1; {let value=\"inner\"; value;} add(value, step:3);}";
    let mut workspace = Workspace::new(ServerOptions::default());
    workspace.open(uri(), 1, source.into()).unwrap();
    let local = workspace
        .complete(&uri(), position(source, "value;}", false), Encoding::Utf16)
        .unwrap();
    let matches: Vec<_> = local.iter().filter(|item| item.label == "value").collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].detail.as_deref(), Some("string"));
    let hover = hover_text(
        workspace
            .hover(
                &uri(),
                position(source, "add(value", false),
                Encoding::Utf16,
            )
            .unwrap()
            .unwrap(),
    );
    assert!(hover.contains("Adds a value."));
    assert!(hover.contains("step: int = 2"));
    let signature = workspace
        .signature(&uri(), position(source, "step:3", true), Encoding::Utf16)
        .unwrap()
        .unwrap();
    assert_eq!(signature.active_parameter, Some(1));
    assert!(signature.signatures[0].label.contains("value: int"));
    let definition = workspace
        .definition(
            &uri(),
            position(source, "add(value", false),
            Encoding::Utf16,
        )
        .unwrap()
        .unwrap();
    assert_eq!(definition.range.start, Position::new(1, 3));
    assert_eq!(
        workspace.analyses, 1,
        "unchanged queries share one project analysis"
    );
    workspace
        .change(
            &uri(),
            2,
            vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "fn main(){let retained=1; retained. }".into(),
            }],
            Encoding::Utf16,
        )
        .unwrap();
    assert!(
        workspace
            .diagnostics(&uri(), Encoding::Utf16)
            .unwrap()
            .iter()
            .any(|params| !params.diagnostics.is_empty())
    );
    assert!(
        workspace
            .complete(&uri(), Position::new(0, 33), Encoding::Utf16)
            .unwrap()
            .iter()
            .any(|item| item.label == "retained")
    );
}

#[test]
fn unsaved_imports_missing_modules_and_close_invalidate_dependencies() {
    let (_project, root) = project_sources();
    let main = root.join("main.aside");
    let helper = root.join("helpers.aside");
    let source = "use game::helpers::add; fn main(){add(step:3);}";
    std::fs::write(&main, source).unwrap();
    let main_uri = file_uri(&main).unwrap();
    let helper_uri = file_uri(&helper).unwrap();
    let mut workspace = Workspace::new(project_options());
    workspace.open(main_uri.clone(), 1, source.into()).unwrap();
    assert!(
        workspace
            .diagnostics(&main_uri, Encoding::Utf16)
            .unwrap()
            .iter()
            .any(|params| !params.diagnostics.is_empty())
    );
    let helper_source = "/// Actual unsaved helper.\npub fn add(step:int=7)->int {step}";
    workspace
        .open(helper_uri.clone(), 1, helper_source.into())
        .unwrap();
    assert!(
        workspace
            .diagnostics(&main_uri, Encoding::Utf16)
            .unwrap()
            .iter()
            .all(|params| params.diagnostics.is_empty())
    );
    let definition = workspace
        .definition(
            &main_uri,
            position(source, "add(step", false),
            Encoding::Utf16,
        )
        .unwrap()
        .unwrap();
    assert_eq!(definition.uri, helper_uri);
    let hover = hover_text(
        workspace
            .hover(
                &main_uri,
                position(source, "add(step", false),
                Encoding::Utf16,
            )
            .unwrap()
            .unwrap(),
    );
    assert!(hover.contains("Actual unsaved helper."));
    assert!(hover.contains("step: int = 7"));
    workspace
        .change(
            &helper_uri,
            2,
            vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "pub fn add(step:string)->string {step}".into(),
            }],
            Encoding::Utf16,
        )
        .unwrap();
    assert!(
        workspace
            .diagnostics(&main_uri, Encoding::Utf16)
            .unwrap()
            .iter()
            .any(|params| !params.diagnostics.is_empty())
    );
    workspace.close(&helper_uri);
    assert!(
        workspace
            .diagnostics(&main_uri, Encoding::Utf16)
            .unwrap()
            .iter()
            .any(|params| {
                params.diagnostics.iter().any(|diagnostic| {
                    diagnostic.code == Some(NumberOrString::String("ASS013".into()))
                })
            })
    );
}

#[test]
fn multiple_nearest_projects_and_public_reexports_keep_real_definition() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::new(project_options());
    let mut documents = Vec::new();
    for (name, ty, value) in [("first", "int", "1"), ("second", "string", "\"two\"")] {
        let project = root.path().join(name).join("src");
        std::fs::create_dir_all(project.join("ui")).unwrap();
        std::fs::write(project.join("main.aside"), "fn main(){}").unwrap();
        std::fs::write(
            project.join("helpers.aside"),
            format!("pub fn get()->{ty} {{{value}}}"),
        )
        .unwrap();
        std::fs::write(project.join("api.aside"), "pub use game::helpers::get;").unwrap();
        let uri = file_uri(&project.join("ui/scene.aside")).unwrap();
        let source = "use game::api::get; fn scene(){let value=get(); value;}";
        workspace.open(uri.clone(), 1, source.into()).unwrap();
        let hover = hover_text(
            workspace
                .hover(&uri, position(source, "value;", false), Encoding::Utf16)
                .unwrap()
                .unwrap(),
        );
        assert!(hover.contains(ty), "{hover}");
        assert_eq!(
            workspace
                .definition(&uri, position(source, "get();", false), Encoding::Utf16)
                .unwrap()
                .unwrap()
                .uri,
            file_uri(&project.join("helpers.aside")).unwrap()
        );
        documents.push(uri);
    }
    let analyses = workspace.analyses;
    let unrelated = file_uri(&root.path().join("main.aside")).unwrap();
    workspace
        .open(unrelated.clone(), 1, "fn main(){}".into())
        .unwrap();
    for uri in &documents {
        workspace.snapshot(uri).unwrap();
    }
    assert_eq!(
        workspace.analyses, analyses,
        "unrelated entry changes invalidated other projects"
    );
    workspace.close(&unrelated);
}

#[test]
fn source_class_members_and_bound_method_parameters() {
    let source = "class Counter {let value:int=1; fn add(self, amount:int=2)->int {self.value+amount}} fn main(){let counter=Counter(); counter.value; counter.add(amount:3);}";
    let mut workspace = Workspace::new(ServerOptions::default());
    workspace.open(uri(), 1, source.into()).unwrap();
    let items = workspace
        .complete(
            &uri(),
            position(source, "counter.value", true),
            Encoding::Utf16,
        )
        .unwrap();
    assert!(items.iter().any(|item| item.label == "value"));
    assert!(items.iter().any(|item| item.label == "add"));
    let signature = workspace
        .signature(&uri(), position(source, "amount:3", true), Encoding::Utf16)
        .unwrap()
        .unwrap();
    assert!(!signature.signatures[0].label.contains("self"));
    assert!(signature.signatures[0].label.contains("amount: int = 2"));
}

#[test]
fn imported_class_constructor_members_defaults_and_reexports_share_identity() {
    let (_project, root) = project_sources();
    let model = "/// A small counter.\npub class Counter {let value:int=1; fn init(self, start:int=4){} /// Adds an amount.\nfn add(self, amount:int=2)->int {self.value+amount} static fn zero()->int {0}}";
    std::fs::write(root.join("model.aside"), model).unwrap();
    std::fs::write(root.join("api.aside"), "pub use game::model::Counter;").unwrap();
    let source = "use game::api::Counter; fn main(){let counter=Counter(start:5); counter.value; counter.add(amount:3); let method=counter.add; method(amount:6); Counter.zero();}";
    let main = file_uri(&root.join("main.aside")).unwrap();
    let mut workspace = Workspace::new(project_options());
    workspace.open(main.clone(), 1, source.into()).unwrap();
    assert!(
        workspace
            .diagnostics(&main, Encoding::Utf16)
            .unwrap()
            .iter()
            .all(|params| params.diagnostics.is_empty())
    );
    let items = workspace
        .complete(
            &main,
            position(source, "counter.value", true),
            Encoding::Utf16,
        )
        .unwrap();
    assert!(
        items
            .iter()
            .any(|item| item.label == "value" && item.detail.as_deref() == Some("int"))
    );
    assert!(items.iter().any(|item| item.label == "add"));
    assert!(!items.iter().any(|item| item.label == "zero"));
    let hover = hover_text(
        workspace
            .hover(
                &main,
                position(source, "counter.add", true),
                Encoding::Utf16,
            )
            .unwrap()
            .unwrap(),
    );
    assert!(hover.contains("Adds an amount."), "{hover}");
    assert!(hover.contains("amount: int = 2"), "{hover}");
    assert!(!hover.contains("self"));
    let signature = workspace
        .signature(&main, position(source, "start:5", true), Encoding::Utf16)
        .unwrap()
        .unwrap();
    assert!(signature.signatures[0].label.contains("start: int = 4"));
    let definition = workspace
        .definition(
            &main,
            position(source, "counter.add", true),
            Encoding::Utf16,
        )
        .unwrap()
        .unwrap();
    assert_eq!(definition.uri, file_uri(&root.join("model.aside")).unwrap());
    assert_eq!(definition.range.start, position(model, "add(self", false));
    let static_items = workspace
        .complete(
            &main,
            position(source, "Counter.zero", true),
            Encoding::Utf16,
        )
        .unwrap();
    assert!(static_items.iter().any(|item| item.label == "zero"));
    assert!(!static_items.iter().any(|item| item.label == "value"));
    let snapshot = workspace.snapshot(&main).unwrap();
    let module = snapshot.module().unwrap();
    assert!(
        module.external_bindings().iter().any(|binding| binding.kind
            == aside_compiler::hir::ExternalBindingKind::Metadata
            && binding.symbol.is_some_and(|symbol| module
                .hir()
                .name(module.symbols()[symbol.0 as usize].name)
                == "method")),
        "a bound alias retains member metadata without becoming a static link"
    );
}

#[test]
fn cross_module_inheritance_reuses_constructor_and_completion_declarations() {
    let (_project, root) = project_sources();
    let base = "pub class Base {let base_field:int=3; /// Creates the base scene.\nfn init(self,label:string=\"Default\",count:int=2){} /// Inherited action.\nfn inherited(self,step:int=7)->int {step} fn named(self,value:int=1)->int {value} /// Static base action.\nstatic fn static_base(value:int=11)->int {value}}";
    std::fs::write(root.join("base.aside"), base).unwrap();
    std::fs::write(root.join("middle.aside"),"use game::base::Base as Root; pub class Middle:Root {/// Replaces the base action.\nfn named(self,value:string=\"middle\")->string {value}}").unwrap();
    std::fs::write(
        root.join("child.aside"),
        "use game::middle::Middle as Parent; /// A derived scene.\npub class Child:Parent {}",
    )
    .unwrap();
    std::fs::write(
        root.join("api.aside"),
        "pub use game::child::Child as PublicChild;",
    )
    .unwrap();
    let source = "use game::api::PublicChild as Renamed; fn main(){let child=Renamed(label:\"x\",count:3); child.inherited(step:4); child.named(value:\"x\"); Renamed.static_base(value:12); child.base_field;}";
    let main = file_uri(&root.join("main.aside")).unwrap();
    let mut workspace = Workspace::new(project_options());
    workspace.open(main.clone(), 1, source.into()).unwrap();
    let diagnostics = workspace.diagnostics(&main, Encoding::Utf16).unwrap();
    assert!(
        diagnostics
            .iter()
            .all(|params| params.diagnostics.is_empty()),
        "{diagnostics:?}"
    );
    let signature = workspace
        .signature(&main, position(source, "count:3", true), Encoding::Utf16)
        .unwrap()
        .unwrap();
    assert!(
        signature.signatures[0]
            .label
            .contains("label: string = \"Default\", count: int = 2"),
        "{:?}",
        signature.signatures[0]
    );
    assert_eq!(signature.active_parameter, Some(1));
    assert!(!signature.signatures[0].label.contains("self"));
    let docs = signature.signatures[0].documentation.as_ref().unwrap();
    assert!(
        matches!(docs,Documentation::String(text) if text.contains("Creates the base scene.")&&text.contains("A derived scene.")),
        "{docs:?}"
    );
    let items = workspace
        .complete(
            &main,
            position(source, "child.base_field", true),
            Encoding::Utf16,
        )
        .unwrap();
    assert!(
        items
            .iter()
            .any(|item| item.label == "base_field" && item.detail.as_deref() == Some("int"))
    );
    let inherited = items.iter().find(|item| item.label == "inherited").unwrap();
    assert!(inherited.detail.as_ref().unwrap().contains("step: int = 7"));
    assert!(
        matches!(&inherited.documentation,Some(Documentation::String(docs)) if docs.contains("Inherited action."))
    );
    let overrides: Vec<_> = items.iter().filter(|item| item.label == "named").collect();
    assert_eq!(overrides.len(), 1);
    assert!(
        overrides[0]
            .detail
            .as_ref()
            .unwrap()
            .contains("value: string = \"middle\"")
    );
    assert!(!items.iter().any(|item| item.label == "static_base"));
    let statics = workspace
        .complete(
            &main,
            position(source, "Renamed.static_base", true),
            Encoding::Utf16,
        )
        .unwrap();
    let inherited = statics
        .iter()
        .find(|item| item.label == "static_base")
        .unwrap();
    assert!(
        inherited
            .detail
            .as_ref()
            .unwrap()
            .contains("value: int = 11")
    );
    assert!(
        !statics
            .iter()
            .any(|item| item.label == "base_field" || item.label == "inherited")
    );
    let definition = workspace
        .definition(
            &main,
            position(source, "child.inherited", true),
            Encoding::Utf16,
        )
        .unwrap()
        .unwrap();
    assert_eq!(definition.uri, file_uri(&root.join("base.aside")).unwrap());
    assert_eq!(
        definition.range.start,
        position(base, "inherited(self", false)
    );
    let base_uri = file_uri(&root.join("base.aside")).unwrap();
    workspace
        .open(
            base_uri.clone(),
            1,
            base.replace("Default", "Edited")
                .replace("Creates the base scene.", "Updated parent constructor."),
        )
        .unwrap();
    let edited = workspace
        .signature(&main, position(source, "count:3", true), Encoding::Utf16)
        .unwrap()
        .unwrap();
    assert!(
        edited.signatures[0]
            .label
            .contains("label: string = \"Edited\"")
    );
    assert!(
        matches!(&edited.signatures[0].documentation,Some(Documentation::String(docs)) if docs.contains("Updated parent constructor."))
    );
    workspace.close(&base_uri);
    let restored = workspace
        .signature(&main, position(source, "count:3", true), Encoding::Utf16)
        .unwrap()
        .unwrap();
    assert!(
        restored.signatures[0]
            .label
            .contains("label: string = \"Default\"")
    );
}

fn signature(parameters: Vec<ExternalParameter>, result: TypeHint) -> ExternalSignature {
    ExternalSignature {
        result_parameter: None,
        parameters,
        result,
        result_cases: Vec::new(),
        effect: CallEffect::NoWait,
        init_safe: Some(true),
        derived_safe: Some(true),
    }
}
fn parameter(name: &str, ty: TypeHint, default: bool) -> ExternalParameter {
    ExternalParameter {
        name: name.into(),
        ty,
        mutable: false,
        default,
        rest: false,
    }
}

#[test]
fn constant_arguments_refine_results_for_aliases_bound_methods_and_defaults() {
    let speech = TypeHint::Named("native::Speech".into());
    let mut say = signature(
        vec![
            parameter("text", TypeHint::String, false),
            parameter("wait", TypeHint::Bool, true),
        ],
        TypeHint::Union(vec![speech.clone(), TypeHint::Null]),
    );
    say.result_cases = vec![
        ExternalResultCase {
            parameter: "wait".into(),
            equals: Const::Bool(false),
            result: speech.clone(),
            when_omitted: false,
        },
        ExternalResultCase {
            parameter: "wait".into(),
            equals: Const::Bool(true),
            result: TypeHint::Null,
            when_omitted: true,
        },
    ];
    let context = HostContext {
        globals: vec![
            ExternalSymbol::function("say", say.clone()),
            ExternalSymbol::value("actor", TypeHint::Named("native::Actor".into())),
        ],
        named_types: vec![
            NamedType {
                name: "native::Actor".into(),
                bases: Vec::new(),
                call: None,
                await_result: None,
                members: vec![HostMember {
                    symbol: ExternalSymbol::function("say", say.clone()),
                    write_type: None,
                }],
            },
            NamedType {
                name: "native::Speech".into(),
                bases: Vec::new(),
                call: None,
                await_result: None,
                members: vec![HostMember {
                    symbol: ExternalSymbol::function("append", say),
                    write_type: None,
                }],
            },
        ],
        ..Default::default()
    };
    let source = "fn main(flag:bool, args:array){let alias=say; let a=say(\"A\",wait:false); let b=say(\"B\"); let c=alias(\"C\",false); let d=actor.say(\"D\",wait:false); let bound=actor.say; let e=bound(\"E\",false); let f=say(\"F\",wait:flag); let g=say(...args); let h=f?.append(\"H\",wait:false); a;b;c;d;e;f;g;h;}";
    let mut workspace = Workspace::new(ServerOptions {
        host: context,
        ..Default::default()
    });
    workspace.open(uri(), 1, source.into()).unwrap();
    assert!(
        workspace
            .diagnostics(&uri(), Encoding::Utf16)
            .unwrap()
            .iter()
            .all(|params| params.diagnostics.is_empty())
    );
    for (name, expected) in [
        ("a;", "native::Speech"),
        ("b;", "null"),
        ("c;", "native::Speech"),
        ("d;", "native::Speech"),
        ("e;", "native::Speech"),
        ("f;", "native::Speech | null"),
        ("g;", "native::Speech | null"),
        ("h;", "native::Speech | null"),
    ] {
        let hover = hover_text(
            workspace
                .hover(&uri(), position(source, name, false), Encoding::Utf16)
                .unwrap()
                .unwrap(),
        );
        assert!(
            hover.contains(&format!(": {expected}\n")),
            "{name}: {hover}"
        );
    }
}

#[test]
fn native_types_keep_nominal_members_readonly_defaults_and_definition_documents() {
    let node = "native::types::Node";
    let text = "native::types::Text";
    let foreign = "native::types::Foreign";
    let reference = SymbolReference {
        module: "native::properties".into(),
        version: "1".into(),
        export: Some("width".into()),
    };
    let width = HostMember {
        symbol: ExternalSymbol::value("width", TypeHint::Int).with_reference(reference.clone()),
        write_type: None,
    };
    let writable = HostMember {
        symbol: ExternalSymbol::value("value", TypeHint::Int).with_reference(SymbolReference {
            module: "native::properties".into(),
            version: "1".into(),
            export: Some("value".into()),
        }),
        write_type: Some(TypeHint::Union(vec![
            TypeHint::Int,
            TypeHint::Named("Binding".into()),
        ])),
    };
    let mut node_symbol = ExternalSymbol::value("Node", TypeHint::Named(node.into()));
    node_symbol.type_only = true;
    let doc_uri = Uri::from_str("aside-native:/types").unwrap();
    let document = "/// Node width.\nlet width: int = 12;\n";
    let start = document.find("width").unwrap() as u32;
    let context = HostContext {
        globals: vec![
            ExternalSymbol::function("make", signature(Vec::new(), TypeHint::Named(text.into()))),
            ExternalSymbol::function(
                "foreign",
                signature(Vec::new(), TypeHint::Named(foreign.into())),
            ),
            ExternalSymbol::function(
                "accept",
                signature(
                    vec![parameter("node", TypeHint::Named(node.into()), false)],
                    TypeHint::Null,
                ),
            ),
        ],
        modules: vec![ModuleInterface {
            canonical_path: "native::types".into(),
            version: "1".into(),
            exports: vec![
                node_symbol,
                ExternalSymbol::value("Text", TypeHint::Named(text.into())),
            ],
        }],
        named_types: vec![
            NamedType {
                name: node.into(),
                bases: Vec::new(),
                call: None,
                await_result: None,
                members: vec![width.clone(), writable.clone()],
            },
            NamedType {
                name: text.into(),
                bases: vec![node.into()],
                call: None,
                await_result: None,
                members: vec![width, writable],
            },
            NamedType {
                name: foreign.into(),
                bases: Vec::new(),
                call: None,
                await_result: None,
                members: Vec::new(),
            },
        ],
        declarations: vec![HostDeclaration {
            reference,
            signature: None,
            markdown: "Node width.".into(),
            definition: Some(HostLocation {
                uri: doc_uri.clone(),
                range: aside_compiler::TextRange::new(start.into(), (start + 5).into()),
            }),
            parameter_defaults: BTreeMap::new(),
            property_parameters: Vec::new(),
            parameter_documentation: BTreeMap::new(),
            property_groups: Vec::new(),
            property_default: Some("12".into()),
        }],
        documents: vec![HostDocument {
            references: Vec::new(),
            uri: doc_uri.clone(),
            text: Arc::from(document),
        }],
    };
    let source = "use native::types::{Node as HostNode, Text}; class Node{let local:int=1;} fn main(){let native:HostNode=make(); let concrete:Text=make(); accept(concrete); native.width; native.value; let local=Node(); local.local;}";
    let mut workspace = Workspace::new(ServerOptions {
        host: context,
        ..Default::default()
    });
    workspace.open(uri(), 1, source.into()).unwrap();
    assert!(
        workspace
            .diagnostics(&uri(), Encoding::Utf16)
            .unwrap()
            .iter()
            .all(|params| params.diagnostics.is_empty())
    );
    let native_items = workspace
        .complete(
            &uri(),
            position(source, "native.width", true),
            Encoding::Utf16,
        )
        .unwrap();
    assert!(native_items.iter().any(|item| item.label == "width"));
    assert!(!native_items.iter().any(|item| item.label == "local"));
    let local_items = workspace
        .complete(
            &uri(),
            position(source, "local.local", true),
            Encoding::Utf16,
        )
        .unwrap();
    assert!(local_items.iter().any(|item| item.label == "local"));
    assert!(!local_items.iter().any(|item| item.label == "width"));
    let hover = hover_text(
        workspace
            .hover(
                &uri(),
                position(source, "native.width", true),
                Encoding::Utf16,
            )
            .unwrap()
            .unwrap(),
    );
    assert!(hover.contains("width: int = 12"));
    assert!(hover.contains("Node width."));
    let write_hover = hover_text(
        workspace
            .hover(
                &uri(),
                position(source, "native.value", true),
                Encoding::Utf16,
            )
            .unwrap()
            .unwrap(),
    );
    assert!(write_hover.contains("let mut value: int"));
    assert!(write_hover.contains("Write type: int | Binding"));
    assert_eq!(
        workspace
            .definition(
                &uri(),
                position(source, "native.width", true),
                Encoding::Utf16
            )
            .unwrap()
            .unwrap()
            .uri,
        doc_uri
    );
    let invalid = "use native::types::Node; fn main(){let concrete=make(); concrete.width=4; accept(foreign()); Node();}";
    workspace
        .change(
            &uri(),
            2,
            vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: invalid.into(),
            }],
            Encoding::Utf16,
        )
        .unwrap();
    let diagnostics = workspace.diagnostics(&uri(), Encoding::Utf16).unwrap();
    assert!(
        diagnostics
            .iter()
            .flat_map(|params| &params.diagnostics)
            .all(|diagnostic| !diagnostic.message.contains("NameId"))
    );
    assert!(
        diagnostics
            .iter()
            .flat_map(|params| &params.diagnostics)
            .any(|diagnostic| diagnostic
                .message
                .contains("type declaration cannot be read"))
    );
    assert!(
        diagnostics
            .iter()
            .flat_map(|params| &params.diagnostics)
            .any(|diagnostic| diagnostic.code
                == Some(NumberOrString::String(
                    aside_compiler::DiagnosticCode::ImmutableBinding
                        .as_str()
                        .into()
                )))
    );
    assert!(
        diagnostics
            .iter()
            .flat_map(|params| &params.diagnostics)
            .any(|diagnostic| diagnostic.message.contains("call argument"))
    );
}

#[test]
fn nested_arguments_and_named_completion_use_direct_binding_positions() {
    let source = "fn sum(a:int,b:int)->int{a+b} fn take(first:int,second:int=2,third:int=3){} fn main(){take(sum(1,2), third:4);}";
    let mut workspace = Workspace::new(ServerOptions::default());
    workspace.open(uri(), 1, source.into()).unwrap();
    let signature = workspace
        .signature(&uri(), position(source, "third:4", true), Encoding::Utf16)
        .unwrap()
        .unwrap();
    assert_eq!(signature.active_parameter, Some(2));
    let items = workspace
        .complete(&uri(), position(source, "third:4", true), Encoding::Utf16)
        .unwrap();
    assert!(!items.iter().any(|item| item.label == "first:"));
    assert!(!items.iter().any(|item| item.label == "third:"));
    assert!(items.iter().any(|item| item.label == "second:"));
}

#[test]
fn named_callable_and_await_protocols_keep_real_result_types() {
    let mut call = signature(
        vec![
            parameter("text", TypeHint::String, false),
            parameter("wait", TypeHint::Bool, true),
        ],
        TypeHint::Union(vec![
            TypeHint::Named("native::Speech".into()),
            TypeHint::Null,
        ]),
    );
    call.result_cases = vec![ExternalResultCase {
        parameter: "wait".into(),
        equals: Const::Bool(false),
        result: TypeHint::Named("native::Speech".into()),
        when_omitted: false,
    }];
    let context = HostContext {
        globals: vec![
            ExternalSymbol::value("actor", TypeHint::Named("native::Actor".into())),
            ExternalSymbol::value("task", TypeHint::Named("native::Task".into())),
        ],
        named_types: vec![
            NamedType {
                name: "native::Actor".into(),
                bases: Vec::new(),
                call: Some(ExternalSymbol::function("speak", call)),
                await_result: None,
                members: Vec::new(),
            },
            NamedType {
                name: "native::Task".into(),
                bases: Vec::new(),
                call: None,
                await_result: Some(TypeHint::Named("native::Completion".into())),
                members: Vec::new(),
            },
        ],
        ..Default::default()
    };
    let source = "fn main(){let alias=actor; let spoken=alias(\"Hi\",wait:false); let completed=await task; spoken; completed;}";
    let mut workspace = Workspace::new(ServerOptions {
        host: context,
        ..Default::default()
    });
    workspace.open(uri(), 1, source.into()).unwrap();
    assert!(
        workspace
            .diagnostics(&uri(), Encoding::Utf16)
            .unwrap()
            .iter()
            .all(|params| params.diagnostics.is_empty())
    );
    for (name, expected) in [
        ("spoken;", "native::Speech"),
        ("completed;", "native::Completion"),
    ] {
        let hover = hover_text(
            workspace
                .hover(&uri(), position(source, name, false), Encoding::Utf16)
                .unwrap()
                .unwrap(),
        );
        assert!(hover.contains(expected), "{hover}");
    }
    assert!(
        workspace
            .signature(
                &uri(),
                position(source, "wait:false", true),
                Encoding::Utf16
            )
            .unwrap()
            .unwrap()
            .signatures[0]
            .label
            .contains("wait?: bool")
    );
    assert_eq!(
        crate::queries::hint_label(&TypeHint::Tuple(vec![TypeHint::Int])),
        "(int,)"
    );
    assert_eq!(
        crate::queries::hint_label(&TypeHint::Callable(
            Vec::new(),
            Box::new(TypeHint::Union(vec![TypeHint::Int, TypeHint::Null]))
        )),
        "fn() -> (int | null)"
    );
}

#[test]
fn identity_result_parameters_preserve_specific_arguments_through_all_call_forms() {
    let node = TypeHint::Named("native::Node".into());
    let particles = TypeHint::Named("native::Particles".into());
    let mut add = signature(vec![parameter("child", node.clone(), true)], node.clone());
    add.result_parameter = Some("child".into());
    let context = HostContext {
        globals: vec![
            ExternalSymbol::function("add", add.clone()),
            ExternalSymbol::function("particles", signature(Vec::new(), particles.clone())),
            ExternalSymbol::value("root", node),
        ],
        named_types: vec![
            NamedType {
                name: "native::Node".into(),
                bases: Vec::new(),
                call: None,
                await_result: None,
                members: vec![HostMember {
                    symbol: ExternalSymbol::function("add", add),
                    write_type: None,
                }],
            },
            NamedType {
                name: "native::Particles".into(),
                bases: vec!["native::Node".into()],
                call: None,
                await_result: None,
                members: Vec::new(),
            },
        ],
        ..Default::default()
    };
    let source = "fn main(args:array){let child=particles();let alias=add;let bound=root.add;let a=add(child);let b=root.add(child:child);let c=bound(child);let d=alias(child:child);let e=add();let f=add(...args);a;b;c;d;e;f;}";
    let mut workspace = Workspace::new(ServerOptions {
        host: context,
        ..Default::default()
    });
    workspace.open(uri(), 1, source.into()).unwrap();
    assert!(
        workspace
            .diagnostics(&uri(), Encoding::Utf16)
            .unwrap()
            .iter()
            .all(|params| params.diagnostics.is_empty())
    );
    for (name, expected) in [
        ("a;", "native::Particles"),
        ("b;", "native::Particles"),
        ("c;", "native::Particles"),
        ("d;", "native::Particles"),
        ("e;", "native::Node"),
        ("f;", "native::Node"),
    ] {
        let hover = hover_text(
            workspace
                .hover(&uri(), position(source, name, false), Encoding::Utf16)
                .unwrap()
                .unwrap(),
        );
        assert!(hover.contains(expected), "{hover}");
    }
}

#[test]
fn mutable_unannotated_fields_and_inherited_imported_constructors_remain_dynamic() {
    let (_project, root) = project_sources();
    std::fs::write(root.join("model.aside"),"pub class Base{let mut value=null; fn init(self,value){self.value=value;} fn set(self,value){self.value=value;} fn run(self){self.value()}} pub class Child:Base{}").unwrap();
    let source =
        "use game::model::Child; fn main(){let child=Child(||1); child.set(2); child.run();}";
    let main = file_uri(&root.join("main.aside")).unwrap();
    let mut workspace = Workspace::new(project_options());
    workspace.open(main.clone(), 1, source.into()).unwrap();
    let diagnostics = workspace.diagnostics(&main, Encoding::Utf16).unwrap();
    assert!(
        diagnostics
            .iter()
            .all(|params| params.diagnostics.is_empty()),
        "{diagnostics:?}"
    );
}

#[test]
fn disk_sources_are_shared_until_watched_change_and_project_creation_invalidates_roots() {
    let (_project, root) = project_sources();
    let helper = root.join("helper.aside");
    std::fs::write(&helper, "pub fn value()->int{1}").unwrap();
    let source = "use game::helper::value; fn main(){value();}";
    let mut workspace = Workspace::new(project_options());
    let a = file_uri(&root.join("ui/a.aside")).unwrap();
    let b = file_uri(&root.join("ui/b.aside")).unwrap();
    workspace.open(a.clone(), 1, source.into()).unwrap();
    assert!(
        workspace
            .diagnostics(&a, Encoding::Utf16)
            .unwrap()
            .iter()
            .any(|params| !params.diagnostics.is_empty())
    );
    let main = file_uri(&root.join("main.aside")).unwrap();
    workspace
        .open(main.clone(), 1, "fn main(){}".into())
        .unwrap();
    for uri in [&a, &b] {
        if uri == &b {
            workspace.open(b.clone(), 1, source.into()).unwrap();
        }
        assert!(
            workspace
                .diagnostics(uri, Encoding::Utf16)
                .unwrap()
                .iter()
                .all(|params| params.diagnostics.is_empty())
        );
    }
    assert_eq!(workspace.disk_reads, 1, "both entries share one disk parse");
    std::fs::write(&helper, "pub fn value()->string{\"changed\"}").unwrap();
    workspace.invalidate(&file_uri(&helper).unwrap());
    workspace.diagnostics(&a, Encoding::Utf16).unwrap();
    workspace.diagnostics(&b, Encoding::Utf16).unwrap();
    assert_eq!(
        workspace.disk_reads, 2,
        "watch invalidation replaces the one shared disk source"
    );
    workspace.close(&main);
    assert!(
        workspace
            .diagnostics(&a, Encoding::Utf16)
            .unwrap()
            .iter()
            .any(|params| !params.diagnostics.is_empty())
    );
    std::fs::write(root.join("main.aside"), "fn main(){}").unwrap();
    workspace.invalidate(&main);
    assert!(
        workspace
            .diagnostics(&a, Encoding::Utf16)
            .unwrap()
            .iter()
            .all(|params| params.diagnostics.is_empty())
    );
    assert_eq!(
        workspace.disk_reads, 2,
        "creating an entry must reuse unchanged source parses"
    );
    std::fs::remove_file(root.join("main.aside")).unwrap();
    workspace.invalidate(&main);
    assert!(
        workspace
            .diagnostics(&a, Encoding::Utf16)
            .unwrap()
            .iter()
            .any(|params| !params.diagnostics.is_empty())
    );
}

#[test]
fn formatting_reuses_syntax_and_preserves_negotiated_unicode_edit_ranges() {
    let source = "fn main(){let 文=\"😀\";unknown(1,2);}\r\n// 尾😀";
    for encoding in [Encoding::Utf8, Encoding::Utf16] {
        let mut workspace = Workspace::new(ServerOptions::default());
        workspace.open(uri(), 1, source.into()).unwrap();
        let edits = workspace.format(&uri(), encoding).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].range.start, Position::new(0, 0));
        let last_line = source.rsplit('\n').next().unwrap();
        let end = match encoding {
            Encoding::Utf8 => last_line.len(),
            Encoding::Utf16 => last_line.encode_utf16().count(),
        };
        assert_eq!(edits[0].range.end, Position::new(1, end as u32));
        assert!(edits[0].new_text.contains("\"😀\""));
        assert!(edits[0].new_text.contains("// 尾😀"));
        assert_eq!(
            workspace.analyses, 0,
            "formatting must not construct semantic facts"
        );
        assert_eq!(
            workspace.disk_reads, 0,
            "formatting must not resolve source imports"
        );
        assert_eq!(
            workspace.format(&uri(), encoding).unwrap(),
            edits,
            "formatting mutated the open document"
        );
        workspace
            .change(
                &uri(),
                2,
                vec![TextDocumentContentChangeEvent {
                    range: Some(edits[0].range),
                    range_length: None,
                    text: edits[0].new_text.clone(),
                }],
                encoding,
            )
            .unwrap();
        assert!(workspace.format(&uri(), encoding).unwrap().is_empty());
        assert!(
            workspace
                .diagnostics(&uri(), encoding)
                .unwrap()
                .iter()
                .any(|params| {
                    params
                        .diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.message.contains("unknown"))
                }),
            "formatting must preserve unresolved-name diagnostics"
        );
        workspace
            .change(
                &uri(),
                3,
                vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "fn main(){let 文=\"😀\";".into(),
                }],
                encoding,
            )
            .unwrap();
        assert!(
            workspace.format(&uri(), encoding).unwrap().is_empty(),
            "incomplete syntax must remain untouched"
        );
    }
}

fn response(connection: &Connection, id: i32) -> Response {
    loop {
        match connection
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        {
            Message::Response(response) if response.id == id.into() => return response,
            _ => {}
        }
    }
}
fn diagnostics(connection: &Connection) -> PublishDiagnosticsParams {
    loop {
        match connection
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        {
            Message::Notification(notification)
                if notification.method == "textDocument/publishDiagnostics" =>
            {
                return serde_json::from_value(notification.params).unwrap();
            }
            _ => {}
        }
    }
}
#[test]
fn protocol_connection_initializes_edits_queries_closes_and_exits() {
    let (server, client) = Connection::memory();
    let thread = std::thread::spawn(|| run_connection(server, ServerOptions::default()).unwrap());
    client
        .sender
        .send(
            Request::new(
                1.into(),
                "initialize".into(),
                json!({"capabilities":{"general":{"positionEncodings":["utf-16","utf-8"]}}}),
            )
            .into(),
        )
        .unwrap();
    let initialized = response(&client, 1).result.unwrap();
    assert_eq!(initialized["capabilities"]["positionEncoding"], "utf-8");
    assert_eq!(
        initialized["capabilities"]["documentFormattingProvider"],
        true
    );
    client
        .sender
        .send(Notification::new("initialized".into(), json!({})).into())
        .unwrap();
    client.sender.send(Notification::new("textDocument/didOpen".into(),json!({"textDocument":{"uri":uri(),"languageId":"aside","version":1,"text":"fn main(){中文;}"}})).into()).unwrap();
    let errors = diagnostics(&client);
    assert!(!errors.diagnostics.is_empty());
    client
        .sender
        .send(
            Request::new(
                5.into(),
                "textDocument/formatting".into(),
                json!({
                    "textDocument":{"uri":uri()},"options":{"tabSize":4,"insertSpaces":true}
                }),
            )
            .into(),
        )
        .unwrap();
    let edits = response(&client, 5).result.unwrap();
    assert_eq!(edits.as_array().unwrap().len(), 1);
    assert_eq!(
        edits[0]["range"]["end"],
        json!({"line":0,"character":"fn main(){中文;}".len()})
    );
    assert!(edits[0]["newText"].as_str().unwrap().contains("    中文;"));
    client.sender.send(Notification::new("textDocument/didChange".into(),json!({"textDocument":{"uri":uri(),"version":2},"contentChanges":[{"text":"fn main(){let 中文=1; 中文;}"}]})).into()).unwrap();
    assert!(diagnostics(&client).diagnostics.is_empty());
    client
        .sender
        .send(
            Request::new(
                2.into(),
                "textDocument/completion".into(),
                json!({"textDocument":{"uri":uri()},"position":{"line":0,"character":27}}),
            )
            .into(),
        )
        .unwrap();
    let items = response(&client, 2).result.unwrap();
    assert!(
        items
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["label"] == "中文")
    );
    client
        .sender
        .send(
            Request::new(
                4.into(),
                "aside/nativeDocument".into(),
                json!({"uri":"aside-native:/missing"}),
            )
            .into(),
        )
        .unwrap();
    assert_eq!(
        response(&client, 4).error.unwrap().code,
        lsp_server::ErrorCode::InvalidParams as i32
    );
    client.sender.send(Notification::new("textDocument/didChange".into(),json!({"textDocument":{"uri":uri(),"version":3},"contentChanges":[{"range":{"start":{"line":999,"character":0},"end":{"line":999,"character":0}},"text":"bad"}]})).into()).unwrap();
    assert!(
        diagnostics(&client)
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("invalid change start"))
    );
    client.sender.send(Notification::new("textDocument/didChange".into(),json!({"textDocument":{"uri":uri(),"version":4},"contentChanges":[{"text":"fn main(){}"}]})).into()).unwrap();
    assert!(diagnostics(&client).diagnostics.is_empty());
    client
        .sender
        .send(
            Notification::new(
                "textDocument/didClose".into(),
                json!({"textDocument":{"uri":uri()}}),
            )
            .into(),
        )
        .unwrap();
    assert!(diagnostics(&client).diagnostics.is_empty());
    client
        .sender
        .send(Request::new(3.into(), "shutdown".into(), Value::Null).into())
        .unwrap();
    assert!(response(&client, 3).error.is_none());
    client
        .sender
        .send(Notification::new("exit".into(), Value::Null).into())
        .unwrap();
    thread.join().unwrap();
}
