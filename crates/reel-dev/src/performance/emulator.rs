use super::*;
use std::time::Instant;

pub(super) fn run(options: &Run, directory: &Path, manifest: &Manifest) -> Result<()> {
    let executable = fs::canonicalize(
        options
            .emulator
            .as_ref()
            .context("emulator run requires --emulator")?,
    )?;
    let storage = fs::canonicalize(
        options
            .storage
            .as_ref()
            .context("emulator run requires --storage")?,
    )?;
    let configuration = configuration(options, directory, &executable, &storage)?;
    let app = storage.join("ux0/app").join(VITA_PLAYER_TITLE_ID);
    ensure!(
        app.join("eboot.bin").is_file(),
        "install the official reel-vita VPK into Vita3K first"
    );
    if let Some(runtime) = &options.runtime {
        ensure!(
            runtime.join("eboot.bin").is_file(),
            "runtime requires eboot.bin"
        );
        for name in ["eboot.bin", "launcher.rpk", "sce_sys", "licenses", "module"] {
            let source = runtime.join(name);
            if source.is_dir() {
                copy_tree(&source, &app.join(name))?;
            } else if source.is_file() {
                fs::copy(source, app.join(name))?;
            }
        }
    }
    let game = mapped(&storage, &manifest.request.package)?
        .parent()
        .context("game directory")?
        .to_owned();
    copy_tree(&directory.join("game"), &game)?;
    fs::create_dir_all(storage.join("ux0/data/reel"))?;
    fs::copy(
        directory.join("request.json"),
        storage.join("ux0/data/reel/performance.json"),
    )?;
    let remote = mapped(&storage, &manifest.request.output)?;
    ensure!(
        !remote.join("status.json").exists(),
        "emulator already has this run; choose a new run directory"
    );
    let log = fs::File::create(directory.join("player.log"))?;
    let mut command = Command::new(&executable);
    command
        .current_dir(executable.parent().context("emulator directory")?)
        .args(["-r", VITA_PLAYER_TITLE_ID, "--log-level", "2"])
        .args(["--load-config", "--keep-config", "--config-location"])
        .arg(configuration)
        .stdout(log.try_clone()?)
        .stderr(log);
    let mut child = LogChild(command.spawn()?);
    let started = Instant::now();
    let mut next_progress = Duration::from_secs(20);
    loop {
        if let Ok(bytes) = fs::read(remote.join("status.json")) {
            let status: serde_json::Value = serde_json::from_slice(&bytes)?;
            if status["state"] != "running" {
                break;
            }
        }
        ensure!(
            child.0.try_wait()?.is_none(),
            "emulator exited before player completion; see player.log"
        );
        ensure!(
            started.elapsed() < Duration::from_secs(options.timeout_seconds),
            "emulator timed out; see player.log"
        );
        if started.elapsed() >= next_progress {
            println!(
                "Waiting for the emulator: {} seconds",
                started.elapsed().as_secs()
            );
            next_progress += Duration::from_secs(20);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    collect(&storage, directory, manifest)
}

pub(super) fn collect(storage: &Path, directory: &Path, manifest: &Manifest) -> Result<()> {
    let remote = mapped(storage, &manifest.request.output)?;
    let reports = directory.join("reports");
    fs::create_dir_all(&reports)?;
    for name in ["status.json", "frames.tsv", "frozen.tsv", "frame.postcard"] {
        if remote.join(name).is_file() {
            fs::copy(remote.join(name), reports.join(name))?;
        }
    }
    Ok(())
}

fn configuration(
    options: &Run,
    directory: &Path,
    executable: &Path,
    storage: &Path,
) -> Result<PathBuf> {
    check_portable(&portable_directory(executable)?, storage)?;
    let source = options.emulator_config.clone().unwrap_or_else(|| {
        executable
            .parent()
            .expect("validated emulator directory")
            .join("config.yml")
    });
    let text = fs::read_to_string(&source).with_context(|| {
        format!(
            "read Vita3K configuration {}; use --emulator-config to select it",
            source.display()
        )
    })?;
    let storage = storage
        .to_str()
        .context("Vita3K storage path requires UTF-8")?;
    // Vita3K joins slash-separated virtual paths onto this string. Windows'
    // verbatim prefix disables slash normalization in those filesystem calls.
    let storage = if let Some(unc) = storage.strip_prefix(r"\\?\UNC\") {
        format!("//{}", unc.replace('\\', "/"))
    } else {
        storage
            .strip_prefix(r"\\?\")
            .unwrap_or(storage)
            .replace('\\', "/")
    };
    let output = directory.join("emulator.yml");
    fs::write(&output, with_storage(&text, &storage)?)?;
    println!(
        "Vita3K configuration: {} (base {})",
        output.display(),
        source.display()
    );
    Ok(output)
}

// Match Vita3K app_init.cpp: portable mode overrides YAML's pref-path with fs/.
fn portable_directory(executable: &Path) -> Result<PathBuf> {
    let base = executable.parent().context("emulator directory")?;
    #[cfg(target_os = "linux")]
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        return Ok(Path::new(&appimage)
            .parent()
            .context("AppImage directory")?
            .join("portable"));
    }
    #[cfg(target_os = "macos")]
    if let Some(bundle) = base
        .ancestors()
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
    {
        return Ok(bundle
            .parent()
            .context("emulator app bundle directory")?
            .join("portable"));
    }
    Ok(base.join("portable"))
}

fn check_portable(portable: &Path, storage: &Path) -> Result<()> {
    if portable.is_dir() {
        let forced = portable.join("fs");
        ensure!(
            fs::canonicalize(&forced).is_ok_and(|path| path == storage),
            "--storage {} conflicts with Vita3K portable mode, which forces {}; select that storage or use a non-portable emulator",
            storage.display(),
            forced.display()
        );
    }
    Ok(())
}

// Vita3K emits a root mapping with one scalar value per line. Leave unrelated
// settings and nested keys intact, and insert before YAML's document terminator.
fn with_storage(text: &str, storage: &str) -> Result<String> {
    let replacement = format!("pref-path: {}", serde_json::to_string(storage)?);
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut output = String::with_capacity(text.len() + replacement.len() + newline.len());
    let mut found = false;
    for line in text.lines() {
        let key = line.split_once(':').map(|(key, _)| key.trim_end());
        if matches!(key, Some("pref-path" | "\"pref-path\"" | "'pref-path'")) {
            output.push_str(&replacement);
            found = true;
        } else {
            if !found && (line == "..." || line.starts_with("... #")) {
                output.push_str(&replacement);
                output.push_str(newline);
                found = true;
            }
            output.push_str(line);
        }
        output.push_str(newline);
    }
    if !found {
        output.push_str(&replacement);
        output.push_str(newline);
    }
    Ok(output)
}

fn mapped(storage: &Path, path: &Path) -> Result<PathBuf> {
    let path = path.to_string_lossy().replace('\\', "/");
    let relative = path
        .strip_prefix("ux0:/")
        .context("emulator paths require ux0:/")?;
    ensure!(
        Path::new(relative)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_))),
        "emulator path must stay inside ux0"
    );
    Ok(storage.join("ux0").join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vita_paths_map_under_the_selected_storage() {
        assert_eq!(
            mapped(Path::new("storage"), Path::new("ux0:/data/reel/results")).unwrap(),
            Path::new("storage/ux0/data/reel/results")
        );
        assert!(mapped(Path::new("storage"), Path::new("ux0:/../other")).is_err());
        assert!(mapped(Path::new("storage"), Path::new("app0:/game.asb")).is_err());
    }

    #[test]
    fn storage_override_preserves_renderer_and_replaces_only_exact_root_key() {
        let base = "---\r\nbackend-renderer: Vulkan\r\npref-path-other: preserved\r\n# pref-path: comment\r\npref-path: old\r\napp:\r\n  pref-path: nested\r\n...\r\n";
        let storage = "D:\\Vita storage\\quoted\"name";
        let output = with_storage(base, storage).unwrap();
        assert!(output.contains("backend-renderer: Vulkan\r\n"));
        assert!(output.contains("pref-path-other: preserved\r\n"));
        assert!(output.contains("# pref-path: comment\r\n"));
        assert!(output.contains("  pref-path: nested\r\n"));
        let value = output
            .lines()
            .find_map(|line| line.strip_prefix("pref-path: "))
            .unwrap();
        assert_eq!(serde_json::from_str::<String>(value).unwrap(), storage);
        for key in ["\"pref-path\"", "'pref-path'"] {
            let output = with_storage(&format!("{key}: old\n"), storage).unwrap();
            assert_eq!(output.lines().count(), 1);
            assert!(output.starts_with("pref-path: "));
        }
    }

    #[test]
    fn absent_storage_is_inserted_before_document_end_or_at_eof() {
        for text in [
            "---\nbackend-renderer: OpenGL\n...\n",
            "backend-renderer: OpenGL",
        ] {
            let output = with_storage(text, "/selected/storage").unwrap();
            let path = output.find("pref-path: ").unwrap();
            assert!(output.contains("backend-renderer: OpenGL"));
            assert!(output.find("...").is_none_or(|end| path < end));
            assert_eq!(output.matches("pref-path: ").count(), 1);
        }
    }

    #[test]
    fn portable_storage_conflict_is_reported_instead_of_overriding_yaml() {
        let root = std::env::temp_dir().join(format!(
            "reel-vita3k-storage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let portable = root.join("portable");
        fs::create_dir_all(portable.join("fs")).unwrap();
        fs::create_dir_all(root.join("other")).unwrap();
        assert!(check_portable(&portable, &fs::canonicalize(root.join("other")).unwrap()).is_err());
        check_portable(&portable, &fs::canonicalize(portable.join("fs")).unwrap()).unwrap();
        check_portable(&root.join("not-portable"), &root).unwrap();
    }
}
