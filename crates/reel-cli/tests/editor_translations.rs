//! Inline translations use the shipped LSP's compiler-backed source features.
#[path = "../../aside-lsp/tests/support/stdio.rs"]
mod support;
use serde_json::{Value, json};
use support::Server;
const URI: &str = "untitled:inline-translations.aside";
fn position(source: &str, needle: &str, inside: usize) -> Value {
    let offset = source.find(needle).unwrap() + inside;
    let prefix = &source[..offset];
    json!({"line":prefix.bytes().filter(|&b|b==b'\n').count(),"character":prefix.rsplit('\n').next().unwrap().encode_utf16().count()})
}
fn query(source: &str, needle: &str, inside: usize) -> Value {
    json!({"textDocument":{"uri":URI},"position":position(source,needle,inside)})
}
fn open(source: &str) -> Server {
    let executable = std::env::var_os("REEL_LSP_EXECUTABLE")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_reel-lsp").into());
    let mut server = Server::start(executable);
    server.request(1, "initialize", json!({"capabilities":{}}));
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({"textDocument":{"uri":URI,"languageId":"aside","version":1,"text":source}}),
    );
    assert_eq!(server.diagnostics(URI, 1), json!([]));
    server
}
fn change(server: &mut Server, source: &str, version: i32) -> Value {
    server.notify(
        "textDocument/didChange",
        json!({"textDocument":{"uri":URI,"version":version},"contentChanges":[{"text":source}]}),
    );
    server.diagnostics(URI, version)
}
#[test]
fn source_and_every_variant_preview_together_while_interpolation_keeps_real_type_hover() {
    let source = r#"fn main(){
        let name="雪😀";let count=2;let line=tr(f"你好{name}，{count}封信。",{en:f"Hello {name}, {count} letters.",ja:f"{name}へ、{count}通。"});
        stage.replace(text(line));
    }"#;
    let mut server = open(source);
    let mut preview = None;
    for (id, needle) in [(2, "你好"), (3, "Hello "), (4, "へ、")] {
        let hover = server.request(id, "textDocument/hover", query(source, needle, 0));
        let markdown = hover["contents"]["value"].as_str().unwrap();
        for expected in [
            "**Source**",
            "你好{name}",
            "**en**",
            "Hello {name}",
            "**ja**",
            "{name}へ",
        ] {
            assert!(markdown.contains(expected), "{markdown}");
        }
        assert!(
            !markdown.contains("雪😀"),
            "preview evaluated the author's variable: {markdown}"
        );
        if let Some(previous) = &preview {
            assert_eq!(previous, markdown);
        } else {
            preview = Some(markdown.to_owned());
        }
    }
    for (id, needle, inside) in [(5, "{name}", 1), (6, "Hello {name}", 7), (7, "{count}", 1)] {
        let hover = server.request(id, "textDocument/hover", query(source, needle, inside));
        let markdown = hover["contents"]["value"].as_str().unwrap();
        assert!(
            !markdown.contains("**Source**"),
            "expression lost ordinary hover: {markdown}"
        );
        assert!(
            markdown.contains(if needle.contains("count") {
                "int"
            } else {
                "string"
            }),
            "{markdown}"
        );
    }
    server.stop();
}
#[test]
fn locale_completion_inserts_valid_aside_and_shadowed_calls_keep_normal_editor_behavior() {
    let source = r#"fn main(){let line=tr("Hello",{en:"Hi", /*locale*/});stage.replace(text(line));}
    fn local(){let tr=|value,translations|value;tr("Local prose",{ /*local*/ });}"#;
    let mut server = open(source);
    let items = server.request(2, "textDocument/completion", query(source, "/*locale*/", 0));
    let items = items.as_array().unwrap();
    assert!(!items.iter().any(|item| item["label"] == "en"), "{items:?}");
    let chinese = items
        .iter()
        .find(|item| item["label"] == "zh-CN")
        .expect("canonical locale completion");
    assert_eq!(chinese["detail"], "Inline translation");
    let inserted = chinese["insertText"].as_str().unwrap();
    let completed = source.replace("/*locale*/", inserted);
    assert_eq!(
        change(&mut server, &completed, 2),
        json!([]),
        "inserted {inserted:?}"
    );
    let ordinary = server.request(3, "textDocument/hover", query(&completed, "Local prose", 0));
    assert!(ordinary.is_null(), "{ordinary}");
    let items = server.request(
        4,
        "textDocument/completion",
        query(&completed, "/*local*/", 0),
    );
    assert!(
        !items
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["detail"] == "Inline translation"),
        "{items}"
    );
    server.stop();
}
#[test]
fn edited_translation_diagnostics_keep_the_exact_author_expression_and_recover() {
    let source = r#"fn main(){
        let name="Mio";let other="Nia";
        let line=tr(f"Value {name}",{en:f"Hi {name}"});stage.replace(text(line));
    }"#;
    let mut server = open(source);
    let invalid = source.replace("Hi {name}", "Hi {other}");
    let diagnostics = change(&mut server, &invalid, 2);
    let expected = position(&invalid, "other}", 0);
    assert!(
        diagnostics
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["severity"] == 1
                && d["range"]["start"] == expected
                && d["message"]
                    .as_str()
                    .unwrap()
                    .contains("translation interpolation")),
        "{diagnostics}"
    );
    let hover = server.request(2, "textDocument/hover", query(&invalid, "Value ", 0));
    assert!(
        hover["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("Value {name}")
    );
    assert_eq!(change(&mut server, source, 3), json!([]));
    server.stop();
}
