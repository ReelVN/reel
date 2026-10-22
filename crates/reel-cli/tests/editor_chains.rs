//! Exercise chain inference through the shipped Reel LSP and its real host declarations.
#[path = "../../aside-lsp/tests/support/stdio.rs"]
mod support;

use serde_json::{Value, json};
use support::Server;

const URI: &str = "untitled:creator-chains.aside";

fn position(source: &str, needle: &str, after: bool) -> Value {
    let offset = source.find(needle).unwrap() + if after { needle.len() } else { 0 };
    let prefix = &source[..offset];
    json!({
        "line": prefix.bytes().filter(|&byte| byte == b'\n').count(),
        "character": prefix.rsplit('\n').next().unwrap().encode_utf16().count(),
    })
}

fn query(source: &str, needle: &str, after: bool) -> Value {
    json!({"textDocument":{"uri":URI},"position":position(source,needle,after)})
}

fn open(source: &str) -> (Server, Value) {
    let executable = std::env::var_os("REEL_LSP_EXECUTABLE")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_reel-lsp").into());
    let mut server = Server::start(executable);
    let initialized = server.request(1, "initialize", json!({"capabilities":{}}));
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({"textDocument":{
            "uri":URI,"languageId":"aside","version":1,"text":source
        }}),
    );
    assert_eq!(server.diagnostics(URI, 1), json!([]));
    (server, initialized)
}

fn assert_method(
    server: &mut Server,
    id: &mut i32,
    source: &str,
    expression: &str,
    method: &str,
    result: &str,
) {
    let parameters = query(source, expression, false);
    let hover = server.request(*id, "textDocument/hover", parameters.clone());
    *id += 1;
    let markdown = hover["contents"]["value"].as_str().unwrap();
    let signature = markdown
        .strip_prefix("```aside\n")
        .unwrap()
        .split("\n```")
        .next()
        .unwrap();
    assert!(
        signature.contains(&format!("fn {method}("))
            && (signature.ends_with(&format!("-> {result}"))
                || signature.ends_with(&format!("-> reel::types::{result}"))),
        "{expression}: {markdown}"
    );
    let definition = server.request(*id, "textDocument/definition", parameters);
    *id += 1;
    assert!(
        definition["uri"]
            .as_str()
            .unwrap()
            .starts_with("aside-native://")
    );
    let document = server.request(
        *id,
        "aside/nativeDocument",
        json!({"uri":definition["uri"]}),
    );
    *id += 1;
    let line = document["text"]
        .as_str()
        .unwrap()
        .lines()
        .nth(definition["range"]["start"]["line"].as_u64().unwrap() as usize)
        .unwrap();
    let start = definition["range"]["start"]["character"].as_u64().unwrap() as usize;
    let end = definition["range"]["end"]["character"].as_u64().unwrap() as usize;
    assert_eq!(&line[start..end], method);
    assert!(line.contains(&format!("fn {method}(")), "{line}");
    assert!(
        line.ends_with(&format!("-> {result};"))
            || line.ends_with(&format!("-> reel::types::{result};")),
        "{expression} navigated to another receiver's declaration: {line}"
    );
}

fn assert_method_token(tokens: &Value, initialized: &Value, source: &str, expression: &str) {
    let expected = position(source, expression, false);
    let mut line = 0;
    let mut column = 0;
    for token in tokens["data"].as_array().unwrap().chunks_exact(5) {
        let delta_line = token[0].as_u64().unwrap();
        line += delta_line;
        column = if delta_line == 0 { column } else { 0 } + token[1].as_u64().unwrap();
        if line == expected["line"].as_u64().unwrap()
            && column == expected["character"].as_u64().unwrap()
        {
            let kind = token[3].as_u64().unwrap() as usize;
            assert_eq!(
                initialized["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"][kind],
                "method",
                "{expression}: {token:?}"
            );
            return;
        }
    }
    panic!("missing semantic method token at {expression}");
}

