//! Builtin service roots and imports navigate through the actual host registry.
use aside_compiler::semantic::TypeHint;
use serde_json::{Value, json};
#[path = "../../aside-lsp/tests/support/stdio.rs"]
mod support;
use support::Server;

const SERVICES: [&str; 8] = [
    "audio",
    "game",
    "localization",
    "mask",
    "persistent",
    "saves",
    "stage",
    "window",
];

#[test]
fn every_registered_module_and_global_service_has_a_generated_root_declaration() {
    let api = reel_engine::aside::source::api();
    let context = reel_cli::editor::host_context(api);
    for document in &context.documents {
        assert!(
            document
                .references
                .windows(2)
                .all(|references| references[0].range.end() <= references[1].range.start()),
            "overlapping references in {}",
            document.uri.as_str()
        );
    }
    let mut services = api
        .globals
        .iter()
        .filter(|symbol| symbol.namespace.is_some() && symbol.signature.is_none())
        .map(|symbol| symbol.name.as_str())
        .collect::<Vec<_>>();
    services.sort_unstable();
    assert_eq!(services, SERVICES);
    for symbol in api
        .globals
        .iter()
        .filter(|symbol| symbol.namespace.is_some() && symbol.signature.is_none())
    {
        let namespace = symbol.namespace.as_ref().unwrap();
        let reference = symbol
            .reference
            .as_ref()
            .expect("global namespace identity");
        assert_eq!(
            symbol.ty,
            TypeHint::Table,
            "{} uses the compiler's namespace table type",
            symbol.name
        );
        assert_eq!(reference.module, namespace.module);
        assert_eq!(reference.version, namespace.version);
        assert_eq!(reference.export, None);
    }
    for module in &api.modules {
        let declaration = context
            .declarations
            .iter()
            .find(|declaration| {
                declaration.reference.module == module.canonical_path
                    && declaration.reference.export.is_none()
            })
            .unwrap_or_else(|| panic!("missing module declaration for {}", module.canonical_path));
        assert_eq!(declaration.reference.version, module.version);
        let location = declaration.definition.as_ref().unwrap();
        let document = context
            .documents
            .iter()
            .find(|document| document.uri == location.uri)
            .unwrap();
        let name = module.canonical_path.rsplit("::").next().unwrap();
        assert_eq!(
            &document.text[usize::from(location.range.start())..usize::from(location.range.end())],
            name
        );
        assert!(
            document
                .text
                .contains(&format!("use {};", module.canonical_path)),
            "{}",
            document.text
        );
        if !module.exports.is_empty() {
            assert!(declaration.markdown.contains("Exports:"));
        }
        for export in &module.exports {
            assert!(
                declaration.markdown.contains(&export.name),
                "{} omitted {}",
                module.canonical_path,
                export.name
            );
        }
        if SERVICES.contains(&name) {
            assert!(
                api.namespaces
                    .iter()
                    .any(|namespace| namespace.documentation.len() > 30
                        && namespace.symbol.reference.as_ref().unwrap().module
                            == module.canonical_path)
            );
            for export in module
                .exports
                .iter()
                .filter(|export| export.signature.is_some())
            {
                assert!(
                    document.text.contains(&format!("fn {}(", export.name)),
                    "{} lacks {}",
                    location.uri.as_str(),
                    export.name
                );
            }
        }
    }
}

#[test]
fn service_signatures_refine_known_inputs_and_preserve_dynamic_font_and_data_forms() {
    let api = reel_engine::aside::source::api();
    let mut dynamic_parameters = Vec::new();
    let mut dynamic_results = Vec::new();
    for module in api.modules.iter().filter(|module| {
        module.canonical_path.starts_with("reel::")
            && SERVICES.contains(&module.canonical_path.rsplit("::").next().unwrap())
    }) {
        for export in &module.exports {
            let Some(signature) = &export.signature else {
                continue;
            };
            let canonical = aside_host::source::canonical(export);
            for parameter in &signature.parameters {
                if parameter.ty == TypeHint::Any {
                    dynamic_parameters.push(format!("{canonical}::{}", parameter.name));
                }
            }
            if signature.result == TypeHint::Any {
                dynamic_results.push(canonical);
            }
        }
    }
    dynamic_parameters.sort_unstable();
    dynamic_results.sort_unstable();
    assert_eq!(
        dynamic_parameters,
        [
            "reel::game::configure::fonts",
            "reel::persistent::read::default",
            "reel::persistent::write::value"
        ]
    );
    assert_eq!(dynamic_results, ["reel::stage::transition"]);
    for fonts in [
        "null",
        "\"font.ttf\"",
        "[\"font.ttf\"]",
        "(\"font.ttf\", \"fallback.ttf\")",
    ] {
        let source = format!(
            "fn main(){{game.configure(title:null,width:960,height:540,text_speed:30,fonts:{fonts});}}"
        );
        reel_engine::aside::source::compile_story(&source, "game::main", 1, "main").unwrap();
    }
    let source = "fn main(){game.reading().set_auto(enabled:true,delay:1,condition:derive(||true));game.reading().set_skip(false);game.reading().pause(false);let operation=saves.save(\"slot\",title:\"Saved\",thumbnail:null);operation.status;operation.error;operation.restored;let data=persistent.read(\"key\",default:0);persistent.write(\"key\",{value:1});stage.transition(||null);}";
    reel_engine::aside::source::compile_story(source, "game::main", 1, "main").unwrap();
    for body in [
        "game.configure(title:12);",
        "game.configure(width:1.5);",
        "game.reading().set_auto(enabled:1);",
        "saves.save(1);",
        "persistent.read(1);",
        "stage.transition(1);",
    ] {
        let source = format!("fn main(){{{body}}}");
        assert!(
            matches!(
                reel_engine::aside::source::compile_story(&source, "game::main", 1, "main"),
                Err(reel_engine::aside::source::SourceError::Analysis(_))
            ),
            "expected a source type diagnostic for {body}"
        );
    }
    let context = reel_cli::editor::host_context(api);
    let text = api.native("reel::text");
    let projected = context
        .declarations
        .iter()
        .find(|declaration| Some(&declaration.reference) == text.symbol.reference.as_deref())
        .unwrap();
    assert_eq!(projected.property_parameters, text.property_parameters);
    assert!(
        projected
            .property_parameters
            .iter()
            .any(|name| name == "font")
    );
    assert!(projected.property_parameters.iter().any(|name| name == "x"));
    assert!(
        api.native("reel::game::configure")
            .property_parameters
            .is_empty()
    );
}

