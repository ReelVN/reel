//! LSP transport and lifecycle; parsing and query semantics stay in the compiler.
use crate::{ServerOptions, position::Encoding, workspace::Workspace};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// Run the standalone protocol over stdin/stdout. Host adapters supply the same
/// configuration API; stdout contains only LSP messages and no game is executed.
pub fn run_stdio(options: ServerOptions) -> Result<()> {
    let (connection, threads) = Connection::stdio();
    run_connection(connection, options)?;
    threads.join()?;
    Ok(())
}

/// Serve one initialized LSP connection. In-memory connections use this same
/// request path for lifecycle and editor-contract tests.
pub fn run_connection(connection: Connection, options: ServerOptions) -> Result<()> {
    let (id, params) = connection.initialize_start()?;
    let params: InitializeParams = serde_json::from_value(params)?;
    let encoding = Encoding::negotiate(
        params
            .capabilities
            .general
            .as_ref()
            .and_then(|general| general.position_encodings.as_deref()),
    );
    let capabilities = ServerCapabilities {
        position_encoding: Some(encoding.kind()),
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::INCREMENTAL,
        )),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".into(), ":".into()]),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(crate::semantic_tokens::options().into()),
        document_formatting_provider: Some(OneOf::Left(true)),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".into(), ",".into(), ":".into()]),
            ..Default::default()
        }),
        ..Default::default()
    };
    connection.initialize_finish(id,json!({"capabilities":capabilities,"serverInfo":{"name":"aside-lsp","version":env!("CARGO_PKG_VERSION")}}))?;
    let mut workspace = Workspace::new(options);
    let mut shutdown = false;
    let mut published = BTreeSet::new();
    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if request.method == "shutdown" {
                    shutdown = true;
                    connection
                        .sender
                        .send(Response::new_ok(request.id, Value::Null).into())?;
                    continue;
                }
                let response = if shutdown {
                    Response::new_err(
                        request.id,
                        ErrorCode::InvalidRequest as i32,
                        "server has shut down".into(),
                    )
                } else {
                    let id = request.id.clone();
                    match dispatch(&mut workspace, request, encoding) {
                        Ok(result) => Response::new_ok(id, result),
                        Err((code, message)) => Response::new_err(id, code as i32, message),
                    }
                };
                connection.sender.send(response.into())?;
            }
            Message::Notification(notification) => {
                if notification.method == "exit" {
                    return if shutdown {
                        Ok(())
                    } else {
                        Err("exit before shutdown".into())
                    };
                }
                if shutdown {
                    continue;
                }
                let error_uri = notification
                    .params
                    .get("textDocument")
                    .and_then(|document| document.get("uri"))
                    .and_then(|uri| serde_json::from_value::<Uri>(uri.clone()).ok());
                let outcome = (|| -> Result<bool> {
                    Ok(match notification.method.as_str() {
                        "textDocument/didOpen" => {
                            let params: DidOpenTextDocumentParams =
                                serde_json::from_value(notification.params)?;
                            workspace.open(
                                params.text_document.uri,
                                params.text_document.version,
                                params.text_document.text,
                            )?;
                            true
                        }
                        "textDocument/didChange" => {
                            let params: DidChangeTextDocumentParams =
                                serde_json::from_value(notification.params)?;
                            workspace.change(
                                &params.text_document.uri,
                                params.text_document.version,
                                params.content_changes,
                                encoding,
                            )?
                        }
                        "textDocument/didClose" => {
                            let params: DidCloseTextDocumentParams =
                                serde_json::from_value(notification.params)?;
                            workspace.close(&params.text_document.uri);
                            connection.sender.send(
                                Notification::new(
                                    "textDocument/publishDiagnostics".into(),
                                    PublishDiagnosticsParams {
                                        uri: params.text_document.uri.clone(),
                                        diagnostics: Vec::new(),
                                        version: None,
                                    },
                                )
                                .into(),
                            )?;
                            published.remove(params.text_document.uri.as_str());
                            true
                        }
                        "workspace/didChangeWatchedFiles" => {
                            let params: DidChangeWatchedFilesParams =
                                serde_json::from_value(notification.params)?;
                            for change in params.changes {
                                workspace.invalidate(&change.uri);
                            }
                            true
                        }
                        _ => false,
                    })
                })();
                match outcome {
                    Ok(true) => publish(&connection, &mut workspace, encoding, &mut published)?,
                    Ok(false) => {}
                    Err(error) => {
                        let message = error.to_string();
                        connection.sender.send(
                            Notification::new(
                                "window/logMessage".into(),
                                LogMessageParams {
                                    typ: MessageType::ERROR,
                                    message: message.clone(),
                                },
                            )
                            .into(),
                        )?;
                        if let Some(uri) = error_uri {
                            connection.sender.send(
                                Notification::new(
                                    "textDocument/publishDiagnostics".into(),
                                    failure(uri.clone(), message),
                                )
                                .into(),
                            )?;
                            published.insert(uri.as_str().to_owned());
                        }
                    }
                }
            }
            Message::Response(_) => {}
        }
    }
    Ok(())
}
fn publish(
    connection: &Connection,
    workspace: &mut Workspace,
    encoding: Encoding,
    published: &mut BTreeSet<String>,
) -> Result<()> {
    let mut current = BTreeMap::new();
    for uri in workspace.open_uris() {
        match workspace.diagnostics(&uri, encoding) {
            Ok(diagnostics) => {
                for diagnostic in diagnostics {
                    current.insert(diagnostic.uri.as_str().to_owned(), diagnostic);
                }
            }
            Err(error) => {
                current.insert(uri.as_str().to_owned(), failure(uri, error));
            }
        }
    }
    for uri in published.difference(&current.keys().cloned().collect()) {
        connection.sender.send(
            Notification::new(
                "textDocument/publishDiagnostics".into(),
                PublishDiagnosticsParams {
                    uri: uri.parse().expect("previously published URI remains valid"),
                    diagnostics: Vec::new(),
                    version: None,
                },
            )
            .into(),
        )?;
    }
    *published = current.keys().cloned().collect();
    for diagnostic in current.into_values() {
        connection
            .sender
            .send(Notification::new("textDocument/publishDiagnostics".into(), diagnostic).into())?;
    }
    Ok(())
}
fn failure(uri: Uri, message: String) -> PublishDiagnosticsParams {
    // A failed analysis has no compiler source span. Anchor the document-level
    // error at its start rather than inventing a declaration or token location.
    PublishDiagnosticsParams {
        uri,
        version: None,
        diagnostics: vec![Diagnostic {
            range: Range::default(),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("aside-lsp".into()),
            message,
            ..Default::default()
        }],
    }
}
type RequestResult = std::result::Result<Value, (ErrorCode, String)>;
fn dispatch(workspace: &mut Workspace, request: Request, encoding: Encoding) -> RequestResult {
    fn params<T: serde::de::DeserializeOwned>(
        value: Value,
    ) -> std::result::Result<T, (ErrorCode, String)> {
        serde_json::from_value(value).map_err(|error| (ErrorCode::InvalidParams, error.to_string()))
    }
    let result = match request.method.as_str() {
        "textDocument/semanticTokens/full" => {
            let params: SemanticTokensParams = params(request.params)?;
            workspace
                .semantic_tokens(&params.text_document.uri, encoding)
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "textDocument/formatting" => {
            let params: DocumentFormattingParams = params(request.params)?;
            workspace
                .format(&params.text_document.uri, encoding)
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "textDocument/completion" => {
            let params: CompletionParams = params(request.params)?;
            workspace
                .complete(
                    &params.text_document_position.text_document.uri,
                    params.text_document_position.position,
                    encoding,
                )
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "textDocument/hover" => {
            let params: HoverParams = params(request.params)?;
            workspace
                .hover(
                    &params.text_document_position_params.text_document.uri,
                    params.text_document_position_params.position,
                    encoding,
                )
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "textDocument/definition" => {
            let params: GotoDefinitionParams = params(request.params)?;
            workspace
                .definition(
                    &params.text_document_position_params.text_document.uri,
                    params.text_document_position_params.position,
                    encoding,
                )
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "textDocument/signatureHelp" => {
            let params: SignatureHelpParams = params(request.params)?;
            workspace
                .signature(
                    &params.text_document_position_params.text_document.uri,
                    params.text_document_position_params.position,
                    encoding,
                )
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "aside/nativeDocument" => {
            #[derive(Deserialize)]
            struct NativeDocumentParams {
                uri: Uri,
            }
            let params: NativeDocumentParams = params(request.params)?;
            let document = workspace
                .host
                .documents
                .get(params.uri.as_str())
                .ok_or_else(|| {
                    (
                        ErrorCode::InvalidParams,
                        format!("unknown native document {}", params.uri.as_str()),
                    )
                })?;
            Ok(json!({"text":document.text.as_ref(),"languageId":"aside"}))
        }
        _ => {
            return Err((
                ErrorCode::MethodNotFound,
                format!("unsupported method {}", request.method),
            ));
        }
    };
    result.map_err(|message| (ErrorCode::InvalidParams, message))
}