#[test]
fn action_and_speech_chains_infer_complete_navigate_and_color_real_methods() {
    let source = r#"fn main() {
    let label = text("雪😀");
    let plan = delay(0.1).then(tween(label, x: 12, duration: 0.2)).together(delay(0.3)).repeat(times: 2);
    let running = plan.run(paused: true);
    let held = running.pause();
    held.resume().stop();
    let sentence = narrator("A", wait: false).revealed().append(" tail", wait: false).wait();
    sentence.append(" more", wait: false).revealed();
    stage.replace(label);
}"#;
    let (mut server, initialized) = open(source);
    let mut id = 2;
    for (expression, method, result) in [
        ("then(tween", "then", "Action"),
        ("together(delay", "together", "Action"),
        ("repeat(times", "repeat", "Action"),
        ("run(paused", "run", "Activity"),
        ("pause();", "pause", "Activity"),
        ("resume()", "resume", "Activity"),
        ("stop();", "stop", "Activity"),
        ("revealed().append", "revealed", "Speech"),
        ("append(\" tail\"", "append", "Speech"),
        ("wait();", "wait", "Speech"),
    ] {
        assert_method(&mut server, &mut id, source, expression, method, result);
    }
    for (expression, names) in [
        ("plan.", &["then", "together", "repeat", "run"][..]),
        ("running.", &["pause", "resume", "stop", "finish"][..]),
        ("sentence.", &["append", "wait", "revealed"][..]),
    ] {
        let items = server.request(
            id,
            "textDocument/completion",
            query(source, expression, true),
        );
        id += 1;
        for &name in names {
            assert!(
                items
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item["label"] == name && item["kind"] == 2),
                "{expression} missing method {name}: {items}"
            );
        }
    }
    let tokens = server.request(
        id,
        "textDocument/semanticTokens/full",
        json!({"textDocument":{"uri":URI}}),
    );
    for expression in [
        "together(delay",
        "repeat(times",
        "revealed().append",
        "append(\" tail\"",
        "wait();",
    ] {
        assert_method_token(&tokens, &initialized, source, expression);
    }
    server.stop();
}

#[test]
fn node_with_chain_retains_the_actual_text_subtype_and_property_schema() {
    let source = r#"fn main() {
    let position = model({x: 12});
    let label = text("Hello").with(x: bind(position.x), font_size: 24, reveal: null);
    let styled: TextNode = label;
    styled.with(opacity: 0.5).value;
    let film = video("clip.mp4");
    film.with(opacity: 1);
    let dust = particles("spark.png");
    dust.with(x: 2);
    stage.replace(styled);
}"#;
    let (mut server, initialized) = open(source);
    let hover = server.request(2, "textDocument/hover", query(source, "label;", false));
    let markdown = hover["contents"]["value"].as_str().unwrap();
    assert!(
        markdown.contains("TextNode") && !markdown.contains("any"),
        "{markdown}"
    );
    let items = server.request(3, "textDocument/completion", query(source, "styled.", true));
    for name in ["with", "value", "font", "font_size"] {
        assert!(
            items
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == name),
            "{items}"
        );
    }
    let signature = server.request(
        4,
        "textDocument/signatureHelp",
        query(source, "font_size: 24", true),
    );
    let label = signature["signatures"][0]["label"].as_str().unwrap();
    assert!(
        label.contains("font_size?:") && label.contains("opacity?:"),
        "{label}"
    );
    let mut id = 5;
    assert_method(
        &mut server,
        &mut id,
        source,
        "with(opacity",
        "with",
        "TextNode",
    );
    let tokens = server.request(
        id,
        "textDocument/semanticTokens/full",
        json!({"textDocument":{"uri":URI}}),
    );
    assert_method_token(&tokens, &initialized, source, "with(opacity");
    id += 1;
    for expression in ["film.with(", "dust.with("] {
        let items = server.request(
            id,
            "textDocument/completion",
            query(source, expression, true),
        );
        id += 1;
        let items = items.as_array().unwrap();
        assert!(
            items.iter().any(|item| item["label"] == "reveal:"),
            "{items:?}"
        );
        for readonly in ["source:", "seed:"] {
            assert!(
                !items.iter().any(|item| item["label"] == readonly),
                "{expression} exposes {readonly}: {items:?}"
            );
        }
    }
    let invalid = source.replace("styled.with(opacity: 0.5)", "styled.with(opacity: null)");
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument":{"uri":URI,"version":2},"contentChanges":[{"text":invalid}]
        }),
    );
    let diagnostics = server.diagnostics(URI, 2);
    assert!(
        diagnostics
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["message"].as_str().unwrap().contains("null")),
        "{diagnostics}"
    );
    server.stop();
}