fn query(source: &str, needle: &str) -> Value {
    let offset = source.find(needle).unwrap();
    let before = &source[..offset];
    let line = before.bytes().filter(|&byte| byte == b'\n').count();
    let column = before.rsplit('\n').next().unwrap().encode_utf16().count();
    json!({"textDocument":{"uri":"untitled:services.aside"},"position":{"line":line,"character":column}})
}

#[test]
fn global_services_and_standard_module_aliases_hover_and_navigate_in_the_real_lsp() {
    let source = "use std::time as clock;\nfn main(){\n game.configure(title: \"Example\", width: 960, height: 540);\n stage.replace(text(\"Hello\"));\n audio.play(\"sound.ogg\");\n localization.locale();\n saves.list();\n persistent.read(\"setting\");\n window.info();\n mask.gray(\"mask.png\");\n clock.now();\n}";
    let executable = std::env::var_os("REEL_LSP_EXECUTABLE")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_reel-lsp").into());
    let mut client = Server::start(executable);
    client.request(1, "initialize", json!({"capabilities":{}}));
    client.notify("initialized", json!({}));
    client.notify("textDocument/didOpen", json!({"textDocument":{"uri":"untitled:services.aside","languageId":"aside","version":1,"text":source}}));
    let diagnostics = client.diagnostics("untitled:services.aside", 1);
    assert!(
        !diagnostics
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["severity"] == 1),
        "{diagnostics}"
    );
    let mut id = 2;
    for service in SERVICES.into_iter().chain(["clock"]) {
        let parameters = query(source, &format!("{service}."));
        let hover = client.request(id, "textDocument/hover", parameters.clone());
        id += 1;
        let contents = hover["contents"]["value"].as_str().unwrap();
        let module = if service == "clock" {
            "std::time".to_string()
        } else {
            format!("reel::{service}")
        };
        assert!(
            contents.contains(&format!("use {module}")),
            "{service}: {contents}"
        );
        assert!(contents.contains("Exports:"), "{service}: {contents}");
        let definition = client.request(id, "textDocument/definition", parameters);
        id += 1;
        let document = client.request(id, "aside/nativeDocument", json!({"uri":definition["uri"]}));
        id += 1;
        let line = document["text"]
            .as_str()
            .unwrap()
            .lines()
            .nth(definition["range"]["start"]["line"].as_u64().unwrap() as usize)
            .unwrap();
        let start = definition["range"]["start"]["character"].as_u64().unwrap() as usize;
        let end = definition["range"]["end"]["character"].as_u64().unwrap() as usize;
        let actual_name = module.rsplit("::").next().unwrap();
        assert_eq!(&line[start..end], actual_name);
        assert!(line.contains(&format!("use {module};")));
        if service == "game" {
            let text = document["text"].as_str().unwrap();
            assert!(
                text.contains("fn configure(\n")
                    && text.contains("    title:")
                    && text.contains("Set title, logical canvas size")
            );
        }
    }
    let text = reel_engine::aside::source::api().native("reel::text");
    let signature = text.symbol.signature.as_ref().unwrap();
    let hidden_properties = signature
        .parameters
        .iter()
        .filter(|parameter| parameter.default && text.property_parameters.contains(&parameter.name))
        .count();
    let hover = client.request(id, "textDocument/hover", query(source, "text(\"Hello\")"));
    id += 1;
    let contents = hover["contents"]["value"].as_str().unwrap();
    let label = contents.lines().nth(1).unwrap();
    assert!(label.starts_with("fn text(value:"), "{label}");
    assert!(!label.contains("x:") && !label.contains("font:"), "{label}");
    assert!(
        contents.contains(&format!(
            "Additional named properties: {hidden_properties}."
        )),
        "{contents}"
    );
    let definition = client.request(
        id,
        "textDocument/definition",
        query(source, "text(\"Hello\")"),
    );
    id += 1;
    let document = client.request(id, "aside/nativeDocument", json!({"uri":definition["uri"]}));
    let full = document["text"].as_str().unwrap();
    assert!(
        full.contains("x:")
            && full.contains("font:")
            && full.contains("fn text(\n")
            && full.contains("    value:")
    );
    client.stop();
}
