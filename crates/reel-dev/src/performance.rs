//! Reproducible performance runs through the distribution player.
use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use reel_player::performance::{CachePolicy, FrameCaptureOptions, Request};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

mod collect;
mod emulator;
mod prepare;
mod run;
mod summary;

/// Build, run and analyze the ordinary player.
#[derive(Args)]
pub struct Options {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Build the current exporter and prepare a game and measurement request.
    Prepare(Prepare),
    /// Build the official Vita player in a configured Linux/VitaSDK shell.
    Runtime {
        #[arg(long)]
        output: PathBuf,
        /// Extra cargo-vita arguments, such as --config paths=[...].
        #[arg(last = true)]
        cargo_args: Vec<String>,
    },
    /// Run a prepared game with the distribution player; collect its reports.
    Run(Run),
    /// Retrieve an interrupted Vita or emulator run without starting another game.
    Collect(Device),
    /// Analyze files already collected in a run directory.
    Report { directory: PathBuf },
}

#[derive(Args)]
struct Prepare {
    project: PathBuf,
    output: PathBuf,
    #[arg(long, default_value = std::env::consts::OS)]
    target: String,
    /// Label emulator measurements separately from hardware measurements.
    #[arg(long, value_parser = ["host", "vita", "emulator"])]
    environment: Option<String>,
    #[arg(long)]
    vm_timing: bool,
    /// Exact game title at which to retain an accepted frame (Vita).
    #[arg(long)]
    frame: Option<String>,
    #[arg(long, default_value_t = 3)]
    accepted_frame: u32,
    #[arg(long, default_value_t = 3)]
    repetitions: u32,
    #[arg(long, default_value = "replay", value_parser = ["replay", "reuse"])]
    cache: String,
    #[arg(long)]
    serial: bool,
    /// Save the captured drawing and resources for offline analysis.
    #[arg(long, requires = "frame")]
    save_frame: bool,
    #[arg(long, value_parser = ["on", "off"])]
    source_atlas: Option<String>,
}

#[derive(Args)]
struct Run {
    directory: PathBuf,
    /// Vita vdb address; omit for a desktop game.
    #[arg(long)]
    address: Option<String>,
    #[arg(long, default_value = "vdb")]
    vdb: PathBuf,
    /// Unpacked official player produced by `perf runtime`.
    #[arg(long)]
    runtime: Option<PathBuf>,
    /// Vita3K executable for an emulator run.
    #[arg(long, requires = "storage", conflicts_with = "address")]
    emulator: Option<PathBuf>,
    /// Vita3K storage directory containing ux0.
    #[arg(long, requires = "emulator")]
    storage: Option<PathBuf>,
    /// Base Vita3K configuration; defaults to config.yml beside its executable.
    #[arg(long, requires = "emulator")]
    emulator_config: Option<PathBuf>,
    #[arg(long, default_value_t = 240)]
    timeout_seconds: u64,
}

#[derive(Args)]
struct Device {
    directory: PathBuf,
    #[arg(long, conflicts_with = "storage")]
    address: Option<String>,
    /// Vita3K storage containing a completed emulator run.
    #[arg(long, conflicts_with = "address")]
    storage: Option<PathBuf>,
    #[arg(long, default_value = "vdb")]
    vdb: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    schema: u32,
    target: String,
    environment: String,
    project: PathBuf,
    exporter: PathBuf,
    request: Request,
}

impl Manifest {
    fn read(directory: &Path) -> Result<Self> {
        let value: Self = serde_json::from_slice(&fs::read(directory.join("run.json"))?)?;
        ensure!(value.schema == 1, "unsupported performance run version");
        value
            .request
            .validate()
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        Ok(value)
    }
}

/// Execute a parsed performance workflow.
pub fn execute(options: Options) -> Result<()> {
    match options.command {
        Action::Prepare(options) => prepare::game(options),
        Action::Runtime { output, cargo_args } => prepare::runtime(&output, &cargo_args),
        Action::Run(options) => run::execute(options),
        Action::Collect(options) => collect::device(&options),
        Action::Report { directory } => collect::report(&directory),
    }
}

fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn checked(command: &mut Command, log: &Path, timeout: Duration) -> Result<Vec<u8>> {
    let result = crate::process::run(command, timeout, None)?;
    let mut output = result.stdout.clone();
    output.extend_from_slice(&result.stderr);
    fs::write(log, output)?;
    ensure!(!result.timed_out, "command timed out; {}", log.display());
    ensure!(result.status.success(), "command failed; {}", log.display());
    Ok(result.stdout)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in walkdir::WalkDir::new(source).min_depth(1) {
        let entry = entry?;
        let target = destination.join(entry.path().strip_prefix(source)?);
        if entry.file_type().is_dir() {
            fs::create_dir_all(target)?;
        } else if entry.file_type().is_file() {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

struct Vdb<'a> {
    path: &'a Path,
    address: &'a str,
}
impl Vdb<'_> {
    fn command(&self) -> Command {
        let mut command = Command::new(self.path);
        command.args(["--addr", self.address, "--no-progress"]);
        command
    }
    fn call(&self, args: &[&std::ffi::OsStr]) -> Result<Vec<u8>> {
        let result = crate::process::run(
            self.command()
                .args(args.iter().map(|arg| remote_argument(arg))),
            Duration::from_secs(60),
            None,
        )?;
        ensure!(
            !result.timed_out && result.status.success(),
            "vdb: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        Ok(result.stdout)
    }
    fn push(&self, source: &Path, destination: &Path) -> Result<()> {
        self.call(&["push".as_ref(), source.as_os_str(), destination.as_os_str()])?;
        Ok(())
    }
    fn status(&self, path: &Path) -> Result<Option<serde_json::Value>> {
        let result = crate::process::run(
            self.command()
                .arg("cat")
                .arg(remote_argument(path.as_os_str())),
            Duration::from_secs(10),
            None,
        )?;
        ensure!(!result.timed_out, "vdb status query timed out");
        if result.status.success() {
            return Ok(Some(
                serde_json::from_slice(&result.stdout).context("read player status")?,
            ));
        }
        let error = String::from_utf8_lossy(&result.stderr);
        if error.trim() == "vdb: file not found" {
            return Ok(None);
        }
        bail!("vdb status: {error}")
    }
    fn pull(&self, source: &Path, destination: &Path) -> Result<()> {
        // vdb pull is create-only. Downloads are staged before replacing a report.
        let temporary = destination.with_extension("download");
        if temporary.exists() {
            fs::remove_file(&temporary)?;
        }
        self.call(&["pull".as_ref(), source.as_os_str(), temporary.as_os_str()])?;
        if destination.exists() {
            fs::remove_file(destination)?;
        }
        fs::rename(temporary, destination)?;
        Ok(())
    }
}

struct LogChild(std::process::Child);
impl Drop for LogChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn remote_argument(value: &std::ffi::OsStr) -> std::ffi::OsString {
    let text = value.to_string_lossy();
    if text.starts_with("ux0:") {
        text.replace('\\', "/").into()
    } else {
        value.to_owned()
    }
}
