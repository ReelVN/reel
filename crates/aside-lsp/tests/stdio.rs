//! Exercise the shipped executable through its actual stdio protocol.
#[path = "support/stdio.rs"]
mod support;
use serde_json::json;
use support::Server;

fn server() -> Server {
    let executable = std::env::var_os("ASIDE_LSP_EXECUTABLE")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_aside-lsp").into());
    Server::start(executable)
}

#[test]
fn real_stdio_colors_names_and_completes_unfinished_declarations() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("helper.aside"),
        "/// Narration text.\npub let 旁白 = \"Hello\";",
    )
    .unwrap();
    let uri = url::Url::from_file_path(project.path().join("main.aside")).unwrap();
    let source = "use helper::旁白;\r\nfn main() { let 文 = \"😀\"; let 值 = 旁白; 值; }\r\ncla";
    for encoding in ["utf-8", "utf-16"] {
        let mut server = server();
        let initialized = server.request(
            1,
            "initialize",
            json!({"capabilities":{"general":{"positionEncodings":[encoding]}}}),
        );
        assert_eq!(initialized["capabilities"]["positionEncoding"], encoding);
        let provider = &initialized["capabilities"]["semanticTokensProvider"];
        assert_eq!(provider["full"], true);
        let legend = provider["legend"]["tokenTypes"].as_array().unwrap();
        server.notify("initialized", json!({}));
        server.notify(
            "textDocument/didOpen",
            json!({"textDocument":{
                "uri":uri.as_str(),"languageId":"aside","version":1,"text":source
            }}),
        );
        server.diagnostics(uri.as_str(), 1);
        let at = |line, character| json!({"textDocument":{"uri":uri.as_str()},"position":{"line":line,"character":character}});
        assert!(
            server.request(2, "textDocument/hover", at(0, 1)).is_null(),
            "the use keyword must not borrow another declaration's hover"
        );
        let hover = server.request(3, "textDocument/hover", at(0, "use helper::".len()));
        assert!(hover.to_string().contains("旁白"), "{hover}");
        assert!(hover.to_string().contains("string"), "{hover}");
        assert_eq!(hover["range"]["start"], json!({"line":0,"character":12}));
        let items = server.request(4, "textDocument/completion", at(2, 3));
        assert!(
            items.as_array().unwrap().iter().any(|item| {
                item["label"] == "class"
                    && item["kind"]
                        == serde_json::to_value(lsp_types::CompletionItemKind::KEYWORD).unwrap()
            }),
            "unfinished class keyword has no completion: {items}"
        );
        let tokens = server.request(
            5,
            "textDocument/semanticTokens/full",
            json!({"textDocument":{"uri":uri.as_str()}}),
        );
        let line = source.lines().nth(1).unwrap();
        let prefix = &line[..line.find('值').unwrap()];
        let expected_column = if encoding == "utf-8" {
            prefix.len()
        } else {
            prefix.encode_utf16().count()
        } as u64;
        let expected_length = if encoding == "utf-8" { 3 } else { 1 };
        let mut token_line = 0;
        let mut column = 0;
        let mut found = false;
        let data = tokens["data"].as_array().unwrap();
        assert_eq!(data.len() % 5, 0);
        for token in data.as_chunks::<5>().0 {
            let delta_line = token[0].as_u64().unwrap();
            token_line += delta_line;
            column = if delta_line == 0 { column } else { 0 } + token[1].as_u64().unwrap();
            if token_line == 1 && column == expected_column {
                assert_eq!(token[2], expected_length);
                assert_eq!(legend[token[3].as_u64().unwrap() as usize], "variable");
                found = true;
            }
        }
        assert!(
            found,
            "missing Chinese variable after emoji with {encoding}: {tokens}"
        );
        server.stop();
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
                return format!(
                    "file:///{}%3A{}",
                    value[8..9].to_ascii_lowercase(),
                    &value[10..]
                );
            }
        }
        uri.to_string()
    };
    let main_uri = client_uri(root.join("main.aside").unwrap());
    let helper_uri = client_uri(root.join("helper.aside").unwrap());
    std::fs::write(
        project.path().join("helper.aside"),
        "/// Double the number.\npub fn twice(value: int) -> int { value * 2 }",
    )
    .unwrap();
    let mut server = server();
    let initialized = server.request(
        1,
        "initialize",
        json!({
            "rootUri":root.as_str(), "capabilities":{}
        }),
    );
    assert_eq!(initialized["capabilities"]["positionEncoding"], "utf-16");
    assert_eq!(
        initialized["capabilities"]["documentFormattingProvider"],
        true
    );
    server.notify("initialized", json!({}));

    let source = "use helper::twice;\r\nfn main() { let 文 = \"😀\"; missing(); twice(2); }";
    server.notify(
        "textDocument/didOpen",
        json!({"textDocument":{
            "uri":main_uri, "languageId":"aside", "version":1, "text":source
        }}),
    );
    let errors = server.diagnostics(&main_uri, 1);
    let missing = errors
        .as_array()
        .unwrap()
        .iter()
        .find(|diagnostic| {
            diagnostic["message"]
                .as_str()
                .is_some_and(|text| text.contains("missing"))
        })
        .expect("missing function diagnostic");
    let line = source.lines().nth(1).unwrap();
    let missing_column = line[..line.find("missing").unwrap()].encode_utf16().count();
    assert_eq!(
        missing["range"]["start"],
        json!({"line":1, "character":missing_column})
    );

    server.notify(
        "textDocument/didChange",
        json!({"textDocument":{"uri":main_uri,"version":2},
            "contentChanges":[{"range":{"start":{"line":1,"character":missing_column},
                "end":{"line":1,"character":missing_column + "missing(); ".len()}},"text":""}]
        }),
    );
    assert_eq!(server.diagnostics(&main_uri, 2), json!([]));
    let position = json!({"textDocument":{"uri":main_uri},
        "position":{"line":1,"character":missing_column + 1}});
    let definition = server.request(2, "textDocument/definition", position.clone());
    assert_eq!(
        url::Url::parse(definition["uri"].as_str().unwrap())
            .unwrap()
            .to_file_path()
            .unwrap(),
        url::Url::parse(&helper_uri)
            .unwrap()
            .to_file_path()
            .unwrap()
    );
    assert_eq!(
        definition["range"]["start"],
        json!({"line":1,"character":7})
    );
    let hover = server.request(3, "textDocument/hover", position.clone());
    assert!(hover.to_string().contains("Double the number."), "{hover}");

    server.notify(
        "textDocument/didOpen",
        json!({"textDocument":{
            "uri":helper_uri,"languageId":"aside","version":1,
            "text":"/// Unsaved helper.\npub fn twice(value: string) -> string { value }"
        }}),
    );
    let errors = server.diagnostics(&main_uri, 2);
    assert!(
        !errors.as_array().unwrap().is_empty(),
        "dependency overlay must invalidate callers"
    );
    let hover = server.request(4, "textDocument/hover", position.clone());
    assert!(hover.to_string().contains("Unsaved helper."), "{hover}");
    assert!(hover.to_string().contains("value: string"), "{hover}");
    assert_eq!(
        server.request(5, "textDocument/definition", position)["uri"],
        helper_uri
    );

    server.notify(
        "textDocument/didClose",
        json!({"textDocument":{"uri":helper_uri}}),
    );
    assert_eq!(server.diagnostics(&main_uri, 2), json!([]));
    let formatting = json!({
        "textDocument":{"uri":main_uri},
        "options":{"tabSize":4,"insertSpaces":true}
    });
    let edits = server.request(6, "textDocument/formatting", formatting.clone());
    assert_eq!(edits.as_array().unwrap().len(), 1);
    let edited_source = source.replace("missing(); ", "");
    assert_eq!(edits[0]["range"]["start"], json!({"line":0,"character":0}));
    assert_eq!(
        edits[0]["range"]["end"],
        json!({
            "line":1,"character":edited_source.lines().nth(1).unwrap().encode_utf16().count()
        })
    );
    assert!(edits[0]["newText"].as_str().unwrap().contains("\"😀\""));
    server.notify(
        "textDocument/didChange",
        json!({
            "textDocument":{"uri":main_uri,"version":3},
            "contentChanges":[{"range":edits[0]["range"],"text":edits[0]["newText"]}]
        }),
    );
    assert_eq!(server.diagnostics(&main_uri, 3), json!([]));
    assert_eq!(
        server.request(7, "textDocument/formatting", formatting),
        json!([])
    );
    server.stop();
}
