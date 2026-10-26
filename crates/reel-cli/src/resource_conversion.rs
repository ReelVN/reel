//! Prepare selected source resources while preserving their author-facing names.
//!
//! An optional project-root `resources.toml` contains ordered `[[targets.NAME]]`
//! rules. The first matching logical path selects `convert = "texture"`, `"tiles"`,
//! `"opus"`, `"video"`, or `"copy"`; unmatched resources retain their selected source file.
//! `match` is a case-sensitive glob with `/` separators (`*` stays within one
//! component; `**` crosses directories). Source target selection runs first.
//! Texture `scale` changes texel density while preserving logical coordinates;
//! `size = [width, height]` instead fits artwork inside an explicit box.
//! Texture rules with `format = "rgba8"` may select `dither = "ordered"` to spread
//! linear RGB rounding across a fixed 8x8 pattern; the default is `"none"`.
//! Alpha remains undithered. Ordered dithering does not apply to block formats or tiles.
//! BC textures and tiles select color fitting against the filtered source's
//! displayed error while retaining the selected format and GPU byte count.
//! Tile rules retain the logical image path and publish adjacent pyramid tiles.
//! `tile_size` defaults to 510 content pixels plus a one-pixel gutter per side;
//! `first_level = 1` starts at half density. `format` and `power_of_two` apply to
//! each tile. The manifest and tiles share one atomic cache object.
//! Video rules use `size` and `bitrate` to encode H.264/AAC with FFmpeg on PATH.
//! `alpha_layout = "top_bottom"` preserves packed color and alpha half-frames.
//! Font rules opt into subsetting with explicit `unicode` ranges and UTF-8 `text`
//! file globs. Include the characters accepted by player input as well as story text.
//!
//! Conversion runs one resource at a time. Immutable cache objects are keyed by
//! source content, normalized recipe and the converter implementation identity.
//! Payloads remain on disk; hashing uses a fixed 64 KiB buffer. A complete index
//! is atomically published only after all selected resources are ready.
use crate::prepare_texture;
use globset::{GlobBuilder, GlobMatcher};
use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
use reel_package::resource_target::ResourceTarget;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
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
    /// Peak image reservation, audio budget use or video workspace estimate.
    /// External video codec allocations and thread stacks are not metered here.
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
    dither: Option<prepare_texture::Dither>,
    power_of_two: Option<bool>,
    bitrate: Option<u32>,
    tile_size: Option<u32>,
    first_level: Option<u32>,
    alpha_layout: Option<crate::prepare_video::AlphaLayout>,
    unicode: Option<Vec<String>>,
    text: Option<Vec<String>>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Convert {
    Copy,
    Texture,
    Tiles,
    Opus,
    Video,
    Font,
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
        dither: prepare_texture::Dither,
    },
    Tiles {
        tile_size: u32,
        first_level: u32,
        format: prepare_texture::Format,
        power_of_two: bool,
    },
    Opus {
        bitrate: u32,
    },
    Video {
        size: [u32; 2],
        bitrate: u32,
        alpha_layout: crate::prepare_video::AlphaLayout,
    },
    Font {
        characters: BTreeSet<u32>,
    },
}

struct Rule {
    matcher: GlobMatcher,
    recipe: Recipe,
}

