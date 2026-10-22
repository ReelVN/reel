//! Exercise chain inference through the shipped Reel LSP and its real host declarations.
#[path = "../../aside-lsp/tests/support/stdio.rs"]
mod support;

use serde_json::{Value, json};
use support::Server;

const URI: &str = "untitled:creator-chains.aside";

#[test]
fn geometry_await_infers_records_and_navigates_generated_readonly_fields() {
    let source = r#"use reel::types::Point;
        fn main() {
            let request = stage.geometry(text("雪"));
            let geometry = await request;
            let width = geometry.bounds.width;
            let translation = geometry.transform.tx;
            let endpoint = geometry.text_end;
            if endpoint is Point {let y = endpoint.y;}
        }"#;
    let (mut server, _) = open(source);
    let mut id = 2;
    for (expression, expected) in [
        ("request;", "GeometryRequest"),
        ("geometry.bounds", "Geometry"),
        ("bounds.width", "Bounds"),
        ("transform.tx", "Transform"),
        ("text_end;", "Point | null"),
    ] {
        let hover = server.request(id, "textDocument/hover", query(source, expression, false));
        id += 1;
        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains(expected),
            "{expression}: {hover}"
        );
    }
    for (expression, field, description) in [
        (
            "geometry.bounds.",
            "width",
            "Width of the transformed axis-aligned border box.",
        ),
        (
            "geometry.transform.",
            "tx",
            "Horizontal translation in reference coordinates.",
        ),
        (
            "endpoint.",
            "y",
            "Vertical coordinate in the query's reference space.",
        ),
    ] {
        let completion = server.request(
            id,
            "textDocument/completion",
            query(source, expression, true),
        );
        id += 1;
        assert!(
            completion
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == field),
            "{completion}"
        );
        let definition = server.request(
            id,
            "textDocument/definition",
            query(source, &format!("{expression}{field}"), true),
        );
        id += 1;
        let document = server.request(id, "aside/nativeDocument", json!({"uri":definition["uri"]}));
        id += 1;
        let text = document["text"].as_str().unwrap();
        let declaration = text
            .lines()
            .nth(definition["range"]["start"]["line"].as_u64().unwrap() as usize)
            .unwrap();
        assert!(
            declaration.contains(&format!("{field}: float")),
            "{declaration}"
        );
        assert!(
            !declaration.contains('='),
            "snapshot fields have no constructor default: {declaration}"
        );
        assert!(text.contains(description), "{text}");
    }
    server.stop();
}

#[test]
fn is_guard_exposes_real_particle_type_inside_owned_closure() {
    let source = r#"use reel::types::{Node,ParticlesNode};
        fn spray(node:Node) {
            if node is ParticlesNode {
                spawn(|| {emit(node,burst:1); let seed=node.seed;},owner:node);
            }
            node.with(opacity:1);
        }"#;
    let (mut server, _) = open(source);
    let hover = server.request(2, "textDocument/hover", query(source, "node.seed", false));
    assert!(
        hover["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("ParticlesNode"),
        "{hover}"
    );
    let inside = server.request(
        3,
        "textDocument/completion",
        query(source, "node.seed", true),
    );
    assert!(
        inside
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["label"] == "seed"),
        "{inside}"
    );
    let outside = server.request(
        4,
        "textDocument/completion",
        query(source, "node.with", true),
    );
    assert!(
        outside
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["label"] != "seed"),
        "{outside}"
    );
    let definition = server.request(5, "textDocument/definition", query(source, "seed;", false));
    assert!(
        definition["uri"]
            .as_str()
            .unwrap()
            .starts_with("aside-native://"),
        "{definition}"
    );
    server.stop();
}

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
    let source = r##"fn main() {
    let position = model({x: 12});
    let label = text("Hello").with(x: bind(position.x), font_size: 24, reveal: null,
        border_width: 2, border_color: "#92a8c8", corner_radius: 12);
    let styled: TextNode = label;
    styled.with(opacity: 0.5).value;
    let film = video("clip.mp4");
    film.with(opacity: 1);
    let dust = particles("spark.png");
    dust.with(x: 2);
    let menu = popup().with(placement: "top-end", offset: 4);
    let floating: PopupNode = menu;
    floating.with(offset: 8);
    styled.add(floating);
    stage.replace(styled);
}"##;
    let (mut server, initialized) = open(source);
    let hover = server.request(2, "textDocument/hover", query(source, "label;", false));
    let markdown = hover["contents"]["value"].as_str().unwrap();
    assert!(
        markdown.contains("TextNode") && !markdown.contains("any"),
        "{markdown}"
    );
    let items = server.request(3, "textDocument/completion", query(source, "styled.", true));
    for name in [
        "with",
        "value",
        "font",
        "font_size",
        "border_width",
        "border_color",
        "corner_radius",
    ] {
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
        label.contains("font_size?:")
            && label.contains("opacity?:")
            && label.contains("border_width?:")
            && label.contains("corner_radius?:"),
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
    assert_method(
        &mut server,
        &mut id,
        source,
        "with(offset:",
        "with",
        "PopupNode",
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

#[test]
fn reading_receiver_chains_keep_context_type_and_removed_globals_stay_absent() {
    let source = r#"use reel::types::Reading;
fn main() {
    let controls:Reading=game.reading();
    controls.set_auto(true,delay:3).pause(false).set_skip(false).clear_history(keep_last:10);
    reading(body:|page| {page.finish("title");});
}"#;
    let (mut server, initialized) = open(source);
    let mut id = 2;
    for (expression, method) in [
        ("set_auto(true", "set_auto"),
        ("pause(false", "pause"),
        ("set_skip(false", "set_skip"),
        ("clear_history(keep_last", "clear_history"),
    ] {
        assert_method(&mut server, &mut id, source, expression, method, "Reading");
    }
    let methods = server.request(
        id,
        "textDocument/completion",
        query(source, "controls.", true),
    );
    id += 1;
    for name in [
        "auto",
        "paused",
        "auto_ready",
        "delay",
        "set_auto",
        "set_skip",
        "pause",
        "clear_history",
    ] {
        assert!(
            methods
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == name),
            "{methods}"
        );
    }
    let globals = server.request(id, "textDocument/completion", query(source, "game.", true));
    id += 1;
    for name in ["set_auto", "set_skip", "pause_reading", "clear_history"] {
        assert!(
            !globals
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == name),
            "{globals}"
        );
    }
    let tokens = server.request(
        id,
        "textDocument/semanticTokens/full",
        json!({"textDocument":{"uri":URI}}),
    );
    for expression in [
        "set_auto(true",
        "pause(false",
        "set_skip(false",
        "clear_history(keep_last",
    ] {
        assert_method_token(&tokens, &initialized, source, expression);
    }
    server.stop();
}

