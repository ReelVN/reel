//! Exercise the shipped executable through its actual stdio protocol.
use std::{
    io::{BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use lsp_server::Message;
use serde_json::{Value, json};

struct Server {
    child: Child,
    input: ChildStdin,
    output: Receiver<Value>,
}

impl Server {
    fn start() -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aside-lsp"));
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, output) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(message) = Message::read(&mut reader).unwrap() {
                if sender.send(serde_json::to_value(message).unwrap()).is_err() {
                    break;
                }
            }
        });
        Self { child, input, output }
    }

    fn send(&mut self, value: Value) {
        let bytes = serde_json::to_vec(&value).unwrap();
        write!(self.input, "Content-Length: {}\r\n\r\n", bytes.len()).unwrap();
        self.input.write_all(&bytes).unwrap();
        self.input.flush().unwrap();
    }

    fn receive(&self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let message = self.output.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("language server did not answer the real stdio client");
            if predicate(&message) { return message; }
        }
    }

    fn request(&mut self, id: i32, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}));
        let message = self.receive(|message| message["id"] == id);
        assert!(message.get("error").is_none(), "{message}");
        message["result"].clone()
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc":"2.0", "method":method, "params":params}));
    }

    fn diagnostics(&self, uri: &str, version: i32) -> Value {
        self.receive(|message| message["method"] == "textDocument/publishDiagnostics"
            && message["params"]["uri"] == uri && message["params"]["version"] == version)
            ["params"]["diagnostics"].clone()
    }

    fn stop(&mut self) {
        assert!(self.request(99, "shutdown", Value::Null).is_null());
        self.notify("exit", Value::Null);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "server exited with {status}");
                return;
            }
            assert!(Instant::now() < deadline, "server did not exit after shutdown");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn real_stdio_preserves_unicode_edits_unsaved_imports_and_shutdown() {
    let project = tempfile::tempdir().unwrap();
    let root = url::Url::from_directory_path(project.path()).unwrap();
    let client_uri = |uri: url::Url| {
        #[cfg(windows)]
        {
            let value = uri.as_str();
            if value.as_bytes().get(9) == Some(&b':') {
                return format!("file:///{}%3A{}", value[8..9].to_ascii_lowercase(), &value[10..]);
            }
        }
        uri.to_string()
    };
    let main_uri = client_uri(root.join("main.aside").unwrap());
    let helper_uri = client_uri(root.join("helper.aside").unwrap());
    std::fs::write(project.path().join("helper.aside"),
        "/// Double the number.\npub fn twice(value: int) -> int { value * 2 }").unwrap();
    let mut server = Server::start();
    let initialized = server.request(1, "initialize", json!({
        "rootUri":root.as_str(), "capabilities":{}
    }));
    assert_eq!(initialized["capabilities"]["positionEncoding"], "utf-16");
    server.notify("initialized", json!({}));

    let source = "use helper::twice;\r\nfn main() { let 文 = \"😀\"; missing(); twice(2); }";
    server.notify("textDocument/didOpen", json!({"textDocument":{
        "uri":main_uri, "languageId":"aside", "version":1, "text":source
    }}));
    let errors = server.diagnostics(&main_uri, 1);
    let missing = errors.as_array().unwrap().iter()
        .find(|diagnostic| diagnostic["message"].as_str().is_some_and(|text| text.contains("missing")))
        .expect("missing function diagnostic");
    let line = source.lines().nth(1).unwrap();
    let missing_column = line[..line.find("missing").unwrap()].encode_utf16().count();
    assert_eq!(missing["range"]["start"], json!({"line":1, "character":missing_column}));

    server.notify("textDocument/didChange", json!({"textDocument":{"uri":main_uri,"version":2},
        "contentChanges":[{"range":{"start":{"line":1,"character":missing_column},
            "end":{"line":1,"character":missing_column + "missing(); ".len()}},"text":""}]
    }));
    assert_eq!(server.diagnostics(&main_uri, 2), json!([]));
    let position = json!({"textDocument":{"uri":main_uri},
        "position":{"line":1,"character":missing_column + 1}});
    let definition = server.request(2, "textDocument/definition", position.clone());
    assert_eq!(url::Url::parse(definition["uri"].as_str().unwrap()).unwrap().to_file_path().unwrap(),
        url::Url::parse(&helper_uri).unwrap().to_file_path().unwrap());
    assert_eq!(definition["range"]["start"], json!({"line":1,"character":7}));
    let hover = server.request(3, "textDocument/hover", position.clone());
    assert!(hover.to_string().contains("Double the number."), "{hover}");

    server.notify("textDocument/didOpen", json!({"textDocument":{
        "uri":helper_uri,"languageId":"aside","version":1,
        "text":"/// Unsaved helper.\npub fn twice(value: string) -> string { value }"
    }}));
    let errors = server.diagnostics(&main_uri, 2);
    assert!(!errors.as_array().unwrap().is_empty(), "dependency overlay must invalidate callers");
    let hover = server.request(4, "textDocument/hover", position.clone());
    assert!(hover.to_string().contains("Unsaved helper."), "{hover}");
    assert!(hover.to_string().contains("value: string"), "{hover}");
    assert_eq!(server.request(5, "textDocument/definition", position)["uri"], helper_uri);

    server.notify("textDocument/didClose", json!({"textDocument":{"uri":helper_uri}}));
    assert_eq!(server.diagnostics(&main_uri, 2), json!([]));
    server.stop();
}
