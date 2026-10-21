//! Verify author typing against the actual Reel language-server executable.
#[path = "../../aside-lsp/tests/support/stdio.rs"]
mod support;
use serde_json::json;
use support::Server;

#[test]
fn typed_prefixes_offer_registered_reel_names_in_unfinished_source() {
    let executable = std::env::var_os("REEL_LSP_EXECUTABLE")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_reel-lsp").into());
    let mut server = Server::start(executable);
    server.request(1, "initialize", json!({"capabilities":{}}));
    server.notify("initialized", json!({}));
    for (index, source) in ["tex", "fn main() { tex", "fn main() { let label = tex"]
        .into_iter()
        .enumerate()
    {
        let uri = format!("untitled:typing-{index}.aside");
        server.notify(
            "textDocument/didOpen",
            json!({"textDocument":{
                "uri":uri,"languageId":"aside","version":1,"text":source
            }}),
        );
        server.diagnostics(&uri, 1);
        let items = server.request(
            2 + index as i32,
            "textDocument/completion",
            json!({
                "textDocument":{"uri":uri},"position":{"line":0,"character":source.len()}
            }),
        );
        assert!(
            items
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == "text"),
            "text missing while typing {source:?}: {items}"
        );
        eprintln!("{source:?}: text completion returned");
        server.notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}));
    }
    server.stop();
}

#[test]
#[ignore = "set REEL_EDITOR_SNIPPETS to the expanded editor snippet fixtures"]
fn shipped_editor_snippets_use_registered_author_interfaces() {
    let path = std::env::var_os("REEL_EDITOR_SNIPPETS").unwrap();
    let cases: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let source = case["source"].as_str().unwrap();
        reel_cli::check(source, "snippet.aside")
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        eprintln!("{name}: compiled");
    }
}
