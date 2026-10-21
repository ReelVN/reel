//! Workspace audio regressions and PCM probes; no device execution claims.
use crate::{maintenance::artifacts, process, root};
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

pub fn regression(release: bool) -> Result<()> {
    let dir = directory("audio-regression")?;
    let mut command = Command::new("cargo");
    command.args([
        "test",
        "-p",
        "reel-audio",
        "--lib",
        "--no-default-features",
        "--locked",
    ]);
    if release {
        command.arg("--release");
    }
    command.args(["--", "--nocapture"]).current_dir(root());
    let output = child(&mut command, &dir, "audio-library", BUILD_TIMEOUT)?;
    print!("{}", String::from_utf8_lossy(&output));
    println!(
        "Workspace audio mixing and retirement regression: {}",
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
            && row["manifest_path"].as_str().map(Path::new) == Some(manifest)
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
                "--features",
                "decode",
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
        "Actual reel-audio/8ch/allocator/64MiB probes (offline, no hardware claim): {}",
        dir.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
