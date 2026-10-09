//! Kira provenance and actual audio probes; no device or Aside execution claims.
use crate::{files, json, maintenance::artifacts, process, root, sha};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MODIFIED: [&str; 2] = ["src/backend/resources.rs", "src/track/main/handle.rs"];
const ORIGINAL_COUNT: usize = 126;
// Approved source/patch identities, independent of mutable JSON fields.
const SOURCE_MANIFEST: &str = "29d477b6d62955dd5bcb7eef9008bed9b2b4610c42102aae29d1db30a2885a71";
const APPROVED_PATCH: &str = "87b2ff43ae9fe88df2e4af351e622085319f183e38c18f23c9fe88feb70c8045";
const VCS: &str = "74b3ef0fd8bd15f90e3b469208ee741f04374a62";
const PINS: [(&str, &str); 3] = [
    (
        "LICENSE-MIT",
        "1d075e895672c131d6adda4c0d03933e4162ca732d8c99b0cc497af884b8e703",
    ),
    (
        "LICENSE-APACHE",
        "85bb5389360900e5ef360073a906f7ff6c2866995743242671091f376fe2c92d",
    ),
    (
        "rustfmt.toml",
        "bce79651b2e2ff42e1c54b9a26c5e17af5222f9f65ab51d9563a48fd159dae84",
    ),
];
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

#[derive(Deserialize)]
struct Pin {
    url: String,
    sha256: String,
}

fn check_pins(fork: &Path, manifest: &str, pins: &[(&str, &str)]) -> Result<()> {
    let sources: BTreeMap<String, Pin> = serde_json::from_slice(&fs::read(fork.join(manifest))?)?;
    ensure!(sources.len() == pins.len(), "changed {manifest} file set");
    for &(name, digest) in pins {
        let pin = sources.get(name).with_context(|| name.to_owned())?;
        ensure!(
            pin.sha256 == digest
                && pin.url
                    == format!("https://raw.githubusercontent.com/tesselode/kira/{VCS}/{name}"),
            "changed exact-VCS source record: {name}"
        );
        ensure!(
            sha(&fs::read(fork.join(name))?) == digest,
            "source hash: {name}"
        );
    }
    Ok(())
}

fn verify(fork: &Path, upstream: Option<&Path>, dir: &Path) -> Result<()> {
    let source_bytes = fs::read(fork.join("UPSTREAM-SHA256.json"))?;
    ensure!(
        sha(&source_bytes) == SOURCE_MANIFEST,
        "changed original Kira manifest"
    );
    let original: BTreeMap<String, String> = serde_json::from_slice(&source_bytes)?;
    ensure!(
        original.len() == ORIGINAL_COUNT,
        "changed original Kira file count"
    );
    ensure!(
        sha(&fs::read(fork.join("REEL-PATCH.patch"))?) == APPROVED_PATCH,
        "changed approved two-file patch"
    );
    let vcs = json(&fork.join(".cargo_vcs_info.json"))?;
    ensure!(
        vcs["git"]["sha1"] == VCS && vcs["path_in_vcs"] == "crates/kira",
        "changed upstream VCS identity"
    );
    check_pins(fork, "LICENSE-SOURCES.json", &PINS[..2])?;
    check_pins(fork, "UPSTREAM-CONFIG-SOURCES.json", &PINS[2..])?;
    let mut expected_rs = BTreeSet::new();
    for name in original.keys() {
        let path = Path::new(name);
        ensure!(
            path.components().all(|c| matches!(c, Component::Normal(_))),
            "invalid original path"
        );
        if path.extension().is_some_and(|e| e == "rs") {
            expected_rs.insert(path.to_path_buf());
        }
    }
    expected_rs.insert(PathBuf::from("src/backend/resources/reel_tests.rs"));
    let actual_rs: BTreeSet<_> = files(fork)?
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .map(|p| p.strip_prefix(fork).map(Path::to_path_buf))
        .collect::<std::result::Result<_, _>>()?;
    ensure!(actual_rs == expected_rs, "changed Kira Rust source closure");
    let restored = dir.join("restored");
    for name in MODIFIED {
        let target = restored.join(name);
        fs::create_dir_all(target.parent().context("source parent")?)?;
        fs::copy(fork.join(name), target)?;
    }
    // Run at the repository root: git apply in a nested directory would skip
    // paths outside its prefix. Only the pinned patch reaches this command.
    let relative = restored.strip_prefix(root())?.to_path_buf();
    child(
        Command::new("git")
            .args(["apply", "--reverse", "--whitespace=nowarn"])
            .arg(format!("--directory={}", relative.display()))
            .arg(fork.join("REEL-PATCH.patch"))
            .current_dir(root()),
        dir,
        "reverse-patch",
        Duration::from_secs(30),
    )?;
    for (name, digest) in &original {
        let path = if MODIFIED.contains(&name.as_str()) {
            restored.join(name)
        } else {
            fork.join(name)
        };
        ensure!(
            sha(&fs::read(&path).with_context(|| name.clone())?) == *digest,
            "original hash: {name}"
        );
        if let Some(upstream) = upstream {
            ensure!(
                sha(&fs::read(upstream.join(name))?) == *digest,
                "official package hash: {name}"
            );
        }
    }
    Ok(())
}

