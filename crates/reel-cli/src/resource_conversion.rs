//! Prepare selected source resources while preserving their author-facing names.
//!
//! An optional project-root `resources.toml` contains ordered `[[targets.NAME]]`
//! rules. The first matching logical path selects `convert = "texture"`, `"tiles"`,
//! `"atlas"`, `"webp"`, `"qoi"`, `"opus"`, `"video"`, `"font"`, or `"copy"`.
//! Unmatched resources retain their selected source file.
//! `match` is a case-sensitive glob with `/` separators (`*` stays within one
//! component; `**` crosses directories). Source target selection runs first.
//! Texture `scale` changes texel density while preserving logical coordinates;
//! `size = [width, height]` instead fits artwork inside an explicit box.
//! Texture rules with `format = "rgba8"` may select `dither = "ordered"` to spread
//! linear RGB rounding across a fixed 8x8 pattern; the default is `"none"`.
//! Texture rules may opt into `mipmaps = true`; the default is a single level.
//! Texture rules may keep the original bytes when BC fitting exceeds the display
//! error threshold by setting `fallback = "source"`.
//! Mips filter the padded linear-premultiplied storage and retain author coordinates.
//! Tiles own their separate level pyramid and do not accept this option.
//! Alpha remains undithered. Ordered dithering does not apply to block formats or tiles.
//! BC textures and tiles select color fitting against the filtered source's
//! displayed error while retaining the selected format and GPU byte count.
//! Each atlas rule forms one explicit shared-residency cohort. `page_size` defaults to
//! [1024, 1024]; atlas pages have one mip level and use four-texel edge gutters.
//! Atlas members retain their original logical paths and share one page.
//! Tile rules retain the logical image path and publish adjacent pyramid tiles.
//! `tile_size` defaults to 510 content pixels plus a one-pixel gutter per side;
//! `first_level = 1` starts at half density. `format` and `power_of_two` apply to
//! each tile. The manifest and tiles share one atomic cache object.
//! Video rules use `size` and `bitrate` to encode H.264/AAC with FFmpeg on PATH.
//! Opus rules import MP4/M4A/AAC through FFmpeg/FFprobe on PATH and bounded PCM;
//! ordinary audio formats use the built-in decoder and the same Opus encoder.
//! `alpha_layout = "top_bottom"` preserves packed color and alpha half-frames.
//! Font rules opt into subsetting with explicit `unicode` ranges and UTF-8 `text`
//! file globs. Include the characters accepted by player input as well as story text.
//!
//! Conversion runs one resource at a time, with BC blocks encoded in parallel
//! within its reserved image workspace. Immutable cache objects are keyed by
//! source content, normalized recipe and the converter implementation identity.
//! Payloads remain on disk; hashing uses a fixed 64 KiB buffer. A complete index
//! is atomically published only after all selected resources are ready.
mod atlas;
mod qoi;
mod webp;
use crate::prepare_texture;
use globset::{GlobBuilder, GlobMatcher};
use reel_package::resource_target::ResourceTarget;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io,
    path::{Path, PathBuf},
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
    /// External media codec allocations and thread stacks are not metered here.
    pub workspace_bytes: u64,
}

