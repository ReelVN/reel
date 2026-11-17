//! Manual source-level diagnosis through the normal desktop development command.
use super::{Manifest, checked, collect, prepare, save};
use anyhow::{Context, Result, ensure};
use clap::Args;
use reel_player::performance::Request;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

/// Open a desktop project and record a manually reproduced issue.
#[derive(Args)]
pub struct DebugOptions {
    project: PathBuf,
    output: PathBuf,
    /// Preview the selected target's resource overrides on the desktop.
    #[arg(long, default_value = std::env::consts::OS)]
    resource_target: String,
    /// Use an existing save directory, or use a fresh directory in this capture.
    #[arg(long)]
    save_directory: Option<PathBuf>,
    /// Include sampled Aside interpreter timing.
    #[arg(long)]
    vm_timing: bool,
}

/// Run source and write a reviewable record when the window closes.
pub fn debug(options: DebugOptions) -> Result<()> {
    reel_package::resource_target::ResourceTarget::parse(&options.resource_target)?;
    let project = fs::canonicalize(options.project).context("open game project")?;
    ensure!(
        project.join("src/main.aside").is_file(),
        "project requires src/main.aside"
    );
    ensure!(
        !options.output.exists(),
        "choose a new diagnostic directory"
    );
    fs::create_dir_all(&options.output)?;
    let directory = fs::canonicalize(options.output)?;
    let saves = if let Some(saves) = options.save_directory {
        fs::create_dir_all(&saves)?;
        fs::canonicalize(saves)?
    } else {
        directory.join("saves")
    };
    println!("Building the desktop development player");
    let messages = checked(
        &mut prepare::reel_build(true),
        &directory.join("build.log"),
        Duration::from_secs(1800),
    )?;
    let exporter = prepare::artifact(&messages, "reel")?;
    let manifest = Manifest {
        schema: 1,
        target: options.resource_target,
        environment: "host".into(),
        request: Request {
            version: 1,
            package: project.join("build/dev/game.asb"),
            saves,
            output: directory.join("reports"),
            vm_timing: options.vm_timing,
            source_atlas: None,
            frame: None,
        },
        project,
        exporter,
    };
    let request = directory.join("request.json");
    save(&request, &manifest.request)?;
    save(&directory.join("run.json"), &manifest)?;
    let log_path = directory.join("player.log");
    let log = fs::File::create(&log_path)?;
    println!(
        "Reproduce the issue in the game, then close it. Log: {}",
        log_path.display()
    );
    let exit = Command::new(&manifest.exporter)
        .current_dir(&manifest.project)
        .arg("dev")
        .arg("--resource-target")
        .arg(&manifest.target)
        .env("REEL_PERFORMANCE_CONFIG", request)
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .status()?;
    let status = collect::read_status(&directory, &manifest)
        .with_context(|| format!("no player status; see {}", log_path.display()))?;
    if manifest.request.output.join("frames.tsv").is_file() {
        collect::analyze_frames(&directory, &manifest)?;
    }
    ensure!(
        status["state"] == "complete" && exit.success(),
        "{}; see {}",
        status["error"]
            .as_str()
            .unwrap_or("development player exited unexpectedly"),
        log_path.display()
    );
    Ok(())
}
