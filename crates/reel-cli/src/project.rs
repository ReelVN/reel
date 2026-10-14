//! Source discovery rooted at the entry file, with optional external module roots.
use super::{Checked, Failure, ReportedDiagnostic, check_sources, error, read_source};
use aside_compiler::{
    DiagnosticCode, TextRange,
    semantic::{self, ImportKind, ImportRequest, SourceModule},
};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
};

pub struct Loaded {
    pub root: PathBuf,
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
fn module_file(
    path: &[String],
    roots: &BTreeMap<String, PathBuf>,
    input: &Path,
) -> Option<PathBuf> {
    if path == ["main"] {
        return Some(input.to_path_buf());
    }
    let mut file = roots.get(path.first()?)?.clone();
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
    pub fn check(&self) -> Result<Checked, Failure> {
        check_sources(&self.sources, &self.aliases)
    }
    pub fn entry(&self, requested: Option<&str>) -> String {
        let mut path = requested
            .unwrap_or("main::main")
            .split("::")
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if path.first().is_some_and(|root| {
            root != "main" && root != "game" && !self.aliases.contains_key(root)
        }) {
            path.insert(0, "game".into());
        }
        canonical(&path, &self.aliases).join("::")
    }
}
pub fn load(input: &Path) -> Result<Loaded, LoadError> {
    load_with_modules(input, &BTreeMap::new())
}

pub fn load_with_modules(
    input: &Path,
    modules: &BTreeMap<String, PathBuf>,
) -> Result<Loaded, LoadError> {
    let input = if input.is_dir() {
        input.join("main.aside")
    } else {
        input.to_path_buf()
    };
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
        source_path: Some(input.to_string_lossy().into()),
        version: "1".into(),
        source: text.into(),
    };
    let setup_error = |message| {
        failure(
            "resolve",
            &input.to_string_lossy(),
            TextRange::empty(0.into()),
            message,
            vec![source.clone()],
        )
    };
    let root = input
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|e| setup_error(e.to_string()))?;
    let mut roots = BTreeMap::from([("game".into(), root.clone())]);
    let mut aliases = BTreeMap::new();
    if let Some(stem) = input.file_stem().and_then(|v| v.to_str()) {
        aliases.insert(format!("game::{stem}"), "main".into());
    }
    let mut directories = BTreeMap::from([(root.clone(), "game".to_owned())]);
    for (alias, path) in modules {
        let valid = aside_compiler::lex(alias).is_ok_and(|l| {
            l.diagnostics.is_empty()
                && l.tokens.len() == 1
                && l.tokens[0].kind == aside_compiler::SyntaxKind::Ident
                && usize::from(l.tokens[0].range.end()) == alias.len()
        });
        if !valid || matches!(alias.as_str(), "main" | "game" | "reel") {
            return Err(setup_error(format!(
                "invalid or reserved module alias {alias}"
            )));
        }
        let directory = path
            .canonicalize()
            .map_err(|e| setup_error(format!("module {alias}: {e}")))?;
        if !directory.is_dir() {
            return Err(setup_error(format!("module {alias} needs a directory")));
        }
        let canonical = directories
            .entry(directory.clone())
            .or_insert_with(|| alias.clone())
            .clone();
        aliases.insert(alias.clone(), canonical.clone());
        roots.entry(canonical).or_insert(directory);
    }
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
        let file = module_file(&path, &roots, &input).expect("discovered module root");
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
            let root_aliases = aliases
                .iter()
                .filter(|(alias, target)| !alias.contains("::") && *target == &path[0])
                .map(|(alias, _)| alias.clone())
                .collect::<Vec<_>>();
            for alias in root_aliases {
                aliases.insert(
                    format!("{}::{}", alias, path[1..].join("::")),
                    target.clone(),
                );
            }
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
            let label = if path[0] == "game" {
                path[1..].join("/") + ".aside"
            } else {
                path.join("/") + ".aside"
            };
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
            if request.path.first().is_some_and(|root| root == "reel") {
                continue;
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
                if module_file(candidate, &roots, &input).is_some_and(|file| file.is_file()) {
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
    Ok(Loaded {
        root,
        input,
        sources,
        files,
        aliases,
    })
}
