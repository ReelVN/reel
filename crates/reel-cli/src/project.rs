//! Discover the imported source graph of the nearest Reel project.
use super::{Checked, Failure, ReportedDiagnostic, error, read_source};
use aside_compiler::{
    DiagnosticCode, TextRange,
    semantic::{self, ImportKind, ImportRequest, SourceModule},
};
use std::{
    collections::{BTreeMap, VecDeque},
    io,
    path::{Path, PathBuf},
};

/// The fixed entry shared by project commands and compiled artifacts.
pub const ENTRY: &str = "main::main";

/// Project directory and its fixed source entry.
pub struct Location {
    pub root: PathBuf,
    pub input: PathBuf,
}

/// Find `src/main.aside` in this directory or an ancestor, without scanning trees.
pub fn locate(directory: &Path) -> io::Result<Location> {
    locate_with(directory, &Path::is_file)
}

/// Query project membership for compiled artifacts that may live without source.
pub fn find(directory: &Path) -> io::Result<Option<Location>> {
    find_with(directory, &Path::is_file)
}

/// Share project discovery with editor overlays containing an unsaved entry.
pub fn locate_with(directory: &Path, exists: &dyn Fn(&Path) -> bool) -> io::Result<Location> {
    find_with(directory, exists)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Reel project not found: expected src/main.aside in this directory or an ancestor",
        )
    })
}

fn find_with(directory: &Path, exists: &dyn Fn(&Path) -> bool) -> io::Result<Option<Location>> {
    let directory = directory.canonicalize()?;
    if !directory.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "project discovery requires a directory",
        ));
    }
    Ok(directory
        .ancestors()
        .find(|root| exists(&root.join("src/main.aside")))
        .map(|root| Location {
            root: root.to_path_buf(),
            input: root.join("src/main.aside"),
        }))
}

pub struct Loaded {
    /// Source module root, independent of the project's resource/build directory.
    pub root: PathBuf,
    /// Canonical project directory for default outputs and loose resources.
    pub project_root: PathBuf,
    pub input: PathBuf,
    pub sources: Vec<SourceModule>,
    pub files: Vec<PathBuf>,
    pub aliases: BTreeMap<String, String>,
}
pub struct LoadError {
    pub failure: Failure,
    pub sources: Vec<SourceModule>,
    pub input_error: Option<Box<super::InputError>>,
}
fn failure(
    stage: &'static str,
    path: &str,
    range: TextRange,
    message: String,
    sources: Vec<SourceModule>,
) -> LoadError {
    let mut failure = error(stage, DiagnosticCode::ModuleResolution, range, message);
    failure.diagnostics[0].source = Some(path.into());
    LoadError {
        failure,
        sources,
        input_error: None,
    }
}
fn canonical(path: &[String], aliases: &BTreeMap<String, String>) -> Vec<String> {
    semantic::canonical_import(
        &ImportRequest {
            from: String::new(),
            path: path.to_vec(),
            kind: ImportKind::NamespaceOrSymbol,
        },
        aliases,
    )
    .path
    .clone()
}
fn module_file(path: &[String], root: &Path, input: &Path) -> Option<PathBuf> {
    if path == ["main"] {
        return Some(input.to_path_buf());
    }
    if path.first()?.as_str() != "game" {
        return None;
    }
    let mut file = root.to_path_buf();
    if path.len() == 1 {
        file.push("mod");
    } else {
        for part in &path[1..] {
            file.push(part);
        }
    }
    file.set_extension("aside");
    Some(file)
}
impl Loaded {
    /// Project resources retain their logical path relative to `resources/`.
    pub fn assets(&self) -> PathBuf {
        self.project_root.join("resources")
    }
    pub fn check(&self) -> Result<Checked, Failure> {
        self.check_with_progress(std::env::consts::OS, &mut |_| {})
    }
    /// Compile the loaded graph while reporting its real stage/module boundaries.
    pub fn check_with_progress(
        &self,
        target: &str,
        progress: &mut dyn FnMut(aside_compiler::Progress),
    ) -> Result<Checked, Failure> {
        let checked =
            super::check_sources_with_progress(&self.sources, &self.aliases, target, progress)?;
        super::artifact::select_entry(&checked, Some(ENTRY))?;
        Ok(checked)
    }
}
pub fn load(input: &Path) -> Result<Loaded, LoadError> {
    load_with_progress(input, &mut |_| {})
}