impl RuleInput {
    fn compile(self, root: &Path) -> io::Result<Rule> {
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
        if self.dither.is_some() && !matches!(self.convert, Convert::Texture) {
            return Err(invalid("dither belongs to texture rules"));
        }
        if !matches!(self.convert, Convert::Tiles)
            && (self.tile_size.is_some() || self.first_level.is_some())
        {
            return Err(invalid("tile_size and first_level belong to tiles rules"));
        }
        if !matches!(self.convert, Convert::Video) && self.alpha_layout.is_some() {
            return Err(invalid("alpha_layout belongs to video rules"));
        }
        if !matches!(self.convert, Convert::Font) && (self.unicode.is_some() || self.text.is_some())
        {
            return Err(invalid("unicode and text belong to font rules"));
        }
        let recipe = match self.convert {
            Convert::Font => {
                if self.scale.is_some()
                    || self.size.is_some()
                    || self.format.is_some()
                    || self.power_of_two.is_some()
                    || self.bitrate.is_some()
                {
                    return Err(invalid(
                        "font rules accept unicode ranges and text file globs",
                    ));
                }
                Recipe::Font {
                    characters: font_characters(
                        root,
                        self.unicode.unwrap_or_default(),
                        self.text.unwrap_or_default(),
                    )?,
                }
            }
            Convert::Video => {
                if self.scale.is_some() || self.format.is_some() || self.power_of_two.is_some() {
                    return Err(invalid("video rules accept size, bitrate and alpha_layout"));
                }
                let size = self
                    .size
                    .ok_or_else(|| invalid("video rules require size"))?;
                let bitrate = self.bitrate.unwrap_or(4_000_000);
                if size.iter().any(|&value| value < 2) || !(16_000..=100_000_000).contains(&bitrate)
                {
                    return Err(invalid(
                        "video size dimensions must be at least two and bitrate 16000..100000000",
                    ));
                }
                Recipe::Video {
                    size,
                    bitrate,
                    alpha_layout: self.alpha_layout.unwrap_or_default(),
                }
            }
            Convert::Tiles => {
                if self.scale.is_some() || self.size.is_some() || self.bitrate.is_some() {
                    return Err(invalid(
                        "tiles rules use first_level for density and tile_size for each tile",
                    ));
                }
                let tile_size = self.tile_size.unwrap_or(510);
                let first_level = self.first_level.unwrap_or(0);
                if tile_size == 0 || first_level > 31 {
                    return Err(invalid(
                        "tile_size must be positive and first_level must be between 0 and 31",
                    ));
                }
                Recipe::Tiles {
                    tile_size,
                    first_level,
                    format: self.format.unwrap_or_default(),
                    power_of_two: self.power_of_two.unwrap_or(false),
                }
            }
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
                let format = self.format.unwrap_or_default();
                let dither = self.dither.unwrap_or_default();
                prepare_texture::Options {
                    format,
                    dither,
                    ..Default::default()
                }
                .validate()
                .map_err(invalid)?;
                if self.bitrate.is_some() {
                    return Err(invalid("bitrate belongs to opus or video rules"));
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
                    format,
                    dither,
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

fn font_characters(
    root: &Path,
    ranges: Vec<String>,
    patterns: Vec<String>,
) -> io::Result<BTreeSet<u32>> {
    let mut characters = BTreeSet::new();
    let scalar = |text: &str| -> io::Result<u32> {
        u32::from_str_radix(text.strip_prefix("U+").unwrap_or(text), 16)
            .ok()
            .filter(|&value| value <= 0x10ffff)
            .ok_or_else(|| invalid(format!("invalid Unicode scalar: {text}")))
    };
    for range in ranges {
        let (start, end) = range.split_once('-').unwrap_or((&range, &range));
        let (start, end) = (scalar(start)?, scalar(end)?);
        if start > end {
            return Err(invalid(format!("reversed Unicode range: {range}")));
        }
        characters.extend((start..=end).filter(|&value| char::from_u32(value).is_some()));
    }
    let mut files = BTreeSet::new();
    for pattern in patterns {
        let path = Path::new(&pattern);
        if pattern.is_empty()
            || pattern.contains('\\')
            || path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(invalid(
                "font text globs must be relative paths using '/' separators",
            ));
        }
        let matcher = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(false)
            .build()
            .map_err(invalid)?
            .compile_matcher();
        let mut base = root.to_path_buf();
        for part in pattern.split('/') {
            if part.contains(['*', '?', '[', '{']) {
                break;
            }
            base.push(part);
        }
        let mut pending = vec![base];
        let mut matched = false;
        while let Some(path) = pending.pop() {
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                for entry in fs::read_dir(path)? {
                    pending.push(entry?.path());
                }
            } else if metadata.is_file() && matcher.is_match(path.strip_prefix(root).unwrap()) {
                files.insert(path);
                matched = true;
            }
        }
        if !matched {
            return Err(invalid(format!(
                "font text glob matched no files: {pattern}"
            )));
        }
    }
    for file in files {
        let text = fs::read_to_string(&file)?;
        characters.extend(text.chars().map(u32::from));
        for rest in text.split("\\u{").skip(1) {
            if let Some((hex, _)) = rest.split_once('}') {
                if let Ok(value) = u32::from_str_radix(hex, 16) {
                    if char::from_u32(value).is_some() {
                        characters.insert(value);
                    }
                }
            }
        }
    }
    if characters.is_empty() {
        return Err(invalid(
            "font rules require nonempty unicode ranges or text files",
        ));
    }
    Ok(characters)
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
            if name != target.as_str() {
                continue;
            }
            let rule = input.compile(root).map_err(|error| {
                invalid(format!(
                    "{}: targets.{name}[{}]: {error}",
                    path.display(),
                    index + 1
                ))
            })?;
            selected.push(rule);
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
        MEMORY_BYTES,
        rules,
        converter_identity,
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
    prepare_with_identity(root, target, selected, memory_bytes, converter_identity)
}

/// Prepare an already selected logical resource map, including explicit mounts.
/// Mount selection and duplicate-name validation remain the caller's responsibility.
pub fn prepare_selected(
    root: &Path,
    target: &ResourceTarget,
    selected: BTreeMap<String, PathBuf>,
) -> io::Result<Prepared> {
    prepare_with_identity(root, target, selected, MEMORY_BYTES, converter_identity)
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

fn converter_identity(recipe: &Recipe) -> &'static str {
    use crate::conversion_identity as identity;
    match recipe {
        Recipe::Copy => unreachable!(),
        Recipe::Texture { .. } => identity::for_kind("texture"),
        Recipe::Tiles { .. } => identity::for_kind("tiles"),
        Recipe::Opus { .. } => identity::for_kind("opus"),
        Recipe::Video { .. } => identity::for_kind("video"),
        Recipe::Font { .. } => identity::for_kind("font"),
    }
}

fn prepare_with_identity<'a>(
    root: &Path,
    target: &ResourceTarget,
    selected: BTreeMap<String, PathBuf>,
    memory_bytes: u64,
    identity: impl Fn(&Recipe) -> &'a str,
) -> io::Result<Prepared> {
    let rules = rules(root, target)?;
    prepare_with_rules(root, target, selected, memory_bytes, rules, identity)
}

fn prepare_with_rules<'a>(
    root: &Path,
    target: &ResourceTarget,
    selected: BTreeMap<String, PathBuf>,
    memory_bytes: u64,
    rules: Vec<Rule>,
    identity: impl Fn(&Recipe) -> &'a str,
) -> io::Result<Prepared> {
    let root = root.canonicalize()?;
    let mut prepared = Prepared {
        files: BTreeMap::new(),
        report: Report::default(),
        index: None,
    };
    let mut video_tools = None;
    let mut font_tools = None;
    for (name, source) in selected {
        let source = source.canonicalize()?;
        let source_bytes = fs::metadata(&source)?.len();
        prepared.report.source_bytes += source_bytes;
        let recipe = rules
            .iter()
            .find(|rule| rule.matcher.is_match(&name))
            .map(|rule| &rule.recipe);
        let path = match recipe {
            Some(
                recipe @ (Recipe::Texture { .. }
                | Recipe::Tiles { .. }
                | Recipe::Opus { .. }
                | Recipe::Video { .. }
                | Recipe::Font { .. }),
            ) => {
                let source_hash = hash_file(&source)?;
                let mut hash = blake3::Hasher::new();
                hash.update(b"Reel resource conversion v1\0");
                hash.update(identity(recipe).as_bytes());
                hash.update(source_hash.as_bytes());
                hash.update(&serde_json::to_vec(recipe).map_err(io::Error::other)?);
                if matches!(recipe, Recipe::Video { .. }) {
                    if video_tools.is_none() {
                        video_tools = Some(crate::prepare_video::external_tools_identity()?);
                    }
                    hash.update(video_tools.as_ref().unwrap().as_bytes());
                }
                if matches!(recipe, Recipe::Font { .. }) {
                    if font_tools.is_none() {
                        font_tools = Some(crate::prepare_font::external_tools_identity()?);
                    }
                    hash.update(font_tools.as_ref().unwrap().as_bytes());
                }
                let key = hash.finalize().to_hex().to_string();
                let objects = root.join("build/resources/objects");
                let object = objects.join(key);
                let output_name = match recipe {
                    Recipe::Texture { .. } => "resource.reel-texture",
                    Recipe::Tiles { .. } => "resource.reel-image",
                    Recipe::Opus { .. } => "resource.opus",
                    Recipe::Video { .. } => "resource.mp4",
                    Recipe::Font { .. } => "resource.ttf",
                    Recipe::Copy => unreachable!(),
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
        if matches!(recipe, Some(Recipe::Tiles { .. })) {
            let manifest = reel_core::prepared_image::PreparedImage::decode(&fs::read(&path)?)
                .map_err(invalid)?;
            let filename = path.file_name().and_then(|name| name.to_str()).unwrap();
            for level in manifest.first_level..manifest.levels() {
                let [width, height] = manifest.dimensions(level);
                for y in 0..height.div_ceil(manifest.tile_edge) {
                    for x in 0..width.div_ceil(manifest.tile_edge) {
                        insert_prepared(
                            &mut prepared,
                            manifest.tile_source(&name, level, x, y),
                            path.parent()
                                .unwrap()
                                .join(manifest.tile_source(filename, level, x, y)),
                        )?;
                    }
                }
            }
        }
        insert_prepared(&mut prepared, name, path)?;
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

fn insert_prepared(prepared: &mut Prepared, name: String, path: PathBuf) -> io::Result<()> {
    use std::collections::btree_map::Entry;
    match prepared.files.entry(name) {
        Entry::Occupied(entry) => Err(invalid(format!(
            "generated resource conflicts with {:?}",
            entry.key()
        ))),
        Entry::Vacant(entry) => {
            prepared.report.output_bytes += fs::metadata(&path)?.len();
            entry.insert(path);
            Ok(())
        }
    }
}

fn convert(source: &Path, output: &Path, recipe: &Recipe, memory_bytes: u64) -> io::Result<u64> {
    match recipe {
        Recipe::Font { characters } => {
            crate::prepare_font::prepare(source, output, characters, memory_bytes)
        }
        Recipe::Video {
            size,
            bitrate,
            alpha_layout,
        } => crate::prepare_video::prepare(
            source,
            output,
            crate::prepare_video::Options {
                size: *size,
                bitrate: *bitrate,
                alpha_layout: *alpha_layout,
            },
            memory_bytes,
        ),
        Recipe::Tiles {
            tile_size,
            first_level,
            format,
            power_of_two,
        } => crate::prepare_image::prepare_bounded(
            source,
            output,
            *tile_size,
            crate::prepare_image::Encoding::ReelTexture,
            *first_level,
            prepare_texture::Options {
                max_size: None,
                format: *format,
                power_of_two: *power_of_two,
                dither: prepare_texture::Dither::None,
            },
            memory_bytes,
        )
        .map(|(_, workspace)| workspace)
        .map_err(io::Error::other),
        Recipe::Texture {
            scale,
            size,
            format,
            power_of_two,
            dither,
        } => {
            let options = prepare_texture::Options {
                max_size: *size,
                power_of_two: *power_of_two,
                format: *format,
                dither: *dither,
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