#[test]
fn standard_random_and_curve_types_offer_real_method_signatures_and_navigation() {
    let source = r#"use std::{random, Random};
use std::math::{curve, Curve};
fn draw(value: Random) -> int { value.int(min: 0, max: 10) }
fn sample(value: Curve) -> float { value.sample(time: 0.25) }
fn main() {
    let rng: Random = random(seed: 42);
    let number = rng.float();
    let item = rng.pick(items: (1, 2));
    let path: Curve = curve([{time: 0, value: 0}, {time: 1, value: 10}]);
    path.sample(0.5);
}"#;
    let (mut server, initialized) = open(source);
    let mut id = 2;
    for (expression, method, result) in [
        ("int(min:", "int", "int"),
        ("float();", "float", "float"),
        ("pick(items:", "pick", "any"),
        ("sample(time:", "sample", "float"),
    ] {
        assert_method(&mut server, &mut id, source, expression, method, result);
    }
    for (expression, names) in [
        ("rng.", &["int", "float", "pick"][..]),
        ("path.", &["sample"][..]),
    ] {
        let items = server.request(
            id,
            "textDocument/completion",
            query(source, expression, true),
        );
        id += 1;
        for name in names {
            assert!(
                items
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item["label"] == *name && item["kind"] == 2),
                "{expression}: {items}"
            );
        }
    }
    let hover = server.request(
        id,
        "textDocument/hover",
        query(source, "pick(items:", false),
    );
    id += 1;
    let markdown = hover["contents"]["value"].as_str().unwrap();
    assert!(markdown.contains("items: array | tuple"), "{markdown}");
    let tokens = server.request(
        id,
        "textDocument/semanticTokens/full",
        json!({"textDocument":{"uri":URI}}),
    );
    for expression in ["int(min:", "float();", "pick(items:", "sample(time:"] {
        assert_method_token(&tokens, &initialized, source, expression);
    }
    server.stop();
}

