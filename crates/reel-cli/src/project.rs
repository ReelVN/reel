//! Filesystem project policy; compilation remains in the shared source pipeline.
use super::{Checked, Failure, ReportedDiagnostic, check_sources, error, read_source};
use aside_compiler::{
    DiagnosticCode, TextRange,
    semantic::{self, ImportKind, ImportRequest, SourceModule},
};
use reel_package::{DisplaySettings, GameSettings};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
};

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub game: Game,
    pub display: DisplaySettings,
    pub modules: BTreeMap<String, Dependency>,
}
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Game {
    pub entry: String,
    pub title: String,
    pub font: Option<String>,
}
impl Default for Game {
    fn default() -> Self {
        Self {
            entry: "main::main".into(),
            title: "Reel".into(),
            font: None,
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub path: PathBuf,
    pub version: String,
}

pub struct Loaded {
    pub root: PathBuf,
    pub input: PathBuf,
    pub sources: Vec<SourceModule>,
    pub files: Vec<PathBuf>,
    pub aliases: BTreeMap<String, String>,
    pub entry: String,
    pub game: Option<GameSettings>,
}
pub struct LoadError {
    pub failure: Failure,
    pub sources: Vec<SourceModule>,
    pub input_error: Option<Box<super::InputError>>,
}
struct Root {
    path: PathBuf,
    version: String,
}

fn identifier(value: &str) -> bool {
    aside_compiler::lex(value).is_ok_and(|lexed| {
        lexed.diagnostics.is_empty()
            && lexed.tokens.len() == 1
            && lexed.tokens[0].kind == aside_compiler::SyntaxKind::Ident
            && usize::from(lexed.tokens[0].range.end()) == value.len()
    })
}
fn source_record(path: &Path, source: String) -> SourceModule {
    SourceModule {
        canonical_path: "<configuration>".into(),
        source_path: Some(path.to_string_lossy().into()),
        version: "1".into(),
        source: source.into(),
    }
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
fn module_path(root: &Path, segments: &[String]) -> PathBuf {
    if segments.is_empty() {
        return root.join("mod.aside");
    }
    let mut path = root.to_path_buf();
    for segment in segments {
        path.push(segment);
    }
    path.set_extension("aside");
    path
}
fn canonical_path(path: &[String], aliases: &BTreeMap<String, String>) -> Vec<String> {
    let request = ImportRequest {
        from: String::new(),
        path: path.to_vec(),
        kind: ImportKind::NamespaceOrSymbol,
    };
    semantic::canonical_import(&request, aliases).path.clone()
}

impl Loaded {
    pub fn check(&self) -> Result<Checked, Failure> {
        let mut checked = check_sources(&self.sources, &self.aliases)?;
        checked.game = self.game.clone();
        Ok(checked)
    }
    pub fn entry(&self, requested: Option<&str>) -> String {
        let requested = requested.unwrap_or(&self.entry);
        if self.game.is_none() {
            return requested.into();
        }
        let mut path = requested.split("::").map(str::to_owned).collect::<Vec<_>>();
        if path
            .first()
            .is_some_and(|root| root != "game" && !self.aliases.contains_key(root))
        {
            path.insert(0, "game".into());
        }
        canonical_path(&path, &self.aliases).join("::")
    }
}

pub fn load(input: &Path) -> Result<Loaded, LoadError> {
    let input = if input.is_dir() {
        input.join("game.toml")
    } else {
        input.to_path_buf()
    };
    let text = read_source(&input).map_err(|error| {
        let mut result = failure(
            "input",
            &input.to_string_lossy(),
            TextRange::empty(0.into()),
            error.message.clone(),
            Vec::new(),
        );
        result.input_error = Some(Box::new(error));
        result
    })?;
    if input
        .extension()
        .is_none_or(|extension| extension != "toml")
    {
        let root = input.parent().unwrap_or(Path::new(".")).to_path_buf();
        return Ok(Loaded {
            root,
            input: input.clone(),
            sources: vec![SourceModule {
                canonical_path: "main".into(),
                source_path: Some(input.to_string_lossy().into()),
                version: "1".into(),
                source: text.into(),
            }],
            files: vec![input],
            aliases: BTreeMap::new(),
            entry: "main::main".into(),
            game: None,
        });
    }
    let configuration = source_record(&input, text);
    let config: Config = toml::from_str(&configuration.source).map_err(|error| {
        let span = error
            .span()
            .map(|range| TextRange::new((range.start as u32).into(), (range.end as u32).into()))
            .unwrap_or_else(|| TextRange::empty(0.into()));
        failure(
            "configuration",
            &input.to_string_lossy(),
            span,
            error.to_string(),
            vec![configuration.clone()],
        )
    })?;
    let config_failure = |message| {
        failure(
            "configuration",
            &input.to_string_lossy(),
            TextRange::empty(0.into()),
            message,
            vec![configuration.clone()],
        )
    };
    if config.display.width == 0 || config.display.height == 0 {
        return Err(config_failure(
            "display width and height must be positive".into(),
        ));
    }
    let entry = config
        .game
        .entry
        .split("::")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if entry.len() < 2 || !entry.iter().all(|name| identifier(name)) {
        return Err(config_failure(
            "game.entry must name a module-level function, for example main::main".into(),
        ));
    }
    let root = input
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|error| config_failure(error.to_string()))?;
    let mut roots = BTreeMap::from([(
        "game".to_owned(),
        Root {
            path: root.clone(),
            version: "1".into(),
        },
    )]);
    let mut aliases = BTreeMap::new();
    let mut directories = BTreeMap::<PathBuf, (String, String)>::new();
    directories.insert(root.clone(), ("game".into(), "1".into()));
    for (alias, dependency) in &config.modules {
        if !identifier(alias) || matches!(alias.as_str(), "game" | "reel") {
            return Err(config_failure(format!(
                "invalid or reserved module alias {alias}"
            )));
        }
        let directory = root
            .join(&dependency.path)
            .canonicalize()
            .map_err(|error| config_failure(format!("module {alias}: {error}")))?;
        if !directory.is_dir() || dependency.version.is_empty() {
            return Err(config_failure(format!(
                "module {alias} needs a directory and a fixed version"
            )));
        }
        if let Some((canonical, version)) = directories.get(&directory) {
            if version != &dependency.version {
                return Err(config_failure(format!(
                    "the same directory was assigned different versions by {canonical} and {alias}"
                )));
            }
            aliases.insert(alias.clone(), canonical.clone());
        } else {
            directories.insert(
                directory.clone(),
                (alias.clone(), dependency.version.clone()),
            );
            aliases.insert(alias.clone(), alias.clone());
            roots.insert(
                alias.clone(),
                Root {
                    path: directory,
                    version: dependency.version.clone(),
                },
            );
        }
    }
    let mut entry_module = entry[..entry.len() - 1].to_vec();
    if entry_module[0] != "game" && !aliases.contains_key(&entry_module[0]) {
        entry_module.insert(0, "game".into());
    }
    let mut pending = VecDeque::from([canonical_path(&entry_module, &aliases)]);
    let mut sources = Vec::new();
    let mut files = Vec::new();
    let mut loaded = BTreeMap::<PathBuf, (String, String)>::new();
    let mut logical = BTreeMap::<String, ()>::new();
    let mut total_bytes = 0usize;
    while let Some(path) = pending.pop_front() {
        let path = canonical_path(&path, &aliases);
        let name = path.join("::");
        if logical.contains_key(&name) {
            continue;
        }
        let scope = roots
            .get(&path[0])
            .ok_or_else(|| config_failure(format!("unconfigured module root {}", path[0])))?;
        let file = module_path(&scope.path, &path[1..]);
        let physical = file
            .canonicalize()
            .map_err(|error| config_failure(format!("module {name}: {error}")))?;
        if let Some((canonical, version)) = loaded.get(&physical) {
            if version != &scope.version {
                return Err(config_failure(format!(
                    "source {name} was assigned two versions"
                )));
            }
            aliases.insert(name.clone(), canonical.clone());
            let roots_for_file = aliases
                .iter()
                .filter(|(alias, target)| !alias.contains("::") && *target == &path[0])
                .map(|(alias, _)| alias.clone())
                .collect::<Vec<_>>();
            for alias in roots_for_file {
                aliases.insert(
                    format!("{}::{}", alias, path[1..].join("::")),
                    canonical.clone(),
                );
            }
            logical.insert(name, ());
            continue;
        }
        let source = read_source(&file)
            .map_err(|error| config_failure(format!("module {name}: {}", error.message)))?;
        total_bytes += source.len();
        if total_bytes > semantic::AnalysisLimits::default().max_total_source_bytes {
            return Err(config_failure("project source byte budget exceeded".into()));
        }
        let label = if path[0] == "game" {
            path[1..].join("/") + ".aside"
        } else {
            path.join("/") + ".aside"
        };
        let discovery =
            semantic::discover_imports(&source, &name, semantic::AnalysisLimits::default())
                .map_err(|error| config_failure(error.to_string()))?;
        loaded.insert(physical, (name.clone(), scope.version.clone()));
        logical.insert(name.clone(), ());
        sources.push(SourceModule {
            canonical_path: name.clone(),
            source_path: Some(label.clone()),
            version: scope.version.clone(),
            source: source.into(),
        });
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
            let requested = canonical_path(&request.path, &aliases);
            let Some(scope) = roots.get(&requested[0]) else {
                return Err(failure(
                    "resolve",
                    &label,
                    span,
                    format!("unconfigured module root {}", requested[0]),
                    sources,
                ));
            };
            let mut found = false;
            for candidate in [
                (request.kind != ImportKind::Symbol).then_some(requested.as_slice()),
                (request.kind != ImportKind::Wildcard && requested.len() > 1)
                    .then_some(&requested[..requested.len() - 1]),
            ]
            .into_iter()
            .flatten()
            {
                if module_path(&scope.path, &candidate[1..]).is_file() {
                    pending.push_back(candidate.to_vec());
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
    let game = GameSettings {
        title: config.game.title,
        display: config.display,
        font: config.game.font,
    };
    Ok(Loaded {
        root,
        input: input.clone(),
        sources,
        files,
        aliases,
        entry: config.game.entry,
        game: Some(game),
    })
}