pub fn audit_kira(upstream: Option<&Path>) -> Result<()> {
    let dir = directory("kira-audit")?;
    verify(&root().join("third_party/kira"), upstream, &dir)?;
    println!(
        "126 original Kira files, reversible two-file patch, exact-VCS licenses/config and Rust closure verified; {}",
        dir.display()
    );
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
    fn actual_fork_reverses_and_rejects_source_patch_license_config_and_forged_records() {
        let dir = directory("kira-audit-test").unwrap();
        let fork = dir.join("fork");
        copy_tree(&root().join("third_party/kira"), &fork).unwrap();
        let pass = dir.join("valid");
        fs::create_dir(&pass).unwrap();
        verify(&fork, None, &pass).unwrap();
        for (index, name) in [
            "src/frame.rs",
            MODIFIED[0],
            "REEL-PATCH.patch",
            "LICENSE-MIT",
            "LICENSE-APACHE",
            "rustfmt.toml",
            "UPSTREAM-SHA256.json",
            "LICENSE-SOURCES.json",
            "UPSTREAM-CONFIG-SOURCES.json",
        ]
        .into_iter()
        .enumerate()
        {
            let path = fork.join(name);
            let original = fs::read(&path).unwrap();
            let mut changed = original.clone();
            changed.extend_from_slice(b"\nforged\n");
            fs::write(&path, changed).unwrap();
            let check = dir.join(format!("negative-{index}"));
            fs::create_dir(&check).unwrap();
            assert!(
                verify(&fork, None, &check).is_err(),
                "accepted changed {name}"
            );
            fs::write(path, original).unwrap();
        }
        // A valid forged record matching forged bytes must also be rejected,
        // rather than only rejecting malformed JSON or a mismatched record.
        for (index, (manifest, name)) in [
            ("LICENSE-SOURCES.json", "LICENSE-MIT"),
            ("UPSTREAM-CONFIG-SOURCES.json", "rustfmt.toml"),
            ("UPSTREAM-SHA256.json", "src/frame.rs"),
        ]
        .into_iter()
        .enumerate()
        {
            let path = fork.join(name);
            let records_path = fork.join(manifest);
            let original_bytes = fs::read(&path).unwrap();
            let original_records = fs::read(&records_path).unwrap();
            let forged = b"forged replacement\n";
            let mut records: serde_json::Value = serde_json::from_slice(&original_records).unwrap();
            if manifest == "UPSTREAM-SHA256.json" {
                records[name] = sha(forged).into();
            } else {
                records[name]["sha256"] = sha(forged).into();
            }
            fs::write(&path, forged).unwrap();
            fs::write(&records_path, serde_json::to_vec(&records).unwrap()).unwrap();
            let check = dir.join(format!("forged-record-{index}"));
            fs::create_dir(&check).unwrap();
            assert!(verify(&fork, None, &check).is_err());
            fs::write(path, original_bytes).unwrap();
            fs::write(records_path, original_records).unwrap();
        }
        fs::write(fork.join("src/unapproved.rs"), "pub fn extra() {}\n").unwrap();
        let extra = dir.join("extra");
        fs::create_dir(&extra).unwrap();
        assert!(verify(&fork, None, &extra).is_err());
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
