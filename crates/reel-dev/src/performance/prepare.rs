use super::*;

pub(super) fn game(options: Prepare) -> Result<()> {
    ensure!(
        ["vita", "windows", "linux", "macos"].contains(&options.target.as_str()),
        "unsupported resource target"
    );
    let environment = options.environment.unwrap_or_else(|| {
        if options.target == "vita" {
            "vita"
        } else {
            "host"
        }
        .to_owned()
    });
    ensure!(
        (options.target == "vita") == (environment != "host"),
        "environment must match the resource target"
    );
    ensure!(
        !options.serial || options.frame.is_some(),
        "--serial requires --frame"
    );
    ensure!(
        options.target == "vita" || options.target == std::env::consts::OS,
        "desktop preparation uses the current host's player and resource target"
    );
    ensure!(
        !options.output.exists(),
        "choose a new run directory: {}",
        options.output.display()
    );
    let project = fs::canonicalize(&options.project).context("open game project")?;
    ensure!(
        project.join("src/main.aside").is_file(),
        "project requires src/main.aside"
    );
    if options.target != "vita" {
        ensure!(
            options.frame.is_none(),
            "fixed-frame capture currently requires GXM/Vita"
        );
    }
    fs::create_dir_all(&options.output)?;
    let directory = fs::canonicalize(&options.output)?;
    let name = directory
        .file_name()
        .and_then(|v| v.to_str())
        .context("run name")?;
    ensure!(
        name.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "run directory name uses letters, digits, '-' or '_'"
    );
    let mut build = Command::new("cargo");
    build.current_dir(crate::root()).args([
        "build",
        "--release",
        "--locked",
        "-p",
        "reel-cli",
        "--bin",
        "reel",
        "--message-format=json-render-diagnostics",
    ]);
    if options.target != "vita" {
        build.args([
            "--features",
            "window,reel-player/performance",
            "--bin",
            "reel-game",
        ]);
    }
    println!("Building the current exporter and player");
    let messages = checked(
        &mut build,
        &directory.join("build.log"),
        Duration::from_secs(1800),
    )?;
    let exporter = artifact(&messages, "reel")?;
    let game = directory.join("game");
    println!("Exporting {} for {}", project.display(), options.target);
    checked(
        Command::new(&exporter)
            .current_dir(&project)
            .args([
                "export",
                "--release",
                "--data-only",
                "--resource-target",
                &options.target,
                "--output",
            ])
            .arg(&game),
        &directory.join("export.log"),
        Duration::from_secs(1800),
    )?;
    if options.target != "vita" {
        let player = artifact(&messages, "reel-game")?;
        fs::copy(
            &player,
            game.join(player.file_name().context("player executable name")?),
        )?;
    }
    let (package, saves, output) = if options.target == "vita" {
        let root = format!("ux0:/data/reel/perf-{name}");
        (
            PathBuf::from(format!("{root}/game.asb")),
            PathBuf::from(format!("{root}/saves")),
            PathBuf::from(format!("ux0:/data/reel/performance/{name}")),
        )
    } else {
        (
            game.join("game.asb"),
            directory.join("saves"),
            directory.join("reports"),
        )
    };
    let request = Request {
        version: 1,
        package,
        saves,
        output,
        vm_timing: options.vm_timing,
        source_atlas: options.source_atlas.map(|v| v == "on"),
        frame: options.frame.map(|label| FrameCaptureOptions {
            label,
            accepted_frame: options.accepted_frame,
            repetitions: options.repetitions,
            cache: if options.cache == "reuse" {
                CachePolicy::Reuse
            } else {
                CachePolicy::Replay
            },
            measure_passes: options.serial,
            save_frame: options.save_frame,
        }),
    };
    request
        .validate()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    save(&directory.join("request.json"), &request)?;
    save(
        &directory.join("run.json"),
        &Manifest {
            schema: 1,
            target: options.target,
            environment,
            project,
            exporter,
            request,
        },
    )?;
    println!("Prepared {}", directory.display());
    Ok(())
}

fn artifact(messages: &[u8], name: &str) -> Result<PathBuf> {
    String::from_utf8_lossy(messages)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|v| v["reason"] == "compiler-artifact" && v["target"]["name"] == name)
        .filter_map(|v| v["executable"].as_str().map(PathBuf::from))
        .last()
        .with_context(|| format!("Cargo did not produce {name}"))
}

pub(super) fn runtime(output: &Path, cargo_args: &[String]) -> Result<()> {
    ensure!(!output.exists(), "choose a new runtime directory");
    fs::create_dir_all(output)?;
    let output = fs::canonicalize(output)?;
    let mut command = Command::new("cargo");
    command
        .current_dir(crate::root())
        .args([
            "+nightly",
            "vita",
            "build",
            "vpk",
            "--release",
            "--locked",
            "-p",
            "reel-player",
            "--bin",
            "reel-vita",
            "--no-default-features",
            "--features",
            "vita,performance",
        ])
        .args(cargo_args);
    checked(
        &mut command,
        &output.join("build.log"),
        Duration::from_secs(1800),
    )?;
    let metadata = checked(
        Command::new("cargo").current_dir(crate::root()).args([
            "metadata",
            "--format-version=1",
            "--no-deps",
        ]),
        &output.join("cargo-metadata.log"),
        Duration::from_secs(60),
    )?;
    let metadata: serde_json::Value = serde_json::from_slice(&metadata)?;
    let target = PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .context("cargo target directory")?,
    )
    .join("armv7-sony-vita-newlibeabihf/release");
    copy_tree(
        &crate::root().join("crates/reel-player/runtime/vita"),
        &output,
    )?;
    fs::copy(target.join("reel-vita.self"), output.join("eboot.bin"))?;
    fs::create_dir_all(output.join("sce_sys"))?;
    fs::copy(
        target.join("reel-vita.sfo"),
        output.join("sce_sys/param.sfo"),
    )?;
    fs::copy(target.join("reel-vita.vpk"), output.join("reel-vita.vpk"))?;
    // Keep build metadata local; deployment sends only the runtime's assets.
    println!("Official Vita performance runtime: {}", output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cargo_artifact_uses_reported_executable_with_custom_target_directory() {
        let messages = br#"{"reason":"compiler-artifact","target":{"name":"reel"},"executable":"/custom/cache/reel"}
{"reason":"compiler-artifact","target":{"name":"reel-game"},"executable":"/custom/cache/reel-game"}"#;
        assert_eq!(
            artifact(messages, "reel").unwrap(),
            PathBuf::from("/custom/cache/reel")
        );
        assert!(artifact(messages, "reel-vita").is_err());
    }
}
