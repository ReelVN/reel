//! One command tree owns parsing, validation and generated help.
use clap::{Args, Parser, Subcommand, ValueEnum};
use reel_cli::{BuildProfile, distribution::Mount};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::PathBuf,
};

#[derive(Parser)]
#[command(
    name = "reel",
    version,
    about = "Check, build, export and play Aside stories"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    #[command(flatten)]
    Source(SourceCommand),
    /// Play an ASB, including its embedded distribution, without recompiling.
    Run(Run),
    /// Extract, bundle and check translation catalogs.
    #[command(subcommand)]
    Loc(Localization),
    /// Prepare a tiled image pyramid for visible-region loading.
    PrepareImage(PrepareImage),
}

#[derive(Subcommand)]
pub enum SourceCommand {
    /// Compile and diagnose source without running the story.
    Check(Check),
    /// Build a movable ASB. Debug includes source; Release strips debug data.
    Build(Build),
    /// Export a complete project; Release packages code, text and resources.
    Export(Export),
    /// Compile and play source or an exported development ASB.
    Dev(Dev),
}
impl SourceCommand {
    pub fn project(&self) -> &Project {
        match self {
            Self::Check(v) => &v.project,
            Self::Build(v) => &v.project,
            Self::Export(v) => &v.project,
            Self::Dev(v) => &v.project,
        }
    }
    pub fn format(&self) -> Format {
        match self {
            Self::Check(v) => v.format,
            Self::Build(v) => v.format,
            Self::Export(v) => v.format,
            Self::Dev(_) => Format::Human,
        }
    }
    pub fn entry(&self) -> Option<&str> {
        match self {
            Self::Check(_) => None,
            Self::Build(v) => v.settings.entry.as_deref(),
            Self::Export(v) => v.settings.entry.as_deref(),
            Self::Dev(v) => v.entry.as_deref(),
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum Format {
    #[default]
    Human,
    Json,
}
impl Format {
    pub fn json(self) -> bool {
        self == Self::Json
    }
}

#[derive(Args)]
pub struct Project {
    /// Project directory or source file; defaults to the current project (dev also accepts an exported ASB).
    #[arg(value_name = "SOURCE", default_value = ".")]
    pub source: PathBuf,
    /// Add an external source root; repeat for multiple aliases.
    #[arg(long = "module", value_name = "ALIAS=DIRECTORY", value_parser = module_root)]
    pub modules: Vec<ModuleRoot>,
}
impl Project {
    pub fn module_roots(&self) -> Result<BTreeMap<String, PathBuf>, String> {
        let mut modules = BTreeMap::new();
        for module in &self.modules {
            if modules
                .insert(module.alias.clone(), module.directory.clone())
                .is_some()
            {
                return Err(format!("module alias {} was specified twice", module.alias));
            }
        }
        Ok(modules)
    }
}
#[derive(Clone)]
pub struct ModuleRoot {
    alias: String,
    directory: PathBuf,
}
fn module_root(value: &str) -> Result<ModuleRoot, String> {
    let (alias, directory) = value
        .split_once('=')
        .filter(|(a, p)| !a.is_empty() && !p.is_empty())
        .ok_or("expected nonempty alias=directory")?;
    Ok(ModuleRoot {
        alias: alias.into(),
        directory: directory.into(),
    })
}
#[derive(Args)]
pub struct Check {
    #[command(flatten)]
    pub project: Project,
    #[arg(long, value_enum, default_value = "human")]
    pub format: Format,
}
#[derive(Args)]
pub struct BuildSettings {
    /// Entry function; defaults to main::main.
    #[arg(long, value_name = "FUNCTION")]
    pub entry: Option<String>,
    /// Omit source and debug mappings from the compiled program.
    #[arg(long)]
    pub release: bool,
    /// Replace an existing --output destination after successful compilation.
    #[arg(long)]
    pub overwrite: bool,
}
impl BuildSettings {
    pub fn profile(&self) -> BuildProfile {
        if self.release {
            BuildProfile::Release
        } else {
            BuildProfile::Debug
        }
    }
}
#[derive(Args)]
pub struct Build {
    #[command(flatten)]
    pub project: Project,
    #[command(flatten)]
    pub settings: BuildSettings,
    /// Destination ASB file; defaults to build/debug/game.asb (build/release with --release).
    #[arg(long, value_name = "FILE.asb", value_parser = asb_output)]
    pub output: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "human")]
    pub format: Format,
}
fn asb_output(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path.extension().is_none_or(|extension| extension != "asb") {
        Err("expected a UTF-8 .asb output path".into())
    } else {
        Ok(path)
    }
}
#[derive(Args)]
pub struct Export {
    #[command(flatten)]
    pub project: Project,
    #[command(flatten)]
    pub settings: BuildSettings,
    /// Destination directory; defaults to build/export/debug (release with --release).
    #[arg(long, value_name = "DIRECTORY")]
    pub output: Option<PathBuf>,
    /// Package a resource directory under its logical prefix; use . for the resource root.
    #[arg(long = "resource", value_name = "PREFIX=DIRECTORY", value_parser = Mount::parse)]
    pub resources: Vec<Mount>,
    /// Package translation catalogs under their logical prefix; repeat as needed.
    #[arg(long, value_name = "PREFIX=DIRECTORY", value_parser = Mount::parse)]
    pub text: Vec<Mount>,
    /// Target resource volume size including index; defaults to 2147483648 (2 GiB).
    #[arg(long, value_name = "BYTES", requires = "release")]
    pub pack_bytes: Option<std::num::NonZeroU64>,
    #[arg(long, value_enum, default_value = "human")]
    pub format: Format,
}
#[derive(Args)]
pub struct Playback {
    /// Asset directory; for exported ASB this explicitly enables loose-file overrides.
    #[arg(long, value_name = "DIRECTORY")]
    pub assets: Option<PathBuf>,
    /// Override the player's save directory.
    #[arg(long, value_name = "DIRECTORY")]
    pub save_directory: Option<PathBuf>,
}
#[derive(Args)]
#[command(
    after_help = "Playback requires the complete Reel tool package, including its platform player."
)]
pub struct Dev {
    #[command(flatten)]
    pub project: Project,
    /// Entry function; defaults to main::main or the embedded development entry.
    #[arg(long, value_name = "FUNCTION")]
    pub entry: Option<String>,
    #[command(flatten)]
    pub playback: Playback,
}
#[derive(Args)]
#[command(
    after_help = "Playback requires the complete Reel tool package, including its platform player."
)]
pub struct Run {
    /// ASB path; defaults to the current project's build/debug/game.asb.
    #[arg(value_name = "ASB")]
    pub source: Option<PathBuf>,
    /// Run the current project's build/release/game.asb instead of build/debug/game.asb.
    #[arg(long, conflicts_with = "source")]
    pub release: bool,
    #[command(flatten)]
    pub playback: Playback,
}

