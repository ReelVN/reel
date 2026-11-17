//! Project file publication around the compiler's shared syntax-tree formatter.
use aside_compiler::{FormatError, semantic::SourceModule};
use reel_cli::{Failure, ReportedDiagnostic};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};

struct Change {
    path: PathBuf,
    text: String,
}

pub(super) fn run(check: bool) -> io::Result<u8> {
    let project = match reel_cli::project::locate(Path::new(".")) {
        Ok(project) => project,
        Err(error) => {
            super::failure(false, "project", None, &error.to_string())?;
            return Ok(2);
        }
    };
    let files = match reel_cli::project::source_files(&project.root) {
        Ok(files) => files,
        Err(error) => {
            super::failure(false, "input", project.root.to_str(), &error.to_string())?;
            return Ok(2);
        }
    };
    let mut changes = Vec::new();
    for path in &files {
        let label = path.strip_prefix(&project.root).unwrap().to_string_lossy();
        let source = match reel_cli::read_source(path) {
            Ok(source) => source,
            Err(error) => {
                super::failure(false, "input", Some(&label), &error.message)?;
                return Ok(error.exit_code);
            }
        };
        match aside_compiler::format(&source) {
            Ok(text) if text != source => changes.push(Change {
                path: path.clone(),
                text,
            }),
            Ok(_) => {}
            Err(FormatError::SourceTooLarge(error)) => {
                super::failure(false, "format", Some(&label), &error.to_string())?;
                return Ok(1);
            }
            Err(FormatError::Syntax(diagnostics)) => {
                let input = SourceModule {
                    canonical_path: "main".into(),
                    source_path: Some(label.to_string()),
                    version: "1".into(),
                    source: source.into(),
                };
                let failure = Failure {
                    stage: "format",
                    code: None,
                    diagnostics: diagnostics
                        .into_iter()
                        .map(|diagnostic| ReportedDiagnostic {
                            source: input.source_path.clone(),
                            diagnostic,
                        })
                        .collect(),
                };
                return super::source_error(false, &label, &[input], failure);
            }
        }
    }
    if check {
        for change in &changes {
            writeln!(
                io::stdout().lock(),
                "Needs formatting: {}",
                change.path.strip_prefix(&project.root).unwrap().display()
            )?;
        }
        return Ok(u8::from(!changes.is_empty()));
    }
    // Every candidate has parsed successfully before the first publication.
    for change in &changes {
        let published = match reel_storage::publish(&change.path, true, |file| {
            file.write_all(change.text.as_bytes())
        }) {
            Ok(published) => published,
            Err(error) => {
                super::failure(false, "output", change.path.to_str(), &error.to_string())?;
                return Ok(2);
            }
        };
        if let Some(warning) = published.cleanup_warning {
            writeln!(io::stderr().lock(), "warning: {warning}")?;
        }
    }
    writeln!(
        io::stdout().lock(),
        "Formatted {} of {} Aside files.",
        changes.len(),
        files.len()
    )?;
    Ok(0)
}
