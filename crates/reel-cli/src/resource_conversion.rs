//! Prepare selected source resources while preserving their author-facing names.
//!
//! An optional project-root `resources.toml` contains ordered `[[targets.NAME]]`
//! rules. The first matching logical path selects `convert = "texture"`,
//! `"opus"`, or `"copy"`; unmatched resources retain their selected source file.
//! `match` is a case-sensitive glob with `/` separators (`*` stays within one
//! component; `**` crosses directories). Source target selection runs first.
//! Texture `scale` changes texel density while preserving logical coordinates;
//! `size = [width, height]` instead fits artwork inside an explicit box.
//!
//! Conversion runs one resource at a time. Immutable cache objects are keyed by
//! source content, normalized recipe and the running tool's content identity.
//! Payloads remain on disk; hashing uses a fixed 64 KiB buffer. A complete index
//! is atomically published only after all selected resources are ready.
use crate::prepare_texture;
use globset::{GlobBuilder, GlobMatcher};
use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
use reel_package::resource_target::ResourceTarget;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

/// Maximum conversion workspace used by automatic project commands (512 MiB).
/// Image conversion reserves its complete pixel/workspace estimate before decode;
/// audio uses its streaming decoder and encoder's shared budget pool.
pub const MEMORY_BYTES: u64 = 512 << 20;

/// Aggregate work for a preparation invocation; byte counts cover selected files.
#[derive(Default, Debug, Serialize)]
pub struct Report {
    /// Resources converted during this invocation.
    pub converted: usize,
    /// Previously prepared resources reused by content identity.
    pub reused: usize,
    /// Resources whose selected source is consumed directly.
    pub unchanged: usize,
    /// Selected original payload length, before conversion.
    pub source_bytes: u64,
    /// Selected prepared or unchanged payload length.
    pub output_bytes: u64,
    /// Peak image reservation or measured audio budget use, excluding thread stacks.
    pub workspace_bytes: u64,
}

