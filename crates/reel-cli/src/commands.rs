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
    /// Prepare a tiled image pyramid for visible-region loading.
    PrepareImage(PrepareImage),
    /// Scale artwork and prepare GPU texture storage while retaining author coordinates.
    PrepareTexture(PrepareTexture),
    /// Prepare this project's selected resource conversion rules.
    PrepareResources(PrepareResources),
    /// Remove generated project files from build/.
    Clean(Clean),
    /// Format every Aside source file in the current project's src directory.
    Fmt(Fmt),
}

#[derive(Args)]
pub struct Clean {
    /// Remove only the resource conversion cache; keep compiled and exported games.
    #[arg(long)]
    pub resources: bool,
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
    /// Destination directory; defaults to build/export/TARGET/debug (release with --release).
    #[arg(long, value_name = "DIRECTORY")]
    pub output: Option<PathBuf>,
    /// Package a resource directory under its logical prefix; use . for the resource root.
    #[arg(long = "resource", value_name = "PREFIX=DIRECTORY", value_parser = Mount::parse)]
    pub resources: Vec<Mount>,
    /// Package translation catalogs under their logical prefix; repeat as needed.
    #[arg(long, value_name = "PREFIX=DIRECTORY", value_parser = Mount::parse)]
    pub text: Vec<Mount>,
    /// Select @NAME resource variants and conversion rules.
    #[arg(long, value_name = "NAME", default_value = std::env::consts::OS, value_parser = resource_target)]
    pub resource_target: String,
    /// Runtime directory to include with its original structure instead of the local player.
    #[arg(long, value_name = "DIRECTORY", requires = "release")]
    pub runtime: Option<PathBuf>,
    /// Export code, text and resources for an already installed shared runtime.
    #[arg(long, requires = "release", conflicts_with = "runtime")]
    pub data_only: bool,
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
    /// Select @NAME resource variants for preview in the current platform player.
    #[arg(long, value_name = "NAME", default_value = std::env::consts::OS, value_parser = resource_target)]
    pub resource_target: String,
    /// Override the player's save directory.
    #[arg(long, value_name = "DIRECTORY")]
    pub save_directory: Option<PathBuf>,
}

fn resource_target(value: &str) -> Result<String, String> {
    reel_package::resource_target::ResourceTarget::parse(value)
        .map(|target| target.as_str().into())
        .map_err(|error| error.to_string())
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
    /// Compressed Reel textures by default; PNG preserves exact source samples.
    #[arg(long, value_enum, default_value = "reel-texture")]
    pub tile_encoding: reel_cli::prepare_image::Encoding,
}

#[derive(Args)]
pub struct PrepareTexture {
    #[arg(value_name = "IMAGE")]
    pub source: PathBuf,
    #[arg(long, value_name = "FILE.reel-texture")]
    pub output: PathBuf,
    /// Fit inside WIDTHxHEIGHT without enlarging the original image.
    #[arg(long, value_name = "WIDTHxHEIGHT", value_parser = texture_size)]
    pub size: Option<[u32; 2]>,
    /// Pad stored dimensions to powers of two, with a minimum of eight texels.
    #[arg(long)]
    pub power_of_two: bool,
    /// Generate all mip levels; enable only for artwork whose sampling may cross source regions.
    #[arg(long)]
    pub mipmaps: bool,
    /// Auto selects BC1 for opaque artwork and BC3 for transparent artwork.
    #[arg(long, value_enum, default_value = "auto")]
    pub format: reel_cli::prepare_texture::Format,
    /// Stable offline RGB quantization; ordered requires --format rgba8.
    #[arg(long, value_enum, default_value = "none")]
    pub dither: reel_cli::prepare_texture::Dither,
}

#[derive(Args)]
/// Project resource preparation and reporting options.
pub struct PrepareResources {
    /// Select the resource overlay and matching resources.toml target rules.
    #[arg(long, value_name = "NAME", default_value = std::env::consts::OS, value_parser = resource_target)]
    pub resource_target: String,
    /// Maximum image/audio conversion workspace; cached outputs need no decode.
    #[arg(long, value_name = "BYTES", default_value_t = std::num::NonZeroU64::new(reel_cli::resource_conversion::MEMORY_BYTES).unwrap())]
    pub memory_bytes: std::num::NonZeroU64,
    /// Render progress and results for a terminal or JSON consumer.
    #[arg(long, value_enum, default_value = "human")]
    pub format: Format,
}

fn texture_size(value: &str) -> Result<[u32; 2], String> {
    let (width, height) = value.split_once('x').ok_or("expected WIDTHxHEIGHT")?;
    let size = [width, height].map(|axis| axis.parse::<u32>().ok().filter(|n| *n > 0));
    match size {
        [Some(width), Some(height)] => Ok([width, height]),
        _ => Err("expected positive WIDTHxHEIGHT".into()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_targets_are_shared_by_development_run_and_export() {
        assert!(Cli::try_parse_from(["reel", "export", "--release", "--data-only"]).is_ok());
        assert!(Cli::try_parse_from(["reel", "export", "--data-only"]).is_err());
        assert!(
            Cli::try_parse_from([
                "reel",
                "export",
                "--release",
                "--data-only",
                "--runtime",
                "vita-runtime"
            ])
            .is_err()
        );
        for command in ["dev", "run", "export", "prepare-resources"] {
            assert!(Cli::try_parse_from(["reel", command, "--resource-target", "vita"]).is_ok());
            assert!(Cli::try_parse_from(["reel", command]).is_ok());
            for invalid in ["Vita", "../vita", "@vita", "vita/game", ""] {
                assert!(
                    Cli::try_parse_from(["reel", command, "--resource-target", invalid]).is_err()
                );
            }
        }
        let Command::Source(SourceCommand::Dev(dev)) =
            Cli::try_parse_from(["reel", "dev"]).unwrap().command
        else {
            panic!()
        };
        assert_eq!(dev.playback.resource_target, std::env::consts::OS);
    }
}
