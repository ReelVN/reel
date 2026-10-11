//! Kira provenance and actual audio probes; no device or Aside execution claims.
use crate::{files, json, maintenance::artifacts, process, root};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const BUILD_TIMEOUT: Duration = Duration::from_secs(600);
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

fn directory(label: &str) -> Result<PathBuf> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = artifacts()?.join(format!(
        "{label}-{}-{stamp}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path)?;
    Ok(path)
}

fn child(command: &mut Command, dir: &Path, label: &str, timeout: Duration) -> Result<Vec<u8>> {
    let result = process::run(command, timeout, None)?;
    fs::write(dir.join(format!("{label}.stdout")), &result.stdout)?;
    fs::write(dir.join(format!("{label}.stderr")), &result.stderr)?;
    fs::write(
        dir.join(format!("{label}.json")),
        serde_json::to_vec_pretty(&serde_json::json!({
            "command": format!("{command:?}"),
            "exit": process::return_code(result.status),
            "timed_out": result.timed_out,
        }))?,
    )?;
    ensure!(
        !result.timed_out && result.status.success(),
        "{label} failed (exit {:?}, timeout {}); raw output: {}",
        process::return_code(result.status),
        result.timed_out,
        dir.display()
    );
    Ok(result.stdout)
}

fn verify_provenance(fork: &Path) -> Result<()> {
    for name in ["LICENSE-MIT", "LICENSE-APACHE", "REEL-FORK.md"] {
        ensure!(
            !fs::read(fork.join(name))?.is_empty(),
            "missing Kira license/change record: {name}"
        );
    }
    let vcs = json(&fork.join(".cargo_vcs_info.json"))?;
    ensure!(
        vcs["git"]["sha1"].as_str().is_some_and(|s| !s.is_empty()),
        "missing Kira origin"
    );
    files(fork)?;
    Ok(())
}

pub fn audit_kira(upstream: Option<&Path>) -> Result<()> {
    let fork = root().join("third_party/kira");
    verify_provenance(&fork)?;
    if let Some(upstream) = upstream {
        let mut differences = Vec::new();
        for source in files(upstream)? {
            let relative = source.strip_prefix(upstream)?;
            if fs::read(fork.join(relative)).ok() != Some(fs::read(&source)?) {
                differences.push(relative.to_string_lossy().into_owned());
            }
        }
        let dir = directory("kira-comparison")?;
        fs::write(
            dir.join("differences.json"),
            serde_json::to_vec_pretty(&differences)?,
        )?;
        println!(
            "Kira source comparison: {} differences; {}",
            differences.len(),
            dir.display()
        );
    }
    println!("Kira origin, licenses and local change record present; source changes are allowed");
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    for file in files(source)? {
        let destination = target.join(file.strip_prefix(source)?);
        fs::create_dir_all(destination.parent().context("destination parent")?)?;
        fs::copy(file, destination)?;
    }
    Ok(())
}

pub fn regression(release: bool) -> Result<()> {
    audit_kira(None)?;
    let dir = directory("kira-regression")?;
    let fork = dir.join("fork");
    copy_tree(&root().join("third_party/kira"), &fork)?;
    // The copy lives below root but is an independent upstream workspace.
    // A sibling wrapper preserves both original imported manifests byte-for-byte.
    fs::write(
        dir.join("Cargo.toml"),
        "[workspace]\nmembers = [\"fork\"]\nresolver = \"2\"\n",
    )?;
    fs::copy(
        root().join("crates/reel-media/tests/support/kira-test.lock"),
        dir.join("Cargo.lock"),
    )?;
    let mut command = Command::new("cargo");
    command
        .args(["test", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(dir.join("build"))
        .args([
            "--no-default-features",
            "--locked",
            "--lib",
            "reel_retirement",
        ]);
    if release {
        command.arg("--release");
    }
    command
        .args(["--", "--nocapture", "--test-threads=1"])
        .current_dir(root());
    let output = child(&mut command, &dir, "retirement", BUILD_TIMEOUT)?;
    print!("{}", String::from_utf8_lossy(&output));
    println!(
        "Actual Kira retirement/quota/play regression: {}",
        dir.display()
    );
    Ok(())
}

fn library(build: &[u8], manifest: &Path, target: &str) -> Result<PathBuf> {
    let mut selected = BTreeSet::new();
    for line in std::str::from_utf8(build)?.lines() {
        let row: serde_json::Value = serde_json::from_str(line)?;
        if row["reason"] == "compiler-artifact"
            && row["target"]["name"] == target
            && row["manifest_path"].as_str() == manifest.to_str()
            && let Some(filenames) = row["filenames"].as_array()
        {
            for file in filenames.iter().filter_map(|f| f.as_str()) {
                if Path::new(file).extension().is_some_and(|e| e == "rlib") {
                    selected.insert(PathBuf::from(file));
                }
            }
        }
    }
    ensure!(
        selected.len() == 1,
        "expected one exact {target} library in Cargo JSON"
    );
    selected.into_iter().next().context("library")
}

pub fn probes() -> Result<()> {
    let dir = directory("audio-probe")?;
    let support = root().join("crates/reel-media/tests/support");
    for (profile, release) in [("debug", false), ("release", true)] {
        let mut command = Command::new("cargo");
        command
            .args([
                "build",
                "-p",
                "reel-media",
                "--no-default-features",
                "--locked",
                "--message-format=json",
            ])
            .arg("--target-dir")
            .arg(dir.join("build"))
            .current_dir(root());
        if release {
            command.arg("--release");
        }
        let build = child(
            &mut command,
            &dir,
            &format!("build-{profile}"),
            BUILD_TIMEOUT,
        )?;
        let media = library(
            &build,
            &root().join("crates/reel-media/Cargo.toml"),
            "reel_media",
        )?;
        let core = library(
            &build,
            &root().join("crates/reel-core/Cargo.toml"),
            "reel_core",
        )?;
        for probe in ["pcm", "reclaim"] {
            let label = format!("{probe}-{profile}");
            let executable = dir.join(format!("{label}{}", std::env::consts::EXE_SUFFIX));
            let mut command = Command::new("rustc");
            command
                .args(["--edition", "2024"])
                .arg(support.join(format!("{probe}_probe.rs")))
                .arg("--extern")
                .arg(format!("reel_media={}", media.display()))
                .arg("--extern")
                .arg(format!("reel_core={}", core.display()))
                .arg("-L")
                .arg(format!(
                    "dependency={}",
                    core.parent().context("dependency directory")?.display()
                ))
                .arg("-o")
                .arg(&executable)
                .current_dir(root());
            if release {
                command.arg("-O");
            }
            child(
                &mut command,
                &dir,
                &format!("compile-{label}"),
                BUILD_TIMEOUT,
            )?;
            let output = child(
                Command::new(&executable).current_dir(root()),
                &dir,
                &label,
                PROBE_TIMEOUT,
            )?;
            println!("profile={profile} probe={probe}");
            print!("{}", String::from_utf8_lossy(&output));
        }
    }
    println!(
        "Actual Kira/8ch/allocator/64MiB probes (offline, no hardware claim): {}",
        dir.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_edits_and_new_modules_do_not_require_repinning_but_licenses_remain_required() {
        let dir = directory("kira-provenance-test").unwrap();
        let fork = dir.join("fork");
        copy_tree(&root().join("third_party/kira"), &fork).unwrap();
        fs::write(
            fork.join("src/frame.rs"),
            "// a reviewed local source edit\n",
        )
        .unwrap();
        fs::write(fork.join("src/new_module.rs"), "// a new local module\n").unwrap();
        verify_provenance(&fork).unwrap();
        fs::remove_file(fork.join("LICENSE-MIT")).unwrap();
        assert!(verify_provenance(&fork).is_err());
    }

    #[test]
    fn cargo_library_selection_uses_exact_manifest_and_rejects_ambiguity() {
        let manifest = Path::new("/repo/crates/reel-media/Cargo.toml");
        let row = serde_json::json!({"reason":"compiler-artifact", "manifest_path":manifest, "target":{"name":"reel_media"}, "filenames":["/build/deps/libreel_media-a.rlib","/build/deps/libreel_media-a.rmeta"]});
        let unrelated = serde_json::json!({"reason":"compiler-artifact", "manifest_path":"/other/reel-media/Cargo.toml", "target":{"name":"reel_media"}, "filenames":["/old/libreel_media-b.rlib"]});
        let data = format!("{unrelated}\n{row}\n");
        assert_eq!(
            library(data.as_bytes(), manifest, "reel_media").unwrap(),
            Path::new("/build/deps/libreel_media-a.rlib")
        );
        assert!(library(format!("{unrelated}\n").as_bytes(), manifest, "reel_media").is_err());
        let mut duplicate = row;
        duplicate["filenames"] = serde_json::json!(["/build/deps/libreel_media-b.rlib"]);
        assert!(
            library(
                format!("{data}{duplicate}\n").as_bytes(),
                manifest,
                "reel_media"
            )
            .is_err()
        );
    }
}
