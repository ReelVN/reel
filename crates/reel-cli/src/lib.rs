//! Author compilation, relocatable packages, and shared player startup.
use aside_compiler::{Diagnostic, SourceMap, TextRange, lowering};
use aside_vm::bytecode as bc;
use reel_engine::aside::source::{self, CompiledStory, ProjectSourceCompilation, SourceError};
use serde_json::{Value, json};
mod input;
pub use input::{InputError, read_source};
mod artifact;
mod conversion_identity;
pub mod output;
mod prepare_audio;
mod prepare_media;
pub use artifact::{
    Artifact, BuildProfile, build, build_export, build_with_profile, package_limits_json,
};
pub mod development;
pub mod distribution;
pub mod editor;
pub mod localization;
pub mod playback;
pub mod prepare_effects;
pub mod prepare_font;
pub mod prepare_image;
pub mod prepare_texture;
pub mod prepare_video;
pub mod project;
pub mod resource_conversion;

#[derive(Debug)]
pub struct Failure {
    pub stage: &'static str,
    pub diagnostics: Vec<ReportedDiagnostic>,
    pub code: Option<String>,
}
#[derive(Debug)]
pub struct ReportedDiagnostic {
    pub source: Option<String>,
    pub diagnostic: Diagnostic,
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
        diagnostics: vec![ReportedDiagnostic {
            source: None,
            diagnostic: Diagnostic {
                code,
                severity: aside_compiler::Severity::Error,
                range: span,
                message,
                related: Vec::new(),
                fix: None,
            },
        }],
    }
}

pub struct Checked {
    pub compilation: lowering::ProjectCompilation,
    pub modules: Vec<reel_package::ModuleMetadata>,
    pub catalogs: std::sync::Arc<source::CatalogProducts>,
    pub effects: std::sync::Arc<source::EffectProducts>,
}

/// Compile and verify source, retaining the program, entry and module metadata.
/// Artifact builders encode the selected profile when an output is requested.
pub fn check(source: &str, source_path: &str) -> Result<Checked, Failure> {
    check_sources(
        &[aside_compiler::semantic::SourceModule {
            canonical_path: "main".into(),
            source_path: Some(source_path.into()),
            version: "1".into(),
            source: source.into(),
        }],
        &std::collections::BTreeMap::new(),
    )
}

pub fn check_sources(
    inputs: &[aside_compiler::semantic::SourceModule],
    aliases: &std::collections::BTreeMap<String, String>,
) -> Result<Checked, Failure> {
    check_sources_with_progress(inputs, aliases, std::env::consts::OS, &mut |_| {})
}

/// Run the same pipeline while reporting completed stages and modules.
pub fn check_sources_with_progress(
    inputs: &[aside_compiler::semantic::SourceModule],
    aliases: &std::collections::BTreeMap<String, String>,
    target: &str,
    progress: &mut dyn FnMut(aside_compiler::Progress),
) -> Result<Checked, Failure> {
    let ProjectSourceCompilation {
        compilation,
        catalogs,
        effects,
    } = source::compile_project_with_progress(inputs, aliases, 1, target, progress).map_err(
        |failure| {
            let mut failure = source_failure(failure);
            for diagnostic in &mut failure.diagnostics {
                if let Some(module) = diagnostic.source.as_deref()
                    && let Some(input) = inputs.iter().find(|input| input.canonical_path == module)
                {
                    diagnostic.source = input.source_path.clone().or_else(|| Some(module.into()));
                }
            }
            failure
        },
    )?;
    let metadata = compilation
        .modules
        .iter()
        .map(|compiled| artifact::module_metadata(compiled, &compilation.program))
        .collect();
    Ok(Checked {
        compilation,
        modules: metadata,
        catalogs,
        effects,
    })
}

pub fn source_failure(failure: SourceError) -> Failure {
    use aside_compiler::DiagnosticCode;
    match failure {
        SourceError::Effect(failure) => {
            let mut result = error(
                "effect",
                DiagnosticCode::InvalidCall,
                failure.span,
                failure.message,
            );
            result.diagnostics[0].source = Some(failure.module);
            result
        }
        SourceError::EffectProduct(failure) => error(
            "effect",
            DiagnosticCode::InvalidHir,
            TextRange::empty(0.into()),
            failure.to_string(),
        ),
        SourceError::Translation(failure) => {
            let mut result = error(
                "localization",
                DiagnosticCode::InvalidCall,
                failure.span,
                failure.message,
            );
            result.diagnostics[0].source = Some(failure.module);
            result
        }
        SourceError::Analysis(failure) => {
            Failure {
                stage: if failure.diagnostics.iter().any(|d| {
                    d.code.as_str().starts_with("ASP") || d.code.as_str().starts_with("ASL")
                }) {
                    "parse"
                } else {
                    "analyze"
                },
                diagnostics: failure
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| ReportedDiagnostic {
                        source: None,
                        diagnostic,
                    })
                    .collect(),
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
        SourceError::ProjectAnalysis(failure) => Failure {
            stage: if failure.diagnostics.iter().any(|d| {
                d.diagnostic.code.as_str().starts_with("ASP")
                    || d.diagnostic.code.as_str().starts_with("ASL")
            }) {
                "parse"
            } else {
                "analyze"
            },
            code: None,
            diagnostics: failure
                .diagnostics
                .into_iter()
                .map(|d| ReportedDiagnostic {
                    source: Some(d.module),
                    diagnostic: d.diagnostic,
                })
                .collect(),
        },
        SourceError::ProjectLowering(failure) => {
            let mut result = error(
                "lower",
                DiagnosticCode::InvalidHir,
                failure.error.span,
                failure.error.detail,
            );
            result.diagnostics[0].source = Some(failure.module);
            result
        }
    }
}

pub fn into_story(checked: Checked, entry: &str) -> Result<CompiledStory, Failure> {
    let entry = artifact::select_entry(&checked, Some(entry))?.expect("requested entry");
    Ok(CompiledStory {
        program: checked.compilation.program,
        initializer: checked.compilation.initializer,
        entry: bc::FunctionId(entry.function_id),
        catalogs: checked.catalogs,
        effects: checked.effects,
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
        assert_eq!(
            checked.compilation.modules[0].metadata.canonical_path,
            "main"
        );
        let module = checked.compilation.program.package().functions
            [checked.compilation.initializer.index()]
        .module;
        assert_eq!(
            checked.compilation.program.package().modules[module.index()].initializer,
            Some(checked.compilation.initializer)
        );
        let program = &checked.compilation.program;
        let bytes = bc::encode(program.package(), bc::Limits::default()).unwrap();
        assert_eq!(bytes.len(), program.stats().encoded_bytes);
        let decoded = bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap();
        assert_eq!(decoded.package(), program.package());
    }
    #[test]
    fn rejects_parse_semantic_import_and_lowering_failures() {
        for (source, stage) in [
            ("let =;", "parse"),
            ("missing();", "analyze"),
            ("use reel::story;", "analyze"),
            ("fn main() { let [a]=[1]; a }", "lower"),
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
        let diagnostic =
            diagnostic_json(&failure.diagnostics[0].diagnostic, &SourceMap::new(source));
        assert_eq!(diagnostic["range"]["start"]["line"], 1);
        assert_eq!(diagnostic["range"]["start"]["utf16_column"], 0);
        assert_eq!(diagnostic["range"]["byte_start"], 17);
    }
}
