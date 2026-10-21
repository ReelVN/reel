//! The public editor adapter serves the actual registered engine API.
use std::{
    fmt::Write as _,
    path::Path,
    time::{Duration, Instant},
};

use lsp_server::{Connection, Message, Notification, Request};
use serde_json::{Value, json};

fn request(connection: &Connection, id: i32, method: &str, params: Value) -> Value {
    connection
        .sender
        .send(Request::new(id.into(), method.into(), params).into())
        .unwrap();
    loop {
        if let Message::Response(response) = connection
            .receiver
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
            && response.id == id.into()
        {
            assert!(response.error.is_none(), "{:?}", response.error);
            return response.result.unwrap();
        }
    }
}

/// Measure editor work against an existing project without running its game.
#[test]
#[ignore = "set REEL_EDITOR_FILE and REEL_EDITOR_URI to an existing project source"]
fn existing_project_editor_cost() {
    let path = std::env::var("REEL_EDITOR_FILE").unwrap();
    let uri = std::env::var("REEL_EDITOR_URI").unwrap();
    let source = std::fs::read_to_string(&path).unwrap();
    let started = Instant::now();
    let options = reel_cli::editor::options();
    let setup_ms = started.elapsed().as_secs_f64() * 1000.0;
    let (server, client) = Connection::memory();
    let thread = std::thread::spawn(move || aside_lsp::run_connection(server, options).unwrap());
    request(&client, 1, "initialize", json!({"capabilities":{}}));
    client
        .sender
        .send(Notification::new("initialized".into(), json!({})).into())
        .unwrap();
    let query = json!({"textDocument":{"uri":uri},"position":{"line":0,"character":0}});
    let barrier = |id: i32| {
        client
            .sender
            .send(Request::new(id.into(), "textDocument/hover".into(), query.clone()).into())
            .unwrap();
        let mut failures = Vec::new();
        let mut documents = 0;
        loop {
            match client
                .receiver
                .recv_timeout(Duration::from_secs(60))
                .unwrap()
            {
                Message::Response(response) if response.id == id.into() => {
                    assert!(response.error.is_none(), "{:?}", response.error);
                    return (documents, failures);
                }
                Message::Notification(notification)
                    if notification.method == "textDocument/publishDiagnostics" =>
                {
                    documents += 1;
                    for diagnostic in notification.params["diagnostics"].as_array().unwrap() {
                        if diagnostic["severity"] == 1 {
                            failures.push(
                                json!({"uri":notification.params["uri"],"diagnostic":diagnostic}),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    };
    let started = Instant::now();
    client
        .sender
        .send(
            Notification::new(
                "textDocument/didOpen".into(),
                json!({"textDocument":{
                    "uri":uri,"languageId":"aside","version":1,"text":source
                }}),
            )
            .into(),
        )
        .unwrap();
    let (documents, failures) = barrier(2);
    let open_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut queries = Vec::new();
    let mut completion_bytes = 0;
    for id in 10..60 {
        let started = Instant::now();
        let response = request(&client, id, "textDocument/completion", query.clone());
        assert!(!response.as_array().unwrap().is_empty());
        queries.push(started.elapsed().as_secs_f64() * 1000.0);
        if id == 10 {
            completion_bytes = serde_json::to_vec(&response).unwrap().len();
        }
    }
    queries.sort_by(f64::total_cmp);
    let started = Instant::now();
    client.sender.send(Notification::new("textDocument/didChange".into(), json!({
        "textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":format!("{source}\n")}]
    })).into()).unwrap();
    let (_, edited_failures) = barrier(60);
    let edit_ms = started.elapsed().as_secs_f64() * 1000.0;
    request(&client, 61, "shutdown", Value::Null);
    client
        .sender
        .send(Notification::new("exit".into(), Value::Null).into())
        .unwrap();
    thread.join().unwrap();
    println!(
        "{}",
        json!({"file":path,"source_bytes":source.len(),"documents":documents,
        "setup_ms":setup_ms,"open_ms":open_ms,"edit_ms":edit_ms,
        "cached_completion_median_ms":queries[25],"cached_completion_p95_ms":queries[47],
        "completion_bytes":completion_bytes,
        "errors":failures,"edited_errors":edited_failures})
    );
    assert!(failures.is_empty(), "project diagnostics above");
    assert!(
        edited_failures.is_empty(),
        "edited project diagnostics above"
    );
}

fn diagnostics(connection: &Connection) -> Value {
    loop {
        if let Message::Notification(notification) = connection
            .receiver
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
            && notification.method == "textDocument/publishDiagnostics"
        {
            return notification.params["diagnostics"].clone();
        }
    }
}

fn at(source: &str, needle: &str, end: bool) -> Value {
    let offset = source.rfind(needle).unwrap() + if end { needle.len() } else { 0 };
    let prefix = &source[..offset];
    json!({"textDocument":{"uri":"untitled:reel.aside"},"position":{
        "line":prefix.bytes().filter(|&byte|byte == b'\n').count(),
        "character":prefix.rsplit('\n').next().unwrap().encode_utf16().count()
    }})
}

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

#[test]
fn nested_src_document_uses_the_project_source_root() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("作品 with spaces");
    for directory in ["src/ui", "src/story", "story", "resources", "build"] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    // Both entry layouts exist, so resolving from the workspace root is incorrect.
    std::fs::write(root.join("main.aside"), "fn main() {}\n").unwrap();
    std::fs::write(root.join("src/main.aside"), "fn main() {}\n").unwrap();
    std::fs::write(
        root.join("story/opening.aside"),
        "pub fn opening() -> int { 42 }\n",
    )
    .unwrap();
    let opening = root.join("src/story/opening.aside");
    std::fs::write(
        &opening,
        "/// The source project's opening.\npub fn opening() -> string { \"Hello\" }\n",
    )
    .unwrap();
    let source = "use game::story::opening::opening;\nfn scene() -> string { opening() }\n";
    let path = root.join("src/ui/scene.aside");
    std::fs::write(&path, source).unwrap();
    let uri = file_uri(&path);
    let opening_uri = file_uri(&opening);

    let options = reel_cli::editor::options();
    let (server, client) = Connection::memory();
    let thread = std::thread::spawn(move || aside_lsp::run_connection(server, options).unwrap());
    request(
        &client,
        1,
        "initialize",
        json!({"capabilities":{},"workspaceFolders":[{
            "uri":file_uri(fixture.path()),"name":"repository"
        }]}),
    );
    client
        .sender
        .send(Notification::new("initialized".into(), json!({})).into())
        .unwrap();
    client
        .sender
        .send(
            Notification::new(
                "textDocument/didOpen".into(),
                json!({"textDocument":{
                    "uri":uri,"languageId":"aside","version":1,"text":source
                }}),
            )
            .into(),
        )
        .unwrap();
    let mut query = at(source, "opening()", false);
    query["textDocument"]["uri"] = json!(uri);
    client
        .sender
        .send(Request::new(2.into(), "textDocument/hover".into(), query.clone()).into())
        .unwrap();
    let mut published = Vec::new();
    let hover = loop {
        match client
            .receiver
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
        {
            Message::Notification(notification)
                if notification.method == "textDocument/publishDiagnostics" =>
            {
                assert_eq!(notification.params["diagnostics"], json!([]));
                published.push(notification.params["uri"].clone());
            }
            Message::Response(response) if response.id == 2.into() => {
                assert!(response.error.is_none(), "{:?}", response.error);
                break response.result.unwrap();
            }
            _ => {}
        }
    };
    assert!(published.contains(&json!(uri)), "{published:?}");
    assert!(published.contains(&json!(opening_uri)), "{published:?}");
    assert!(
        hover.to_string().contains("source project's opening"),
        "{hover}"
    );
    assert!(hover.to_string().contains("string"), "{hover}");
    let definition = request(&client, 3, "textDocument/definition", query);
    assert_eq!(definition["uri"], opening_uri);
    assert_eq!(definition["range"]["start"]["line"], 1);
    request(&client, 4, "shutdown", Value::Null);
    client
        .sender
        .send(Notification::new("exit".into(), Value::Null).into())
        .unwrap();
    thread.join().unwrap();
}

#[test]
fn real_reel_types_complete_and_navigate_to_generated_declarations() {
    let options = reel_cli::editor::options();
    let (server, client) = Connection::memory();
    let thread = std::thread::spawn(move || aside_lsp::run_connection(server, options).unwrap());
    request(&client, 1, "initialize", json!({"capabilities":{}}));
    client
        .sender
        .send(Notification::new("initialized".into(), json!({})).into())
        .unwrap();
    let source = r#"fn accepts_node(node: Node) {}
fn main() {
    let label: TextNode = text("Hello", x: 12);
    accepts_node(label);
    label.x = 24;
    let speech = narrator("Hi", wait: false);
    speech.append(" there", wait: false);
    let state = game.reading();
    state.paused;
}"#;
    client
        .sender
        .send(
            Notification::new(
                "textDocument/didOpen".into(),
                json!({"textDocument":{
                    "uri":"untitled:reel.aside","languageId":"aside","version":1,"text":source
                }}),
            )
            .into(),
        )
        .unwrap();
    assert_eq!(diagnostics(&client), json!([]));

    let items = request(
        &client,
        2,
        "textDocument/completion",
        at(source, "label.x", true),
    );
    for name in ["x", "font", "value"] {
        assert!(
            items
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == name),
            "missing {name}: {items}"
        );
    }
    let speech = request(
        &client,
        3,
        "textDocument/hover",
        at(source, "speech.append", false),
    );
    assert!(speech.to_string().contains("Speech"), "{speech}");
    let signature = request(
        &client,
        4,
        "textDocument/signatureHelp",
        at(source, "wait: false", true),
    );
    let label = signature["signatures"][0]["label"].as_str().unwrap();
    assert!(
        label.contains("history: bool | string | reel::types::RichText = true")
            && label.contains("wait: bool = true"),
        "{label}"
    );
    assert!(
        !label.contains("receiver") && !label.contains("self"),
        "{label}"
    );
    assert_eq!(signature["activeParameter"], 2);

    let definition = request(
        &client,
        5,
        "textDocument/definition",
        at(source, "append", false),
    );
    assert!(
        definition["uri"]
            .as_str()
            .unwrap()
            .starts_with("aside-native://")
    );
    let document = request(
        &client,
        6,
        "aside/nativeDocument",
        json!({"uri":definition["uri"]}),
    );
    let line = document["text"]
        .as_str()
        .unwrap()
        .lines()
        .nth(definition["range"]["start"]["line"].as_u64().unwrap() as usize)
        .unwrap();
    assert!(line.contains("fn append("), "{line}");
    assert!(
        !line.contains("receiver") && !line.contains("self"),
        "{line}"
    );
    let inherited = request(
        &client,
        7,
        "textDocument/definition",
        at(source, "label.x", true),
    );
    assert!(
        inherited["uri"].as_str().unwrap().ends_with("/Node.aside"),
        "{inherited}"
    );

    let changed = source.replace("state.paused;", "state.paused = true;");
    client
        .sender
        .send(
            Notification::new(
                "textDocument/didChange".into(),
                json!({
                    "textDocument":{"uri":"untitled:reel.aside","version":2},
                    "contentChanges":[{"text":changed}]
                }),
            )
            .into(),
        )
        .unwrap();
    assert!(
        !diagnostics(&client).as_array().unwrap().is_empty(),
        "read-only engine state must reject assignment"
    );
    request(&client, 8, "shutdown", Value::Null);
    client
        .sender
        .send(Notification::new("exit".into(), Value::Null).into())
        .unwrap();
    thread.join().unwrap();
}