/// A complete selected resource view for development reads or packaging.
pub struct Prepared {
    /// Logical author paths mapped to immutable prepared files or selected sources.
    pub files: BTreeMap<String, PathBuf>,
    /// Actual work performed by this invocation.
    pub report: Report,
    /// Index published by explicit preparation or export for a configured target.
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
    mipmaps: Option<bool>,
    fallback: Option<TextureFallback>,
    power_of_two: Option<bool>,
    page_size: Option<[u32; 2]>,
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
    Webp,
    Qoi,
    Texture,
    Atlas,
    Tiles,
    Opus,
    Video,
    Font,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum TextureFallback {
    Source,
}

#[derive(Serialize)]
#[serde(tag = "convert", rename_all = "lowercase")]
enum Recipe {
    Copy,
    Webp,
    Qoi,
    Texture {
        scale: f64,
        size: Option<[u32; 2]>,
        format: prepare_texture::Format,
        power_of_two: bool,
        dither: prepare_texture::Dither,
        mipmaps: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        fallback: Option<TextureFallback>,
    },
    Atlas {
        scale: f64,
        format: prepare_texture::Format,
        dither: prepare_texture::Dither,
        page_size: [u32; 2],
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
        if self.mipmaps.is_some()
            && !matches!(self.convert, Convert::Texture)
            && (!matches!(self.convert, Convert::Atlas) || self.mipmaps == Some(true))
        {
            return Err(invalid(
                "atlas pages have one level; mipmaps belongs to texture rules",
            ));
        }
        if self.dither.is_some() && !matches!(self.convert, Convert::Texture | Convert::Atlas) {
            return Err(invalid("dither belongs to texture rules"));
        }
        if self.fallback.is_some() && !matches!(self.convert, Convert::Texture) {
            return Err(invalid("fallback belongs to texture rules"));
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
        if self.page_size.is_some() && !matches!(self.convert, Convert::Atlas) {
            return Err(invalid("page_size belongs to atlas rules"));
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
            Convert::Copy | Convert::Webp | Convert::Qoi => {
                if self.scale.is_some()
                    || self.size.is_some()
                    || self.format.is_some()
                    || self.power_of_two.is_some()
                    || self.bitrate.is_some()
                {
                    return Err(invalid(
                        "copy, webp and qoi rules only accept match and convert",
                    ));
                }
                match self.convert {
                    Convert::Webp => Recipe::Webp,
                    Convert::Qoi => Recipe::Qoi,
                    _ => Recipe::Copy,
                }
            }
            Convert::Atlas => {
                if self.size.is_some() || self.power_of_two.is_some() || self.bitrate.is_some() {
                    return Err(invalid(
                        "atlas rules accept scale, format, dither and page_size",
                    ));
                }
                let scale = self.scale.unwrap_or(1.);
                if !scale.is_finite() || scale <= 0. || scale > 1. {
                    return Err(invalid(
                        "atlas scale must be finite and greater than zero up to one",
                    ));
                }
                let page_size = self.page_size.unwrap_or([1024, 1024]);
                if page_size
                    .iter()
                    .any(|&n| !(8..=4096).contains(&n) || !n.is_multiple_of(4))
                {
                    return Err(invalid(
                        "atlas page dimensions must be multiples of four from 8 to 4096",
                    ));
                }
                let format = self.format.unwrap_or_default();
                let dither = self.dither.unwrap_or_default();
                prepare_texture::Options {
                    format,
                    dither,
                    ..Default::default()
                }
                .validate()
                .map_err(invalid)?;
                Recipe::Atlas {
                    scale,
                    format,
                    dither,
                    page_size,
                }
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
                    mipmaps: self.mipmaps.unwrap_or(false),
                    fallback: self.fallback,
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
            if let Some((hex, _)) = rest.split_once('}')
                && let Ok(value) = u32::from_str_radix(hex, 16)
                && char::from_u32(value).is_some()
            {
                characters.insert(value);
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
    let mut config: Configuration =
        toml::from_str(&text).map_err(|error| invalid(format!("{}: {error}", path.display())))?;
    for name in config.targets.keys() {
        ResourceTarget::parse(name)
            .map_err(|error| invalid(format!("{}: targets.{name}: {error}", path.display())))?;
    }
    let mut selected = Vec::new();
    for name in std::iter::once(target.as_str()).chain(target.fallback()) {
        let inputs = config.targets.remove(name).unwrap_or_default();
        for (index, input) in inputs.into_iter().enumerate() {
            let rule = input.compile(root).map_err(|error| {
                invalid(format!(
                    "{}: targets.{name}[{}]: {error}",
                    path.display(),
                    index + 1
                ))
            })?;
            if name == "vita" && matches!(rule.recipe, Recipe::Webp | Recipe::Qoi) {
                return Err(invalid(
                    "Vita image rules prepare textures, tiles or atlases instead of WebP or QOI",
                ));
            }
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
/// Playback consumes the selected files in memory and does not publish an index.
pub fn prepare_directory_if_configured(
    project_root: &Path,
    resources: &Path,
    target: &ResourceTarget,
) -> io::Result<Option<Prepared>> {
    let rules = rules(project_root, target)?;
    if rules.is_empty() {
        return Ok(None);
    }
    let selected = directory_files(target, resources)?;
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
    let selected = directory_files(target, &root.join("resources"))?;
    prepare_with_identity(root, target, selected, memory_bytes, converter_identity)
}

fn directory_files(target: &ResourceTarget, root: &Path) -> io::Result<BTreeMap<String, PathBuf>> {
    if root.is_dir() {
        // The resource walk admits regular files and directories. Resolve the
        // project root once so every selected file already has an absolute path.
        target.collect(&root.canonicalize()?)
    } else {
        Ok(BTreeMap::new())
    }
}

/// Prepare an already selected logical resource map, including explicit mounts.
/// Mount selection and duplicate-name validation remain the caller's responsibility.
pub fn prepare_selected(
    root: &Path,
    target: &ResourceTarget,
    mut selected: BTreeMap<String, PathBuf>,
) -> io::Result<Prepared> {
    for path in selected.values_mut() {
        *path = path.canonicalize()?;
    }
    prepare_with_identity(root, target, selected, MEMORY_BYTES, converter_identity)
}

fn hash_file(path: &Path) -> io::Result<blake3::Hash> {
    let mut file = File::open(path)?;
    let mut hash = blake3::Hasher::new();
    hash.update_reader(&mut file)?;
    Ok(hash.finalize())
}

fn converter_identity(recipe: &Recipe) -> &'static str {
    use crate::conversion_identity as identity;
    match recipe {
        Recipe::Copy => unreachable!(),
        Recipe::Webp => identity::for_kind("webp"),
        Recipe::Qoi => identity::for_kind("qoi"),
        Recipe::Texture { .. } => identity::for_kind("texture"),
        Recipe::Atlas { .. } => identity::for_kind("atlas"),
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
    let publish_index = !rules.is_empty();
    let mut prepared = prepare_with_rules(root, target, selected, memory_bytes, rules, identity)?;
    if publish_index {
        let directory = root
            .canonicalize()?
            .join("build/resources")
            .join(target.as_str());
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
    let mut audio_tools = None;
    let mut font_tools = None;
    let mut groups: BTreeMap<usize, Vec<(String, PathBuf)>> = BTreeMap::new();
    let mut ordinary = Vec::new();
    for (name, source) in selected {
        let selected_rule = rules.iter().position(|rule| rule.matcher.is_match(&name));
        if let Some(index) = selected_rule
            && matches!(rules[index].recipe, Recipe::Atlas { .. })
        {
            groups.entry(index).or_default().push((name, source));
        } else {
            ordinary.push((
                name,
                source,
                selected_rule.map(|index| &rules[index].recipe),
            ));
        }
    }
    for (index, members) in groups {
        atlas::prepare(
            &root,
            target,
            &rules[index].recipe,
            members,
            memory_bytes,
            identity(&rules[index].recipe),
            &mut prepared,
        )?;
    }
    for (name, source, recipe) in ordinary {
        let source_bytes = fs::metadata(&source)?.len();
        prepared.report.source_bytes += source_bytes;
        if matches!(recipe, None | Some(Recipe::Copy))
            && target.is_desktop()
            && source
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
        {
            let file = io::BufReader::new(File::open(&source)?);
            reel_media::video::validate_desktop_mp4(file, source_bytes).map_err(|error| {
                invalid(format!(
                    "resource {name:?} ({}): {error}. Use convert = \"video\" in [[targets.{}]] to prepare it",
                    source.display(), target.as_str()
                ))
            })?;
        }
        let path = match recipe {
            Some(
                recipe @ (Recipe::Texture { .. }
                | Recipe::Webp
                | Recipe::Qoi
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
                if matches!(recipe, Recipe::Opus { .. })
                    && crate::prepare_audio::uses_external(&source)
                {
                    if audio_tools.is_none() {
                        audio_tools = Some(crate::prepare_audio::external_tools_identity()?);
                    }
                    hash.update(audio_tools.unwrap().as_bytes());
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
                    Recipe::Webp => "resource.webp",
                    Recipe::Qoi => "resource.qoi",
                    Recipe::Texture { .. } => "resource.reel-texture",
                    Recipe::Tiles { .. } => "resource.reel-image",
                    Recipe::Opus { .. } => "resource.opus",
                    Recipe::Video { .. } => "resource.mp4",
                    Recipe::Font { .. } => "resource.ttf",
                    Recipe::Atlas { .. } | Recipe::Copy => unreachable!(),
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
                    let workspace = convert(
                        &source,
                        &staged_output,
                        recipe,
                        memory_bytes,
                        target.as_str() == "vita",
                    )
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
                prepared.insert(name, source, source_bytes)?;
                continue;
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
    Ok(prepared)
}

fn insert_prepared(prepared: &mut Prepared, name: String, path: PathBuf) -> io::Result<()> {
    let bytes = fs::metadata(&path)?.len();
    prepared.insert(name, path, bytes)
}

impl Prepared {
    fn insert(&mut self, name: String, path: PathBuf, bytes: u64) -> io::Result<()> {
        use std::collections::btree_map::Entry;
        match self.files.entry(name) {
            Entry::Occupied(entry) => Err(invalid(format!(
                "generated resource conflicts with {:?}",
                entry.key()
            ))),
            Entry::Vacant(entry) => {
                self.report.output_bytes += bytes;
                entry.insert(path);
                Ok(())
            }
        }
    }
}

fn convert(
    source: &Path,
    output: &Path,
    recipe: &Recipe,
    memory_bytes: u64,
    gxm_layout: bool,
) -> io::Result<u64> {
    match recipe {
        Recipe::Webp => webp::prepare(source, output, memory_bytes),
        Recipe::Qoi => qoi::prepare(source, output, memory_bytes),
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
                gxm_layout,
                format: *format,
                power_of_two: *power_of_two,
                dither: prepare_texture::Dither::None,
                mipmaps: false,
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
            mipmaps,
            fallback,
        } => {
            let options = prepare_texture::Options {
                max_size: *size,
                gxm_layout,
                power_of_two: *power_of_two,
                format: *format,
                dither: *dither,
                mipmaps: *mipmaps,
            };
            prepare_texture::prepare_scaled_bounded(
                source,
                output,
                options,
                *scale,
                memory_bytes,
                fallback.is_some(),
            )
            .map(|(_, workspace)| workspace)
            .map_err(io::Error::other)
        }
        Recipe::Opus { bitrate } => {
            crate::prepare_audio::prepare(source, output, *bitrate, memory_bytes)
        }
        Recipe::Atlas { .. } | Recipe::Copy => {
            unreachable!("atlas groups and copy recipes are handled separately")
        }
    }
}

#[cfg(test)]
mod tests;
