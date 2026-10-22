//! Query real module prefixes through the shipped LSP and generated host source.
#[path = "../../aside-lsp/tests/support/stdio.rs"]
mod support;
use serde_json::{Value, json};
use std::{fmt::Write, path::Path};
use support::Server;

fn file_uri(path: &Path) -> String {
    let path = path.to_str().unwrap().replace('\\', "/");
    let mut uri = String::from(if path.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/:-._~".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            write!(uri, "%{byte:02X}").unwrap();
        }
    }
    uri
}

fn position(source: &str, needle: &str) -> Value {
    let before = &source[..source.find(needle).unwrap()];
    json!({"line":before.bytes().filter(|byte| *byte == b'\n').count(),
        "character":before.rsplit('\n').next().unwrap().encode_utf16().count()})
}
fn query(uri: &str, source: &str, needle: &str) -> Value {
    json!({"textDocument":{"uri":uri},"position":position(source,needle)})
}
fn open(server: &mut Server, uri: &str, source: &str, version: i32) {
    server.notify(
        "textDocument/didOpen",
        json!({"textDocument":{
        "uri":uri,"languageId":"aside","version":version,"text":source}}),
    );
}
fn start() -> Server {
    let mut server = Server::start(env!("CARGO_BIN_EXE_reel-lsp"));
    server.request(1, "initialize", json!({"capabilities":{}}));
    server.notify("initialized", json!({}));
    server
}
fn document_line<'a>(document: &'a Value, definition: &Value) -> &'a str {
    document["text"]
        .as_str()
        .unwrap()
        .lines()
        .nth(definition["range"]["start"]["line"].as_u64().unwrap() as usize)
        .unwrap()
}

#[test]
fn standard_module_prefixes_open_the_registered_generated_documents() {
    let uri = "untitled:module-prefixes.aside";
    let source = "use std::math::{sin,curve}; fn main(){let value=sin(0.5);}";
    let mut server = start();
    open(&mut server, uri, source, 1);
    assert_eq!(server.diagnostics(uri, 1), json!([]));
    let mut id = 2;
    let mut evidence = Vec::new();
    for (needle, path) in [("std::math", "std"), ("math::{", "std::math")] {
        let params = query(uri, source, needle);
        let hover = server.request(id, "textDocument/hover", params.clone());
        id += 1;
        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains(&format!("use {path}")),
            "{hover}"
        );
        let definition = server.request(id, "textDocument/definition", params);
        id += 1;
        assert_eq!(
            definition["uri"],
            format!(
                "aside-native://reel/modules/{}.aside",
                path.replace("::", "/")
            )
        );
        let document = server.request(id, "aside/nativeDocument", json!({"uri":definition["uri"]}));
        id += 1;
        assert_eq!(
            document_line(&document, &definition),
            format!("use {path};")
        );
        evidence.push(json!({"path":path,"hover":hover,"definition":definition}));
        open(
            &mut server,
            definition["uri"].as_str().unwrap(),
            document["text"].as_str().unwrap(),
            1,
        );
    }
    let leaf = server.request(id, "textDocument/definition", query(uri, source, "sin,"));
    id += 1;
    let document = server.request(id, "aside/nativeDocument", json!({"uri":leaf["uri"]}));
    id += 1;
    assert!(document_line(&document, &leaf).starts_with("pub fn sin("));
    // An opened generated module keeps its own prefix navigation, with no source copy.
    let generated_uri = leaf["uri"].as_str().unwrap();
    let prefix = server.request(
        id,
        "textDocument/definition",
        query(
            generated_uri,
            document["text"].as_str().unwrap(),
            "std::math;",
        ),
    );
    assert_eq!(prefix["uri"], "aside-native://reel/modules/std.aside");
    println!(
        "{}",
        json!({"standard_module_navigation":evidence,"leaf":leaf})
    );
    server.stop();
}

#[test]
fn local_directory_indexes_retain_project_roots_and_unsaved_source_navigation() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().join("src");
    std::fs::create_dir_all(root.join("ui")).unwrap();
    std::fs::write(root.join("main.aside"), "fn main(){}").unwrap();
    let dialogue = root.join("ui/dialogue.aside");
    std::fs::write(&dialogue, "pub fn speak(){}").unwrap();
    let main_uri = file_uri(&root.join("main.aside"));
    let draft_uri = file_uri(&root.join("ui/draft.aside"));
    let source = "use game::ui::dialogue::{speak}; fn main(){speak();}";
    let mut server = start();
    open(&mut server, &draft_uri, "pub let work=1;", 1);
    open(&mut server, &main_uri, source, 1);
    assert_eq!(server.diagnostics(&main_uri, 1), json!([]));
    let definition = server.request(
        2,
        "textDocument/definition",
        query(&main_uri, source, "ui::"),
    );
    let index_uri = definition["uri"].as_str().unwrap();
    let document = server.request(3, "aside/nativeDocument", json!({"uri":index_uri}));
    let text = document["text"].as_str().unwrap();
    assert!(text.contains("use game::ui::dialogue;"));
    assert!(text.contains("use game::ui::draft;"));
    open(&mut server, index_uri, text, 1);
    assert_eq!(server.diagnostics(index_uri, 1), json!([]));
    let file = server.request(
        4,
        "textDocument/definition",
        query(index_uri, text, "dialogue;"),
    );
    assert!(
        file["uri"]
            .as_str()
            .unwrap()
            .eq_ignore_ascii_case(&file_uri(&dialogue)),
        "{file}"
    );
    let unsaved = server.request(
        5,
        "textDocument/definition",
        query(index_uri, text, "draft;"),
    );
    assert_eq!(unsaved["uri"], draft_uri);
    // Empty nested groups discover exports before any leaf identifier is typed.
    let source = "use game::{ui::{dialogue::{ ";
    server.notify(
        "textDocument/didChange",
        json!({"textDocument":{"uri":main_uri,"version":2},"contentChanges":[{"text":source}]}),
    );
    let at =
        json!({"textDocument":{"uri":main_uri},"position":{"line":0,"character":source.len()}});
    let completion = server.request(6, "textDocument/completion", at);
    assert!(
        completion
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["label"] == "speak")
    );
    println!(
        "{}",
        json!({"directory":definition,"file":file,"unsaved":unsaved,"empty_group_completion":completion})
    );
    server.stop();
}