/// A complete selected resource view for development reads or packaging.
pub struct Prepared {
    /// Logical author paths mapped to immutable prepared files or selected sources.
    pub files: BTreeMap<String, PathBuf>,
    /// Actual work performed by this invocation.
    pub report: Report,
    /// Atomically published cache index, present when this target has rules.
    pub index: Option<PathBuf>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    #[serde(default)]
    targets: BTreeMap<String, Vec<RuleInput>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleInput {
    #[serde(rename = "match")]
    pattern: String,
    convert: Convert,
    scale: Option<f64>,
    size: Option<[u32; 2]>,
    format: Option<prepare_texture::Format>,
    power_of_two: Option<bool>,
    bitrate: Option<u32>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Convert {
    Copy,
    Texture,
    Opus,
}

#[derive(Serialize)]
#[serde(tag = "convert", rename_all = "lowercase")]
enum Recipe {
    Copy,
    Texture {
        scale: f64,
        size: Option<[u32; 2]>,
        format: prepare_texture::Format,
        power_of_two: bool,
    },
    Opus {
        bitrate: u32,
    },
}

struct Rule {
    matcher: GlobMatcher,
    recipe: Recipe,
}

impl RuleInput {
    fn compile(self) -> io::Result<Rule> {
        if self.pattern.is_empty() || self.pattern.contains('\\') || self.pattern.starts_with('/') {
            return Err(invalid(
                "match must be a nonempty relative glob using '/' separators",
            ));
        }
        let matcher = GlobBuilder::new(&self.pattern)
            .literal_separator(true)
            .backslash_escape(false)
            .build()
            .map_err(invalid)?
            .compile_matcher();
        let recipe = match self.convert {
            Convert::Copy => {
                if self.scale.is_some()
                    || self.size.is_some()
                    || self.format.is_some()
                    || self.power_of_two.is_some()
                    || self.bitrate.is_some()
                {
                    return Err(invalid("copy rules only accept match and convert"));
                }
                Recipe::Copy
            }
            Convert::Texture => {
                if self.bitrate.is_some() {
                    return Err(invalid("bitrate belongs to opus rules"));
                }
                if self.scale.is_some() && self.size.is_some() {
                    return Err(invalid("texture rules select either scale or size"));
                }
                let scale = self.scale.unwrap_or(1.);
                if !scale.is_finite() || scale <= 0. || scale > 1. {
                    return Err(invalid(
                        "texture scale must be finite and greater than zero up to one",
                    ));
                }
                if self.size.is_some_and(|size| size.contains(&0)) {
                    return Err(invalid("texture size must contain two positive dimensions"));
                }
                Recipe::Texture {
                    scale,
                    size: self.size,
                    format: self.format.unwrap_or_default(),
                    power_of_two: self.power_of_two.unwrap_or(false),
                }
            }
            Convert::Opus => {
                if self.scale.is_some()
                    || self.size.is_some()
                    || self.format.is_some()
                    || self.power_of_two.is_some()
                {
                    return Err(invalid("opus rules accept match, convert and bitrate"));
                }
                let bitrate = self.bitrate.unwrap_or(96_000);
                if !(6_000..=512_000).contains(&bitrate) {
                    return Err(invalid(
                        "opus bitrate must be between 6000 and 512000 bits per second",
                    ));
                }
                Recipe::Opus { bitrate }
            }
        };
        Ok(Rule { matcher, recipe })
    }
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

fn rules(root: &Path, target: &ResourceTarget) -> io::Result<Vec<Rule>> {
    let path = root.join("resources.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let config: Configuration =
        toml::from_str(&text).map_err(|error| invalid(format!("{}: {error}", path.display())))?;
    let mut selected = Vec::new();
    for (name, inputs) in config.targets {
        ResourceTarget::parse(&name)
            .map_err(|error| invalid(format!("{}: targets.{name}: {error}", path.display())))?;
        for (index, input) in inputs.into_iter().enumerate() {
            let rule = input.compile().map_err(|error| {
                invalid(format!(
                    "{}: targets.{name}[{}]: {error}",
                    path.display(),
                    index + 1
                ))
            })?;
            if name == target.as_str() {
                selected.push(rule);
            }
        }
    }
    Ok(selected)
}

/// Select common and `@target` source files, then prepare this target's rules.
/// A missing resource directory selects an empty view.
pub fn prepare_project(root: &Path, target: &ResourceTarget) -> io::Result<Prepared> {
    prepare_project_with_limit(root, target, MEMORY_BYTES)
}

/// Prepare a directory only when the selected target has conversion rules.
/// Development callers use `None` to keep ordinary lazy directory resolution;
/// unconfigured targets never enumerate or stat the resource tree.
pub fn prepare_directory_if_configured(
    project_root: &Path,
    resources: &Path,
    target: &ResourceTarget,
) -> io::Result<Option<Prepared>> {
    let rules = rules(project_root, target)?;
    if rules.is_empty() {
        return Ok(None);
    }
    let selected = if resources.is_dir() {
        target.collect(resources)?
    } else {
        BTreeMap::new()
    };
    prepare_with_rules(
        project_root,
        target,
        selected,
        &std::env::current_exe()?,
        MEMORY_BYTES,
        rules,
    )
    .map(Some)
}

/// Prepare the project with an explicit workspace limit in bytes.
/// Existing immutable cache objects are reused regardless of this invocation's
/// limit because reusing a file performs no image or audio conversion.
pub fn prepare_project_with_limit(
    root: &Path,
    target: &ResourceTarget,
    memory_bytes: u64,
) -> io::Result<Prepared> {
    let resources = root.join("resources");
    let selected = if resources.is_dir() {
        target.collect(&resources)?
    } else {
        BTreeMap::new()
    };
    prepare_with_tool(
        root,
        target,
        selected,
        &std::env::current_exe()?,
        memory_bytes,
    )
}

/// Prepare an already selected logical resource map, including explicit mounts.
/// Mount selection and duplicate-name validation remain the caller's responsibility.
pub fn prepare_selected(
    root: &Path,
    target: &ResourceTarget,
    selected: BTreeMap<String, PathBuf>,
) -> io::Result<Prepared> {
    prepare_with_tool(
        root,
        target,
        selected,
        &std::env::current_exe()?,
        MEMORY_BYTES,
    )
}

fn hash_file(path: &Path) -> io::Result<blake3::Hash> {
    let mut file = File::open(path)?;
    let mut hash = blake3::Hasher::new();
    let mut scratch = [0; 64 << 10];
    loop {
        let count = file.read(&mut scratch)?;
        if count == 0 {
            break;
        }
        hash.update(&scratch[..count]);
    }
    Ok(hash.finalize())
}

fn prepare_with_tool(
    root: &Path,
    target: &ResourceTarget,
    selected: BTreeMap<String, PathBuf>,
    tool: &Path,
    memory_bytes: u64,
) -> io::Result<Prepared> {
    let rules = rules(root, target)?;
    prepare_with_rules(root, target, selected, tool, memory_bytes, rules)
}

fn prepare_with_rules(
    root: &Path,
    target: &ResourceTarget,
    selected: BTreeMap<String, PathBuf>,
    tool: &Path,
    memory_bytes: u64,
    rules: Vec<Rule>,
) -> io::Result<Prepared> {
    let root = root.canonicalize()?;
    let mut prepared = Prepared {
        files: BTreeMap::new(),
        report: Report::default(),
        index: None,
    };
    let mut tool_hash = None;
    for (name, source) in selected {
        let source = source.canonicalize()?;
        let source_bytes = fs::metadata(&source)?.len();
        prepared.report.source_bytes += source_bytes;
        let recipe = rules
            .iter()
            .find(|rule| rule.matcher.is_match(&name))
            .map(|rule| &rule.recipe);
        let path = match recipe {
            Some(recipe @ (Recipe::Texture { .. } | Recipe::Opus { .. })) => {
                let tool = match tool_hash {
                    Some(hash) => hash,
                    None => *tool_hash.insert(hash_file(tool)?),
                };
                let source_hash = hash_file(&source)?;
                let mut hash = blake3::Hasher::new();
                hash.update(b"Reel resource conversion v1\0");
                hash.update(tool.as_bytes());
                hash.update(source_hash.as_bytes());
                hash.update(&serde_json::to_vec(recipe).map_err(io::Error::other)?);
                let key = hash.finalize().to_hex().to_string();
                let objects = root.join("build/resources/objects");
                let object = objects.join(key);
                let output_name = if matches!(recipe, Recipe::Texture { .. }) {
                    "resource.reel-texture"
                } else {
                    "resource.opus"
                };
                let output = object.join(output_name);
                if output.is_file() {
                    prepared.report.reused += 1;
                } else {
                    fs::create_dir_all(&objects)?;
                    let staging = tempfile::Builder::new()
                        .prefix(".prepare-")
                        .tempdir_in(&objects)?;
                    let staged_output = staging.path().join(output_name);
                    let workspace = convert(&source, &staged_output, recipe, memory_bytes)
                        .map_err(|error| {
                            io::Error::other(format!(
                                "resource {name:?} ({}): {error}",
                                source.display()
                            ))
                        })?;
                    prepared.report.workspace_bytes =
                        prepared.report.workspace_bytes.max(workspace);
                    if hash_file(&source)? != source_hash {
                        return Err(io::Error::other(format!(
                            "resource changed during preparation: {}",
                            source.display()
                        )));
                    }
                    match fs::rename(staging.path(), &object) {
                        Ok(()) => {}
                        Err(_) if output.is_file() => {}
                        Err(error) => return Err(error),
                    }
                    prepared.report.converted += 1;
                }
                output
            }
            _ => {
                prepared.report.unchanged += 1;
                source
            }
        };
        prepared.report.output_bytes += fs::metadata(&path)?.len();
        prepared.files.insert(name, path);
    }
    if !rules.is_empty() {
        let directory = root.join("build/resources").join(target.as_str());
        fs::create_dir_all(&directory)?;
        let index = directory.join("index.json");
        let published = reel_storage::publish(&index, true, |file| {
            serde_json::to_writer(file, &prepared.files).map_err(io::Error::other)
        })?;
        if let Some(warning) = published.cleanup_warning {
            eprintln!("{warning}");
        }
        prepared.index = Some(index);
    }
    Ok(prepared)
}

fn convert(source: &Path, output: &Path, recipe: &Recipe, memory_bytes: u64) -> io::Result<u64> {
    match recipe {
        Recipe::Texture {
            scale,
            size,
            format,
            power_of_two,
        } => {
            let options = prepare_texture::Options {
                max_size: *size,
                power_of_two: *power_of_two,
                format: *format,
            };
            prepare_texture::prepare_scaled_bounded(source, output, options, *scale, memory_bytes)
                .map(|(_, workspace)| workspace)
                .map_err(io::Error::other)
        }
        Recipe::Opus { bitrate } => {
            let input = File::open(source)?;
            let length = input.metadata()?.len();
            let budget = BudgetPool::new(
                BudgetCategory::Temporary,
                QueueLimits {
                    max_requests: 64,
                    max_bytes: memory_bytes,
                },
            );
            let mut output = File::create(output)?;
            reel_media::encode::opus(
                input,
                length,
                &mut output,
                *bitrate,
                budget.clone(),
                &AtomicBool::new(false),
            )
            .map_err(io::Error::other)?;
            output.sync_all()?;
            Ok(budget.usage().peak_bytes)
        }
        Recipe::Copy => unreachable!("copy recipes retain their selected source"),
    }
}

#[cfg(test)]
mod tests;
