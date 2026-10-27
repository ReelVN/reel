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
    let barrier = |id: i32, version: i32| {
        client
            .sender
            .send(Request::new(id.into(), "textDocument/hover".into(), query.clone()).into())
            .unwrap();
        let mut failures = Vec::new();
        let mut documents = 0;
        let mut answered = false;
        let mut published = false;
        let mut fence_sent = false;
        loop {
            match client
                .receiver
                .recv_timeout(Duration::from_secs(60))
                .unwrap()
            {
                Message::Response(response) if response.id == id.into() => {
                    assert!(response.error.is_none(), "{:?}", response.error);
                    answered = true;
                }
                Message::Response(response) if response.id == (id + 1000).into() => {
                    assert!(response.error.is_none(), "{:?}", response.error);
                    return (documents, failures);
                }
                Message::Notification(notification)
                    if notification.method == "textDocument/publishDiagnostics" =>
                {
                    documents += 1;
                    published |= notification.params["uri"] == uri
                        && notification.params["version"] == version;
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
            if answered && published && !fence_sent {
                // The server sends the complete diagnostic batch before it
                // handles this fence, including modules sorted after the entry.
                client
                    .sender
                    .send(
                        Request::new(
                            (id + 1000).into(),
                            "textDocument/hover".into(),
                            query.clone(),
                        )
                        .into(),
                    )
                    .unwrap();
                fence_sent = true;
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
    let (documents, failures) = barrier(2, 1);
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
    let (_, edited_failures) = barrier(60, 2);
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

fn diagnostics(connection: &Connection, uri: &str) -> Value {
    loop {
        if let Message::Notification(notification) = connection
            .receiver
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
            && notification.method == "textDocument/publishDiagnostics"
            && notification.params["uri"] == uri
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
    // A root-level main does not establish a Reel project.
    std::fs::write(root.join("main.aside"), "fn main() {}\n").unwrap();
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
    let main_uri = file_uri(&root.join("src/main.aside"));

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
    assert!(
        !diagnostics(&client, &uri).as_array().unwrap().is_empty(),
        "a root-level main must not establish the project source directory"
    );
    client
        .sender
        .send(
            Notification::new(
                "textDocument/didOpen".into(),
                json!({"textDocument":{
                    "uri":main_uri,"languageId":"aside","version":1,"text":"fn main() {}"
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
    let mut hover = None;
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
                hover = response.result;
            }
            _ => {}
        }
        if published.contains(&json!(uri))
            && published.contains(&json!(opening_uri))
            && let Some(hover) = hover.take()
        {
            break hover;
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
    client
        .sender
        .send(
            Notification::new(
                "textDocument/didOpen".into(),
                json!({"textDocument":{
                    "uri":opening_uri,"languageId":"aside","version":1,
                    "text":"pub fn opening() -> int { 42 }"
                }}),
            )
            .into(),
        )
        .unwrap();
    assert!(!diagnostics(&client, &uri).as_array().unwrap().is_empty());
    client
        .sender
        .send(
            Notification::new(
                "textDocument/didClose".into(),
                json!({"textDocument":{"uri":opening_uri}}),
            )
            .into(),
        )
        .unwrap();
    assert_eq!(diagnostics(&client, &uri), json!([]));
    client
        .sender
        .send(
            Notification::new(
                "textDocument/didClose".into(),
                json!({"textDocument":{"uri":main_uri}}),
            )
            .into(),
        )
        .unwrap();
    assert!(
        !diagnostics(&client, &uri).as_array().unwrap().is_empty(),
        "closing the unsaved src/main must retire its project root"
    );
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
    assert_eq!(diagnostics(&client, "untitled:reel.aside"), json!([]));

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
        !diagnostics(&client, "untitled:reel.aside")
            .as_array()
            .unwrap()
            .is_empty(),
        "read-only engine state must reject assignment"
    );
    request(&client, 8, "shutdown", Value::Null);
    client
        .sender
        .send(Notification::new("exit".into(), Value::Null).into())
        .unwrap();
    thread.join().unwrap();
}

#[path = "../../aside-lsp/tests/support/stdio.rs"]
mod support;

mod editor_stdio {
    //! Verify author typing against the actual Reel language-server executable.
    use super::support;
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
        let cases: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for case in cases.as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let source = case["source"].as_str().unwrap();
            reel_cli::check(source, "snippet.aside")
                .unwrap_or_else(|error| panic!("{name}: {error:?}"));
            eprintln!("{name}: compiled");
        }
    }
}

mod editor_modules {
    //! Query real module prefixes through the shipped LSP and generated host source.
    use super::support;
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
            let document =
                server.request(id, "aside/nativeDocument", json!({"uri":definition["uri"]}));
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
}

mod editor_declarations {
    //! Follow the registered API through real read-only declaration views over stdio.
    use super::support;
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
    fn definition(
        server: &mut Server,
        id: &mut i32,
        uri: &str,
        source: &str,
        needle: &str,
    ) -> Value {
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
        assert!(constructor_text.contains("// TextNode properties."));
        assert!(constructor_text.contains("// Node properties."));
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
        let exported_type =
            definition(&mut server, &mut id, &module_uri, &module_text, "TextNode;");
        assert_eq!(exported_type, text_type);
        server.stop();
    }

    #[test]
    fn node_schema_groups_and_parameter_help_describe_the_actual_properties() {
        let mut server = Server::start(env!("CARGO_BIN_EXE_reel-lsp"));
        server.request(1, "initialize", json!({"capabilities":{}}));
        server.notify("initialized", json!({}));
        let uri = "untitled:property-parameter-help.aside";
        let source = "fn main() { let editor = text_input(value: \"Name\", password: true); editor.with(password: false, x: 12); }";
        open(&mut server, uri, source);
        let mut id = 1;
        for (call, argument, native) in [
            ("text_input(", "password: true", "reel::text_input"),
            ("with(", "password: false", "reel::text_input::with"),
        ] {
            id += 1;
            let hover = server.request(id, "textDocument/hover", query(uri, source, call));
            let markdown = hover["contents"]["value"].as_str().unwrap();
            assert!(markdown.contains("TextInputNode properties:"), "{markdown}");
            assert!(markdown.contains("Node properties:"), "{markdown}");
            assert!(
                markdown.contains("`password`") && markdown.contains("`x`"),
                "{markdown}"
            );
            id += 1;
            let signature = server.request(
                id,
                "textDocument/signatureHelp",
                query(uri, source, argument),
            );
            let active = signature["activeParameter"].as_u64().unwrap() as usize;
            let parameters = signature["signatures"][0]["parameters"].as_array().unwrap();
            let expected = if native.ends_with("::with") {
                "password?:"
            } else {
                "password:"
            };
            assert!(
                parameters[active]["label"]
                    .as_str()
                    .unwrap()
                    .starts_with(expected),
                "{signature}"
            );
            let documentation = parameters[active]["documentation"]["value"]
                .as_str()
                .unwrap();
            assert!(
                documentation.contains("Display one bullet per Unicode grapheme"),
                "{documentation}"
            );
            let registered = reel_engine::aside::source::api()
                .native(native)
                .symbol
                .signature
                .as_ref()
                .unwrap();
            assert_eq!(
                parameters.len(),
                registered.parameters.len() - usize::from(native.ends_with("::with"))
            );
        }
        server.stop();
    }

    #[test]
    fn reexports_keep_function_identity_when_a_namespace_has_the_same_path() {
        let mut server = Server::start(env!("CARGO_BIN_EXE_reel-lsp"));
        server.request(1, "initialize", json!({"capabilities":{}}));
        server.notify("initialized", json!({}));
        let uri = "untitled:function-namespace-identity.aside";
        let source = "use reel::prelude::{video}; fn main() { let clip = video(\"clip.mp4\"); }";
        open(&mut server, uri, source);
        let mut id = 1;
        let constructor = definition(&mut server, &mut id, uri, source, "video};");
        let module = definition(&mut server, &mut id, uri, source, "prelude::{");
        let (module_uri, module_text) = document(&mut server, &mut id, &module);
        let reexport = definition(&mut server, &mut id, &module_uri, &module_text, "video;");
        assert_eq!(
            reexport, constructor,
            "the reexport must retain its function identity"
        );
        let (_, text) = document(&mut server, &mut id, &reexport);
        assert!(
            text.lines()
                .nth(reexport["range"]["start"]["line"].as_u64().unwrap() as usize)
                .unwrap()
                .starts_with("pub fn video(")
        );
        let namespace_uri = "aside-native://reel/modules/reel/video.aside";
        id += 1;
        let namespace = server.request(id, "aside/nativeDocument", json!({"uri":namespace_uri}));
        let namespace_text = namespace["text"].as_str().unwrap();
        open(&mut server, namespace_uri, namespace_text);
        let namespace = definition(
            &mut server,
            &mut id,
            namespace_uri,
            namespace_text,
            "video;",
        );
        assert_eq!(namespace["uri"], namespace_uri);
        assert_ne!(namespace, constructor);
        server.stop();
    }
}

mod editor_services {
    //! Builtin service roots and imports navigate through the actual host registry.
    use super::support;
    use aside_compiler::semantic::TypeHint;
    use serde_json::{Value, json};
    use support::Server;

    const SERVICES: [&str; 9] = [
        "assets",
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
                .unwrap_or_else(|| {
                    panic!("missing module declaration for {}", module.canonical_path)
                });
            assert_eq!(declaration.reference.version, module.version);
            let location = declaration.definition.as_ref().unwrap();
            let document = context
                .documents
                .iter()
                .find(|document| document.uri == location.uri)
                .unwrap();
            let name = module.canonical_path.rsplit("::").next().unwrap();
            assert_eq!(
                &document.text
                    [usize::from(location.range.start())..usize::from(location.range.end())],
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
        let source = "fn main(){game.reading().set_auto(enabled:true,delay:1,condition:derive(||true));game.reading().set_skip(false);game.reading().pause(false);let operation=saves.save(\"slot\",title:\"Saved\",thumbnail:null);operation.status;operation.error;operation.restored;let copied=saves.copy(source:\"slot\",destination:\"backup\");copied.status;copied.error;let data=persistent.read(\"key\",default:0);persistent.write(\"key\",{value:1});stage.transition(||null);}";
        reel_engine::aside::source::compile_story(source, "game::main", 1, "main").unwrap();
        for body in [
            "game.configure(title:12);",
            "game.configure(width:1.5);",
            "game.reading().set_auto(enabled:1);",
            "saves.save(1);",
            "saves.copy(1,\"backup\");",
            "saves.copy(\"slot\",1);",
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
        let source = "use std::time as clock;\nfn main(){\n game.configure(title: \"Example\", width: 960, height: 540);\n stage.replace(text(\"Hello\"));\n assets.read(\"data.bin\");\n audio.play(\"sound.ogg\");\n localization.locale();\n saves.list();\n persistent.read(\"setting\");\n window.info();\n mask.gray(\"mask.png\");\n clock.now();\n}";
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
            let document =
                client.request(id, "aside/nativeDocument", json!({"uri":definition["uri"]}));
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
            .filter(|parameter| {
                parameter.default && text.property_parameters.contains(&parameter.name)
            })
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
}
