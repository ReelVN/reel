use aside_compiler::{SourceMap, TextRange};
use reel_engine::aside::source::{CompiledStory, SourceError, compile_story_for_target};
use serde::Serialize;

const SOURCE: &str = "src/main.aside";

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum RunResult {
    Running,
    CompileError { diagnostics: Vec<Diagnostic> },
    RuntimeError { message: String },
}

#[derive(Serialize)]
pub(super) struct Diagnostic {
    source: &'static str,
    code: String,
    message: String,
    start: Option<Position>,
    end: Option<Position>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Position {
    line: usize,
    utf16_column: usize,
    byte_offset: usize,
}

pub(super) fn snippet(source: &str) -> Result<CompiledStory, RunResult> {
    compile_story_for_target(source, "main", SOURCE, 1, "main", "web").map_err(|error| {
        let map = SourceMap::new(source);
        let diagnostic = |code: &str, message: String, range: Option<TextRange>| {
            let position = |byte_offset| {
                map.position(byte_offset).map(|position| Position {
                    line: position.line,
                    utf16_column: position.utf16_column,
                    byte_offset,
                })
            };
            Diagnostic {
                source: SOURCE,
                code: code.into(),
                message,
                start: range.and_then(|range| position(range.start().into())),
                end: range.and_then(|range| position(range.end().into())),
            }
        };
        let compiler_diagnostic = |entry: aside_compiler::Diagnostic| {
            diagnostic(entry.code.as_str(), entry.message, Some(entry.range))
        };
        let diagnostics = match error {
            SourceError::Analysis(failure) => failure
                .diagnostics
                .into_iter()
                .map(compiler_diagnostic)
                .collect(),
            SourceError::NativeArgument(entry) => vec![compiler_diagnostic(entry.diagnostic)],
            SourceError::Effect(entry) => {
                vec![diagnostic("effect", entry.message, Some(entry.span))]
            }
            SourceError::Translation(entry) => {
                vec![diagnostic("translation", entry.message, Some(entry.span))]
            }
            SourceError::Entry => vec![diagnostic(
                "entry",
                "Define a story entry with fn main().".into(),
                None,
            )],
            other => vec![diagnostic("compile", other.to_string(), None)],
        };
        RunResult::CompileError { diagnostics }
    })
}
