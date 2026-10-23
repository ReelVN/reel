//! One command tree owns parsing, validation and generated help.
use clap::{Args, Parser, Subcommand, ValueEnum};
use reel_cli::{BuildProfile, distribution::Mount};
use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
};

#[derive(Parser)]
#[command(
    name = "reel",
    version,
    about = "Check, test, build, export and play Aside stories"
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
    /// Prepare a tiled image pyramid for visible-region loading.
    PrepareImage(PrepareImage),
    /// Format every Aside source file in the current project's src directory.
    Fmt(Fmt),
    /// Explore the current game through the real headless source and ASB runtime.
    Test(Test),
}

#[derive(Args)]
pub struct Test {
    /// Replay a previously recorded test input trace.
    #[arg(long, value_name = "FILE")]
    pub replay: Option<PathBuf>,
    /// Override the project's resources directory.
    #[arg(long, value_name = "DIRECTORY")]
    pub assets: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "human")]
    pub format: Format,
}

#[derive(Args)]
pub struct Fmt {
    /// Report formatting differences without writing files.
    #[arg(long)]
    pub check: bool,
}

#[derive(Subcommand)]
pub enum SourceCommand {
    /// Compile and diagnose source without running the story.
    Check(Check),
    /// Build a movable ASB. Debug includes source; Release strips debug data.
    Build(Build),
    /// Export a complete project; Release packages code, text and resources.
    Export(Export),
    /// Compile and play the current project.
    Dev(Dev),
}
impl SourceCommand {
    pub fn format(&self) -> Format {
        match self {
            Self::Check(v) => v.format,
            Self::Build(v) => v.format,
            Self::Export(v) => v.format,
            Self::Dev(_) => Format::Human,
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
pub struct Check {
    #[arg(long, value_enum, default_value = "human")]
    pub format: Format,
}
#[derive(Args)]
pub struct BuildSettings {
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
