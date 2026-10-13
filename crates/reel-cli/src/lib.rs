//! Author-facing static compilation. No runtime admission or execution occurs.
use aside_compiler::{Diagnostic, SourceMap, TextRange, lowering};
use aside_vm::bytecode as bc;
use reel_engine::aside::source::{self, CompiledStory, SourceCompilation, SourceError};
use serde_json::{Value, json};
mod input;
pub use input::{InputError, read_source};
mod artifact;
pub mod output;
pub use artifact::{Artifact, build, package_limits_json};

#[derive(Debug)]
pub struct Failure {
    pub stage: &'static str,
    pub diagnostics: Vec<Diagnostic>,
    pub code: Option<String>,
}

fn error(
    stage: &'static str,
    code: aside_compiler::DiagnosticCode,
    span: TextRange,
    message: String,
) -> Failure {
    Failure {
        stage,
        code: Some(format!("REEL-{}", stage.to_ascii_uppercase())),
        diagnostics: vec![Diagnostic {
            code,
            severity: aside_compiler::Severity::Error,
            range: span,
            message,
            related: Vec::new(),
            fix: None,
        }],
    }
}

pub struct Checked {
    pub compilation: lowering::Compilation,
    pub bytes: Vec<u8>,
    pub declarations: Vec<reel_package::Declaration>,
}

/// Run the complete supported static pipeline, retaining entry and metadata
/// beside the raw B01 bytes. No on-disk release format is implied.
pub fn check(source: &str, source_path: &str) -> Result<Checked, Failure> {
    let SourceCompilation {
        module,
        compilation,
    } = source::compile_module(source, "main", source_path, 1).map_err(source_failure)?;
    let limits = bc::Limits::default();
    let convert = |e: bc::Error| {
        let span = e
            .source
            .map(|s| TextRange::new(s.start.into(), s.end.into()))
            .unwrap_or(compilation.metadata.source_span);
        error(
            "bytecode",
            aside_compiler::DiagnosticCode::InvalidHir,
            span,
            e.to_string(),
        )
    };
    let bytes = bc::encode(compilation.program.package(), limits).map_err(convert)?;
    let decoded = bc::VerifiedProgram::decode(&bytes, limits).map_err(convert)?;
    if decoded.package() != compilation.program.package() {
        return Err(error(
            "bytecode",
            aside_compiler::DiagnosticCode::InvalidHir,
            compilation.metadata.source_span,
            "B01 round trip changed program data".into(),
        ));
    }
    let declarations = artifact::declarations(&module, &compilation)?;
    Ok(Checked {
        compilation,
        bytes,
        declarations,
    })
}

pub fn source_failure(failure: SourceError) -> Failure {
    use aside_compiler::DiagnosticCode;
    match failure {
        SourceError::Analysis(failure) => {
            Failure {
                stage: if failure.diagnostics.iter().any(|d| {
                    d.code.as_str().starts_with("ASP") || d.code.as_str().starts_with("ASL")
                }) {
                    "parse"
                } else {
                    "analyze"
                },
                diagnostics: failure.diagnostics,
                code: None,
            }
        }
        SourceError::Lowering(failure) => error(
            "lower",
            DiagnosticCode::InvalidHir,
            failure.span,
            failure.detail,
        ),
        SourceError::Format(failure) => error(
            "bytecode",
            DiagnosticCode::InvalidHir,
            failure
                .source
                .map(|s| TextRange::new(s.start.into(), s.end.into()))
                .unwrap_or_else(|| TextRange::empty(0.into())),
            failure.to_string(),
        ),
        SourceError::Source(failure) => error(
            "parse",
            DiagnosticCode::BudgetExceeded,
            TextRange::empty(0.into()),
            failure.to_string(),
        ),
        SourceError::Entry => error(
            "entry",
            DiagnosticCode::InvalidCall,
            TextRange::empty(0.into()),
            "entry must be an ordinary module function with no required arguments".into(),
        ),
    }
}

pub fn into_story(checked: Checked, entry: &str) -> Result<CompiledStory, Failure> {
    let entry = artifact::select_entry(&checked, Some(entry))?.expect("requested entry");
    Ok(CompiledStory {
        program: checked.compilation.program,
        initializer: checked.compilation.entry,
        entry: bc::FunctionId(entry.function_id),
    })
}

pub fn decode_story(bytes: &[u8]) -> Result<CompiledStory, reel_package::Error> {
    let (program, metadata) =
        reel_package::decode(bytes, reel_package::Limits::default())?.into_parts();
    let entry = metadata.game_entry.ok_or_else(|| reel_package::Error {
        kind: reel_package::ErrorKind::Metadata,
        detail: "this package contains only a module; use reel build <source.aside> --output <story.asb> to build a runnable story".into(),
    })?;
    Ok(CompiledStory {
        program,
        initializer: bc::FunctionId(metadata.initializer.function_id),
        entry: bc::FunctionId(entry.function_id),
    })
}

pub fn diagnostic_json(diagnostic: &Diagnostic, map: &SourceMap<'_>) -> Value {
    let range = |span: TextRange| {
        let position = |offset: usize| {
            map.position(offset).map(|p| {
                json!({
                    "line": p.line, "byte_column": p.byte_column,
                    "scalar_column": p.scalar_column, "utf16_column": p.utf16_column
                })
            })
        };
        json!({"byte_start": u32::from(span.start()), "byte_end": u32::from(span.end()),
            "start": position(usize::from(span.start())), "end": position(usize::from(span.end()))})
    };
    json!({
        "code": diagnostic.code.as_str(),
        "severity": match diagnostic.severity { aside_compiler::Severity::Error => "error", aside_compiler::Severity::Warning => "warning" },
        "message": diagnostic.message, "range": range(diagnostic.range),
        "related": diagnostic.related.iter().map(|r| json!({"range": range(r.range), "message": r.message})).collect::<Vec<_>>(),
        "fix": diagnostic.fix.as_ref().map(|f| json!({"range": range(f.range), "replacement": f.replacement}))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_pipeline_is_static() {
        let checked = check("fn main() { loop {} }", "story.aside").unwrap();
        assert_eq!(checked.compilation.metadata.canonical_path, "main");
        let module = checked.compilation.program.package().functions
            [checked.compilation.entry.index()]
        .module;
        assert_eq!(
            checked.compilation.program.package().modules[module.index()].initializer,
            Some(checked.compilation.entry)
        );
        assert!(!checked.bytes.is_empty());
    }
    #[test]
    fn rejects_parse_semantic_import_and_lowering_failures() {
        for (source, stage) in [
            ("let =;", "parse"),
            ("missing();", "analyze"),
            ("use reel::story;", "analyze"),
            ("fn main() { [...[1]] }", "lower"),
        ] {
            let failure = check(source, "story.aside").err().unwrap();
            assert_eq!(failure.stage, stage);
            assert!(!failure.diagnostics.is_empty());
        }
    }
    #[test]
    fn unicode_coordinates_are_preserved() {
        let source = "let 中文 = 1;\r\nmissing();";
        let failure = check(source, "story.aside").err().unwrap();
        let diagnostic = diagnostic_json(&failure.diagnostics[0], &SourceMap::new(source));
        assert_eq!(diagnostic["range"]["start"]["line"], 1);
        assert_eq!(diagnostic["range"]["start"]["utf16_column"], 0);
        assert_eq!(diagnostic["range"]["byte_start"], 17);
    }
}
