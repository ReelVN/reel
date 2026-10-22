//! Follow the registered API through real read-only declaration views over stdio.
#[path = "../../aside-lsp/tests/support/stdio.rs"]
mod support;
use serde_json::{Value, json};
use support::Server;

fn query(uri: &str, source: &str, needle: &str) -> Value {
    let before = &source[..source
        .find(needle)
        .unwrap_or_else(|| panic!("missing {needle} in {source}"))];
    json!({"textDocument":{"uri":uri},"position":{
        "line":before.bytes().filter(|byte| *byte == b'\n').count(),
        "character":before.rsplit('\n').next().unwrap().encode_utf16().count()}})
}
fn open(server: &mut Server, uri: &str, source: &str) {
    server.notify(
        "textDocument/didOpen",
        json!({"textDocument":{
        "uri":uri,"languageId":"aside","version":1,"text":source}}),
    );
    assert_eq!(server.diagnostics(uri, 1), json!([]));
}
fn definition(server: &mut Server, id: &mut i32, uri: &str, source: &str, needle: &str) -> Value {
    *id += 1;
    let result = server.request(*id, "textDocument/definition", query(uri, source, needle));
    assert!(
        result["uri"]
            .as_str()
            .is_some_and(|uri| uri.starts_with("aside-native://")),
        "{needle}: {result}"
    );
    result
}
fn document(server: &mut Server, id: &mut i32, definition: &Value) -> (String, String) {
    *id += 1;
    let result = server.request(
        *id,
        "aside/nativeDocument",
        json!({"uri":definition["uri"]}),
    );
    let uri = definition["uri"].as_str().unwrap().to_owned();
    let text = result["text"].as_str().unwrap().to_owned();
    open(server, &uri, &text);
    (uri, text)
}
fn selected<'a>(text: &'a str, definition: &Value) -> &'a str {
    let line = text
        .lines()
        .nth(definition["range"]["start"]["line"].as_u64().unwrap() as usize)
        .unwrap();
    &line[definition["range"]["start"]["character"].as_u64().unwrap() as usize
        ..definition["range"]["end"]["character"].as_u64().unwrap() as usize]
}

#[test]
fn host_type_imports_and_generated_signatures_keep_complete_navigation() {
    let mut server = Server::start(env!("CARGO_BIN_EXE_reel-lsp"));
    server.request(1, "initialize", json!({"capabilities":{}}));
    server.notify("initialized", json!({}));
    let uri = "untitled:registered-types.aside";
    let source = "use reel::types::{Node, TextNode};\nfn title(node: Node) -> TextNode { text(\"标题\") }\nfn main() { title(text(\"Hello\")); }";
    open(&mut server, uri, source);
    let mut id = 1;
    for needle in ["TextNode};", "TextNode {", "Node)"] {
        let target = definition(&mut server, &mut id, uri, source, needle);
        let (_, text) = document(&mut server, &mut id, &target);
        assert_eq!(
            selected(&text, &target),
            if needle.starts_with("Text") {
                "TextNode"
            } else {
                "Node"
            }
        );
    }
    let constructor = definition(&mut server, &mut id, uri, source, "text(\"标题\")");
    let (constructor_uri, constructor_text) = document(&mut server, &mut id, &constructor);
    assert!(constructor_text.contains("pub fn text(\n"));
    assert!(constructor_text.contains("// Registered node properties."));
    assert!(constructor_text.contains("    x: "));
    assert!(constructor_text.contains("    opacity: "));
    let text_type = definition(
        &mut server,
        &mut id,
        &constructor_uri,
        &constructor_text,
        "TextNode;",
    );
    let (type_uri, type_text) = document(&mut server, &mut id, &text_type);
    assert_eq!(selected(&type_text, &text_type), "TextNode");
    let base = definition(&mut server, &mut id, &type_uri, &type_text, "Node {");
    let (base_uri, base_text) = document(&mut server, &mut id, &base);
    assert!(base_uri.ends_with("/Node.aside"));
    assert_eq!(selected(&base_text, &base), "Node");
    id += 1;
    let hover = server.request(
        id,
        "textDocument/hover",
        query(&type_uri, &type_text, "Node {"),
    );
    let markdown = hover["contents"]["value"].as_str().unwrap();
    assert!(markdown.contains("class reel::types::Node"), "{markdown}");
    assert!(markdown.contains("shared layout"), "{markdown}");

    let module = definition(&mut server, &mut id, uri, source, "types::{");
    let (module_uri, module_text) = document(&mut server, &mut id, &module);
    assert!(module_text.contains("pub use reel::types::TextNode;"));
    let exported_type = definition(&mut server, &mut id, &module_uri, &module_text, "TextNode;");
    assert_eq!(exported_type, text_type);
    server.stop();
}
