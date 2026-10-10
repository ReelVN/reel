//! Author-facing static compilation. No runtime admission or execution occurs.
use aside_compiler::{
    AnalysisOptions, Diagnostic, SourceMap, TextRange, analyze, lowering, parse,
    semantic::NoModules,
};
use aside_vm::bytecode as bc;
use serde_json::{Value, json};
mod input;
pub use input::{InputError, MAX_SOURCE_BYTES, read_source};

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
}

/// Run the complete supported static pipeline, retaining entry and metadata
/// beside the raw B01 bytes. No on-disk release format is implied.
pub fn check(source: &str) -> Result<Checked, Failure> {
    let parsed = parse(source).map_err(|e| {
        error(
            "parse",
            aside_compiler::DiagnosticCode::BudgetExceeded,
            TextRange::empty(0.into()),
            e.to_string(),
        )
    })?;
    if !parsed.is_ok() {
        return Err(Failure {
            stage: "parse",
            diagnostics: parsed.diagnostics().to_vec(),
            code: None,
        });
    }
    // Filesystem paths are not B01 module identities. This is one standalone
    // logical `main` module, without a project/module resolver.
    let options = AnalysisOptions::default();
    let analyzed = analyze(&parsed, &options, &mut NoModules);
    if !analyzed.is_ok() {
        return Err(Failure {
            stage: "analyze",
            diagnostics: analyzed.diagnostics,
            code: None,
        });
    }
    let compilation = lowering::compile(
        analyzed.module().expect("successful analysis"),
        &lowering::Linkage::default(),
        lowering::Options::default(),
    )
    .map_err(|e| {
        error(
            "lower",
            aside_compiler::DiagnosticCode::InvalidHir,
            e.span,
            format!("{:?}: {}", e.kind, e.detail),
        )
    })?;
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
    Ok(Checked { compilation, bytes })
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
        let checked = check("fn main() { loop {} }").unwrap();
        assert_eq!(checked.compilation.metadata.canonical_path, "main");
        assert_eq!(
            checked.compilation.program.package().modules[0].initializer,
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
            let failure = check(source).err().unwrap();
            assert_eq!(failure.stage, stage);
            assert!(!failure.diagnostics.is_empty());
        }
    }
    #[test]
    fn unicode_coordinates_are_preserved() {
        let source = "let 中文 = 1;\r\nmissing();";
        let failure = check(source).err().unwrap();
        let diagnostic = diagnostic_json(&failure.diagnostics[0], &SourceMap::new(source));
        assert_eq!(diagnostic["range"]["start"]["line"], 1);
        assert_eq!(diagnostic["range"]["start"]["utf16_column"], 0);
        assert_eq!(diagnostic["range"]["byte_start"], 17);
    }
}