#[test]
fn text_editor_policy_fields_share_typed_constructor_and_with_declarations() {
    let source = r##"fn main() {
    let editor = text_input(placeholder: "Name", read_only: true, password: true,
        selection_color: "#10203040", caret_color: null);
    editor.with(placeholder: "New hint", read_only: false, caret_color: "#aabbcc");
    editor.read_only = false;
    editor.password = false;
    let notes = text_area(placeholder: "Notes", read_only: true);
    notes.with(caret_color: null);
    let label = text("Label");
    label.with(color: "#ffffff");
    let fresh = text_input();
    let sheet = text_area();
    let editor_probe = editor;
    let notes_probe = notes;
    editor_probe.with();
    notes_probe.with();
}"##;
    let (mut server, _) = open(source);
    let mut id = 2;
    for (expression, result) in [
        ("with(placeholder:", "TextInputNode"),
        ("with(caret_color:", "TextInputNode"),
    ] {
        assert_method(&mut server, &mut id, source, expression, "with", result);
    }
    for expression in [
        "fresh = text_input(",
        "sheet = text_area(",
        "editor_probe.with(",
        "notes_probe.with(",
    ] {
        let items = server.request(
            id,
            "textDocument/completion",
            query(source, expression, true),
        );
        id += 1;
        for name in [
            "placeholder:",
            "read_only:",
            "password:",
            "selection_color:",
            "caret_color:",
        ] {
            assert!(
                items.as_array().unwrap().iter().any(|i| i["label"] == name),
                "{expression} is missing {name}"
            );
        }
    }
    let items = server.request(
        id,
        "textDocument/completion",
        query(source, "label.with(", true),
    );
    id += 1;
    assert!(
        !items
            .as_array()
            .unwrap()
            .iter()
            .any(|i| matches!(i["label"].as_str(), Some("read_only:" | "password:"))),
        "{items}"
    );
    let hover = server.request(id, "textDocument/hover", query(source, "password =", false));
    let markdown = hover["contents"]["value"].as_str().unwrap();
    assert!(markdown.contains("bool"), "{markdown}");
    id += 1;
    let definition = server.request(
        id,
        "textDocument/definition",
        query(source, "password =", false),
    );
    id += 1;
    let document = server.request(id, "aside/nativeDocument", json!({"uri":definition["uri"]}));
    let text = document["text"].as_str().unwrap();
    let declaration = text
        .lines()
        .nth(definition["range"]["start"]["line"].as_u64().unwrap() as usize)
        .unwrap();
    assert!(declaration.contains("password: bool"), "{declaration}");
    assert!(
        text.contains("Display one bullet per Unicode grapheme"),
        "{text}"
    );
    let invalid = source.replace("editor.password = false", "editor.password = 42");
    server.notify(
        "textDocument/didChange",
        json!({"textDocument":{"uri":URI,"version":2}, "contentChanges":[{"text":invalid}]}),
    );
    let diagnostics = server.diagnostics(URI, 2);
    assert!(
        diagnostics
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["message"].as_str().unwrap().contains("bool")),
        "{diagnostics}"
    );
    server.stop();
}

#[test]
fn input_transactions_keep_typed_chain_hover_navigation_completion_and_readonly_outputs() {
    let source = r#"fn main() {
        let editor = text_area(value: "Draft");
        let same:TextInputNode = editor.select_bytes(0, focus: 5).select_all().undo().redo();
        let anchor:int = same.selection_anchor;
        let focus:int = same.selection_focus;
        let undoable:bool = same.can_undo;
        let redoable:bool = same.can_redo;
    }"#;
    let (mut server, initialized) = open(source);
    let mut id = 2;
    for (expression, method) in [
        ("select_bytes(0", "select_bytes"),
        ("select_all().undo", "select_all"),
        ("undo().redo", "undo"),
        ("redo();", "redo"),
    ] {
        assert_method(
            &mut server,
            &mut id,
            source,
            expression,
            method,
            "TextInputNode",
        );
    }
    let tokens = server.request(
        id,
        "textDocument/semanticTokens/full",
        json!({"textDocument":{"uri":URI}}),
    );
    id += 1;
    for expression in [
        "select_bytes(0",
        "select_all().undo",
        "undo().redo",
        "redo();",
    ] {
        assert_method_token(&tokens, &initialized, source, expression);
    }
    let completions = server.request(id, "textDocument/completion", query(source, "same.", true));
    let items = completions["items"]
        .as_array()
        .or_else(|| completions.as_array())
        .unwrap();
    for name in [
        "select_bytes",
        "select_all",
        "undo",
        "redo",
        "selection_anchor",
        "selection_focus",
        "can_undo",
        "can_redo",
    ] {
        assert!(
            items.iter().any(|item| item["label"] == name),
            "{name}: {items:?}"
        );
    }
    server.notify("textDocument/didChange",json!({"textDocument":{"uri":URI,"version":2},"contentChanges":[{"text":source.replace("let anchor:int = same.selection_anchor;","same.selection_anchor = 1;")}]}));
    let diagnostics = server.diagnostics(URI, 2);
    assert!(
        !diagnostics.as_array().unwrap().is_empty(),
        "readonly selection must be rejected"
    );
    server.stop();
}
