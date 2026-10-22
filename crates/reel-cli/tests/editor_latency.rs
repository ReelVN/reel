//! Opt-in stdio latency probe over supplied author projects, without changing their files.
#[path = "../../aside-lsp/tests/support/stdio.rs"]
mod support;

use serde_json::{Value, json};
use std::{fmt::Write, path::Path, time::Instant};
use support::Server;

fn uri(path: &Path) -> String {
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

fn query(uri: &str, source: &str, needle: &str) -> Value {
    let before = &source[..source.rfind(needle).unwrap()];
    json!({"textDocument":{"uri":uri},"position":{
        "line":before.bytes().filter(|byte| *byte == b'\n').count(),
        "character":before.rsplit('\n').next().unwrap().encode_utf16().count()}})
}

fn timed(server: &mut Server, id: &mut i32, method: &str, params: Value) -> Value {
    let started = Instant::now();
    let result = server.request(*id, method, params);
    *id += 1;
    json!({"method":method,"ms":started.elapsed().as_secs_f64()*1000.0,
        "bytes":result.to_string().len(),"value":result})
}

fn edited(source: &str, value: i32) -> String {
    let end = source.rfind('}').unwrap();
    format!(
        "{}\n    let latency_probe = {value};\n{}",
        &source[..end],
        &source[end..]
    )
}

#[test]
#[ignore = "set REEL_LSP_PROJECTS to semicolon-separated project roots"]
fn real_project_stdio_latency() {
    let executable = std::env::var_os("REEL_LSP_EXECUTABLE")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_reel-lsp").into());
    for project in std::env::var("REEL_LSP_PROJECTS")
        .expect("REEL_LSP_PROJECTS")
        .split(';')
    {
        let root = Path::new(project);
        let path = root.join("src/main.aside");
        let source = std::fs::read_to_string(&path).unwrap();
        let uri = uri(&path);
        let needle = if source.contains("boot_desktop(root") {
            "boot_desktop(root"
        } else {
            "frontend.show(root"
        };
        let params = query(&uri, &source, needle);
        let mut server = Server::start(&executable);
        server.request(
            1,
            "initialize",
            json!({"rootUri":self::uri(root),"capabilities":{}}),
        );
        server.notify("initialized", json!({}));
        let mut id = 2;
        let started = Instant::now();
        server.notify(
            "textDocument/didOpen",
            json!({"textDocument":{
            "uri":uri,"languageId":"aside","version":1,"text":source}}),
        );
        let first = timed(&mut server, &mut id, "textDocument/hover", params.clone());
        let open_hover_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert!(!first["value"].is_null(), "{first}");
        let mut requests = vec![json!({"phase":"open_hover","ms":open_hover_ms,"request":first})];
        for method in [
            "textDocument/hover",
            "textDocument/completion",
            "textDocument/definition",
        ] {
            for _ in 0..3 {
                requests.push(timed(&mut server, &mut id, method, params.clone()));
            }
        }
        let started = Instant::now();
        server.notify(
            "textDocument/didChange",
            json!({"textDocument":{"uri":uri,"version":2},
            "contentChanges":[{"text":edited(&source, 2)}]}),
        );
        let changed = timed(
            &mut server,
            &mut id,
            "textDocument/definition",
            params.clone(),
        );
        requests.push(json!({"phase":"change_definition","ms":started.elapsed().as_secs_f64()*1000.0,"request":changed}));
        let started = Instant::now();
        for version in 3..=5 {
            server.notify(
                "textDocument/didChange",
                json!({"textDocument":{"uri":uri,"version":version},
                "contentChanges":[{"text":edited(&source, version)}]}),
            );
        }
        let burst = timed(&mut server, &mut id, "textDocument/hover", params.clone());
        requests.push(json!({"phase":"burst_hover","ms":started.elapsed().as_secs_f64()*1000.0,"request":burst}));
        let leaf_path = root.join(if needle.starts_with("boot") {
            "src/app.aside"
        } else {
            "src/frontend.aside"
        });
        let leaf_source = std::fs::read_to_string(&leaf_path).unwrap();
        let leaf_uri = self::uri(&leaf_path);
        let leaf_needle = if needle.starts_with("boot") {
            "boot_desktop(root"
        } else {
            "show(root"
        };
        let started = Instant::now();
        server.notify(
            "textDocument/didOpen",
            json!({"textDocument":{
            "uri":leaf_uri,"languageId":"aside","version":1,"text":leaf_source}}),
        );
        let leaf = timed(
            &mut server,
            &mut id,
            "textDocument/hover",
            query(&leaf_uri, &leaf_source, leaf_needle),
        );
        assert!(!leaf["value"].is_null());
        requests.push(json!({"phase":"open_imported_hover","ms":started.elapsed().as_secs_f64()*1000.0,"request":leaf}));
        let started = Instant::now();
        server.notify(
            "textDocument/didChange",
            json!({"textDocument":{"uri":uri,"version":6},
            "contentChanges":[{"text":edited(&source, 6)}]}),
        );
        let changed = timed(&mut server, &mut id, "textDocument/definition", params);
        assert!(changed["value"]["uri"].is_string());
        requests.push(json!({"phase":"two_open_change_definition","ms":started.elapsed().as_secs_f64()*1000.0,"request":changed}));
        let started = Instant::now();
        server.notify(
            "textDocument/didChange",
            json!({"textDocument":{"uri":uri,"version":7},
            "contentChanges":[{"text":edited(&source, 7)}]}),
        );
        let diagnostics = server.diagnostics(&uri, 7);
        requests.push(json!({"phase":"two_open_change_diagnostics","ms":started.elapsed().as_secs_f64()*1000.0,
            "diagnostics":diagnostics.as_array().unwrap().len()}));
        println!("{}", json!({"project":project,"requests":requests}));
        server.stop();
    }
}