#[derive(Subcommand)]
pub enum Localization {
    /// Extract messages from the imported module graph.
    Extract(Extract),
    /// Compile one language's module catalogs into a runtime FTL.
    Bundle(Bundle),
    /// Check Fluent syntax and language-tag validity.
    Check(CatalogCheck),
}
#[derive(Args)]
pub struct Extract {
    #[arg(value_name = "SOURCE")]
    pub source: PathBuf,
    #[command(flatten)]
    pub destination: ExtractionOutput,
    #[arg(long)]
    pub overwrite: bool,
}
#[derive(Args)]
#[group(required = true, multiple = false)]
pub struct ExtractionOutput {
    /// Write a single source catalog.
    #[arg(long, value_name = "SOURCE.ftl")]
    pub output: Option<PathBuf>,
    /// Mirror module paths in a directory of source catalogs.
    #[arg(long, value_name = "DIRECTORY")]
    pub output_dir: Option<PathBuf>,
}
#[derive(Args)]
pub struct Bundle {
    #[arg(value_name = "LOCALE_DIRECTORY")]
    pub source: PathBuf,
    #[arg(long, value_name = "CATALOG.ftl")]
    pub output: PathBuf,
    #[arg(long)]
    pub overwrite: bool,
}
#[derive(Args)]
pub struct CatalogCheck {
    #[arg(value_name = "CATALOG.ftl")]
    pub source: PathBuf,
    #[arg(long, value_name = "LANGUAGE_TAG")]
    pub locale: String,
}
#[derive(Args)]
pub struct PrepareImage {
    #[arg(value_name = "IMAGE")]
    pub source: PathBuf,
    #[arg(long, value_name = "FILE.reel-image")]
    pub output: PathBuf,
    /// Interior tile edge in pixels, excluding the two-pixel border.
    #[arg(long, default_value_t = 512, value_parser = clap::value_parser!(u32).range(1..=i64::from(u32::MAX - 2)))]
    pub tile_size: u32,
}

/// Invalid commands still honor machine-readable diagnostics. This only selects
/// their renderer; clap remains the sole parser/validator of command arguments.
pub fn requested_json(raw: &[OsString]) -> bool {
    let mut previous = None;
    raw.iter().take_while(|arg| *arg != "--").any(|arg| {
        let requested =
            arg == "--format=json" || (previous == Some(OsStr::new("--format")) && arg == "json");
        previous = Some(arg.as_os_str());
        requested
    })
}