/// Run the same pipeline while reporting completed stages and modules.
pub fn load_with_progress(
    input: &Path,
    progress: &mut dyn FnMut(aside_compiler::Progress),
) -> Result<Loaded, LoadError> {
    progress(aside_compiler::Progress {
        stage: aside_compiler::ProgressStage::Discover,
        completed: 0,
        total: 0,
    });
    let location = locate(input).map_err(|error| {
        let mut result = failure(
            "input",
            &input.to_string_lossy(),
            TextRange::empty(0.into()),
            error.to_string(),
            vec![],
        );
        result.input_error = Some(Box::new(super::InputError {
            code: "REEL-INPUT-IO",
            message: format!("cannot locate source: {error}"),
            byte_range: None,
            exit_code: 2,
        }));
        result
    })?;
    let input = location.input;
    let text = read_source(&input).map_err(|error| {
        let mut result = failure(
            "input",
            &input.to_string_lossy(),
            TextRange::empty(0.into()),
            error.message.clone(),
            vec![],
        );
        result.input_error = Some(Box::new(error));
        result
    })?;
    let source = SourceModule {
        canonical_path: "main".into(),
        source_path: Some("src/main.aside".into()),
        version: "1".into(),
        source: text.into(),
    };
    let root = location.root.join("src");
    let mut aliases = BTreeMap::from([("game::main".into(), "main".into())]);
    let mut first = Some(source);
    let mut pending = VecDeque::from([(vec!["main".into()], None::<(String, TextRange)>)]);
    let mut sources = Vec::new();
    let mut files = Vec::new();
    let mut loaded = BTreeMap::<PathBuf, String>::new();
    let mut logical = std::collections::BTreeSet::new();
    let mut total_bytes = 0usize;
    while let Some((path, origin)) = pending.pop_front() {
        let path = canonical(&path, &aliases);
        let name = path.join("::");
        if logical.contains(&name) {
            continue;
        }
        let file = module_file(&path, &root, &input).expect("discovered module root");
        let (origin_path, span) =
            origin.unwrap_or_else(|| (input.to_string_lossy().into(), TextRange::empty(0.into())));
        let physical = file.canonicalize().map_err(|e| {
            failure(
                "input",
                &origin_path,
                span,
                format!("module {name}: {e}"),
                sources.clone(),
            )
        })?;
        if let Some(target) = loaded.get(&physical) {
            aliases.insert(name.clone(), target.clone());
            logical.insert(name);
            continue;
        }
        let source = if name == "main" {
            first.take().expect("entry loaded once")
        } else {
            let source = read_source(&file).map_err(|e| {
                failure(
                    "input",
                    &origin_path,
                    span,
                    format!("module {name}: {}", e.message),
                    sources.clone(),
                )
            })?;
            let label = "src/".to_owned() + &path[1..].join("/") + ".aside";
            SourceModule {
                canonical_path: name.clone(),
                source_path: Some(label),
                version: "1".into(),
                source: source.into(),
            }
        };
        total_bytes += source.source.len();
        if total_bytes > semantic::AnalysisLimits::default().max_total_source_bytes {
            return Err(failure(
                "input",
                &origin_path,
                span,
                "project source byte budget exceeded".into(),
                sources,
            ));
        }
        let discovery =
            semantic::discover_imports(&source.source, &name, semantic::AnalysisLimits::default())
                .map_err(|e| {
                    failure("parse", &origin_path, span, e.to_string(), sources.clone())
                })?;
        let label = source.source_path.clone().unwrap();
        loaded.insert(physical, name.clone());
        logical.insert(name);
        sources.push(source);
        files.push(file);
        progress(aside_compiler::Progress {
            stage: aside_compiler::ProgressStage::Discover,
            completed: sources.len(),
            total: 0,
        });
        if !discovery.diagnostics.is_empty() {
            return Err(LoadError {
                failure: Failure {
                    stage: "parse",
                    code: None,
                    diagnostics: discovery
                        .diagnostics
                        .into_iter()
                        .map(|diagnostic| ReportedDiagnostic {
                            source: Some(label.clone()),
                            diagnostic,
                        })
                        .collect(),
                },
                sources,
                input_error: None,
            });
        }
        for (request, span) in discovery.requests {
            let Some(first) = request.path.first() else {
                continue;
            };
            if matches!(first.as_str(), "reel" | "std") {
                continue;
            }
            if !matches!(first.as_str(), "main" | "game") {
                aliases
                    .entry(first.clone())
                    .or_insert_with(|| format!("game::{first}"));
            }
            let requested = canonical(&request.path, &aliases);
            let mut found = false;
            for candidate in [
                (request.kind != ImportKind::Symbol).then_some(requested.as_slice()),
                (request.kind != ImportKind::Wildcard && requested.len() > 1)
                    .then_some(&requested[..requested.len() - 1]),
            ]
            .into_iter()
            .flatten()
            {
                if module_file(candidate, &root, &input).is_some_and(|file| file.is_file()) {
                    pending.push_back((candidate.to_vec(), Some((label.clone(), span))));
                    found = true;
                }
            }
            if !found {
                return Err(failure(
                    "resolve",
                    &label,
                    span,
                    format!("source module {} was not found", request.path.join("::")),
                    sources,
                ));
            }
        }
    }
    progress(aside_compiler::Progress {
        stage: aside_compiler::ProgressStage::Discover,
        completed: sources.len(),
        total: sources.len(),
    });
    Ok(Loaded {
        root,
        project_root: location.root,
        input,
        sources,
        files,
        aliases,
    })
}
