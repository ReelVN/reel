use crate::{files, root, sha};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub fn artifacts() -> Result<PathBuf> {
    let path = root().join("target/reel-artifacts");
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn output(command: &mut Command) -> Result<String> {
    let result = command.output().with_context(|| format!("{command:?}"))?;
    ensure!(
        result.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(String::from_utf8(result.stdout)?)
}

pub fn dependencies(manifest: Option<&Path>) -> Result<()> {
    let manifest = manifest
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root().join("Cargo.toml"))
        .canonicalize()?;
    let metadata: serde_json::Value = serde_json::from_str(&output(
        Command::new("cargo")
            .args([
                "metadata",
                "--locked",
                "--format-version",
                "1",
                "--manifest-path",
            ])
            .arg(&manifest),
    )?)?;
    let packages = metadata["packages"].as_array().context("packages")?;
    if manifest == root().join("Cargo.toml") {
        ensure!(
            metadata["workspace_root"].as_str() == root().to_str(),
            "workspace root changed"
        );
        for (name, version, dir) in [
            ("masonry", "0.4.0", "masonry"),
            ("masonry_core", "0.4.0", "masonry_core"),
            ("tree_arena", "0.2.0", "tree_arena"),
            ("linebender_include_doc_path", "0.1.0", "include_doc_path"),
        ] {
            let selected: Vec<_> = packages.iter().filter(|p| p["name"] == name).collect();
            ensure!(
                selected.len() == 1 && selected[0]["version"] == version,
                "duplicate or wrong-version {name}"
            );
            let p = selected[0];
            ensure!(
                p["manifest_path"].as_str()
                    == root().join("crates").join(dir).join("Cargo.toml").to_str(),
                "wrong source for {name}"
            );
            ensure!(
                metadata["workspace_members"]
                    .as_array()
                    .unwrap()
                    .contains(&p["id"]),
                "{name} is not a workspace member"
            );
            if name == "tree_arena" {
                let node = metadata["resolve"]["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|n| n["id"] == p["id"])
                    .context("tree node")?;
                ensure!(
                    node["features"]
                        .as_array()
                        .unwrap()
                        .contains(&serde_json::json!("safe_tree")),
                    "production tree lost safe_tree"
                );
            }
            println!("{name} {version}: one local workspace member");
        }
    } else {
        ensure!(
            metadata["workspace_root"].as_str() == manifest.parent().unwrap().to_str(),
            "experiment is not isolated"
        );
        for name in [
            "masonry",
            "masonry_core",
            "tree_arena",
            "imaging",
            "peniko",
            "kurbo",
        ] {
            ensure!(
                packages.iter().filter(|p| p["name"] == name).count() == 1,
                "duplicate {name}"
            );
        }
    }
    let mut command = Command::new("cargo");
    command
        .args(["tree", "--locked", "--manifest-path"])
        .arg(&manifest)
        .args(["-e", "normal,build", "--prefix", "none"]);
    if manifest == root().join("Cargo.toml") {
        command.args(["-p", "reel-stage", "-p", "reel-render"]);
    }
    let tree = output(&mut command)?;
    ensure!(
        !tree.lines().any(|l| matches!(
            l.split_whitespace().next(),
            Some("winit" | "wgpu" | "masonry_winit")
        )),
        "window/GPU dependency entered headless closure"
    );
    println!("No active winit/wgpu/masonry_winit normal/build dependencies");
    Ok(())
}

pub fn audit_masonry(upstream: &Path) -> Result<()> {
    let base = root().join("crates");
    let manifest = fs::read_to_string(base.join("masonry/UPSTREAM-SOURCE.sha256"))?;
    let mut expected = BTreeSet::new();
    let mut diff = Vec::new();
    for line in manifest.lines() {
        let (digest, name) = line.split_once("  ").context("source manifest line")?;
        let original = upstream.join(name);
        let before = fs::read(&original).with_context(|| name.to_owned())?;
        ensure!(sha(&before) == digest, "upstream hash: {name}");
        let local = match name {
            "Cargo.toml" => root().join("Cargo.toml"),
            "LICENSE" => base.join("masonry/LICENSE"),
            _ => base.join(name),
        };
        let after = fs::read(&local).with_context(|| format!("retained source: {name}"))?;
        if name.ends_with("LICENSE") {
            ensure!(before == after, "changed license: {name}");
        }
        if name != "Cargo.toml" && name.ends_with("Cargo.toml") {
            expected.insert(base.join(name));
        }
        if name.ends_with(".rs") {
            expected.insert(base.join(name));
            let source = std::str::from_utf8(&after)?;
            for line in std::str::from_utf8(&before)?.lines().take(4) {
                if line.starts_with("// Copyright") || line.starts_with("// SPDX-") {
                    ensure!(source.contains(line), "missing copyright header: {name}");
                }
            }
        }
        if before != after {
            let result = Command::new("git")
                .args(["diff", "--no-index", "--no-ext-diff", "--no-color", "--"])
                .arg(&original)
                .arg(&local)
                .output()?;
            ensure!(
                matches!(result.status.code(), Some(0 | 1)),
                "source diff failed"
            );
            diff.extend(result.stdout);
        }
    }
    expected.insert(base.join("masonry_core/src/app/presented_geometry.rs"));
    expected.insert(base.join("tree_arena/tests/lookup_depth.rs"));
    let mut actual = BTreeSet::new();
    for name in ["masonry", "masonry_core", "tree_arena", "include_doc_path"] {
        for path in files(&base.join(name))? {
            if path.extension().is_some_and(|e| e == "rs")
                || path.file_name().is_some_and(|n| n == "Cargo.toml")
            {
                actual.insert(path);
            }
        }
    }
    ensure!(
        actual == expected,
        "retained source closure differs from provenance; extra={:?}; missing={:?}",
        actual.difference(&expected).collect::<Vec<_>>(),
        expected.difference(&actual).collect::<Vec<_>>()
    );
    for name in [
        "masonry_core/src/app/presented_geometry.rs",
        "tree_arena/tests/lookup_depth.rs",
    ] {
        let result = Command::new("git")
            .args([
                "diff",
                "--no-index",
                "--no-ext-diff",
                "--no-color",
                "--",
                "/dev/null",
            ])
            .arg(base.join(name))
            .output()?;
        ensure!(result.status.code() == Some(1), "new source diff failed");
        diff.extend(result.stdout);
    }
    let target = artifacts()?.join("masonry-production.patch");
    fs::write(&target, diff)?;
    println!(
        "Verified {} pinned files, original licenses/headers and Rust source closure; diff: {}",
        manifest.lines().count(),
        target.display()
    );
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if matches!(
            entry.file_name().to_str(),
            Some(".git" | "target" | "__pycache__" | "experiment")
        ) {
            continue;
        }
        ensure!(!entry.file_type()?.is_symlink(), "unexpected symlink");
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), target.join(entry.file_name()))?;
        }
    }
    Ok(())
}

pub fn negative_controls() -> Result<()> {
    let directory = artifacts()?.join(format!("root-negative-{}", std::process::id()));
    ensure!(
        !directory.exists(),
        "negative-control directory already exists"
    );
    let copy = directory.join("repo");
    copy_tree(&root(), &copy)?;
    let names = ["frame_slots.rs", "gc.rs", "root_ranges.rs"];
    let sources: Vec<_> = names
        .iter()
        .map(|n| fs::read_to_string(root().join("crates/aside-vm/src").join(n)))
        .collect::<std::io::Result<_>>()?;
    let result = (|| -> Result<()> {
        let frame = copy.join("crates/aside-vm/src/frame_slots.rs");
        let cases = [
            (
                "write",
                "clear",
                vec!["value"],
                "scanned_register_overwrite_alone_preserves_an_otherwise_white_object",
            ),
            (
                "swap",
                "move_to_host_root",
                vec!["a_v", "b_v"],
                "swap_alone_preserves_the_sole_unscanned_root_moved_behind_the_cursor",
            ),
            (
                "move_value",
                "swap",
                vec!["value"],
                "move_alone_preserves_the_sole_unscanned_root_and_its_initialization_state",
            ),
        ];
        for (method, next, values, test) in cases {
            let start = sources[0]
                .find(&format!("    pub fn {method}("))
                .context("mutation start")?;
            let end = start
                + sources[0][start..]
                    .find(&format!("    pub fn {next}("))
                    .context("mutation end")?;
            let mut changed = sources[0][start..end].to_owned();
            for value in values {
                let line = format!("heap.root_insertion_barrier({value})?;");
                ensure!(
                    changed.matches(&line).count() == 1,
                    "mutation no longer uniquely applies"
                );
                changed =
                    changed.replace(&line, "// Negative control: insertion barrier disabled.");
            }
            fs::write(
                &frame,
                format!("{}{}{}", &sources[0][..start], changed, &sources[0][end..]),
            )?;
            negative_test(&copy, &directory, method, test, "InvalidHandle")?;
            fs::write(&frame, &sources[0])?;
        }
        let collector = copy.join("crates/aside-vm/src/gc.rs");
        let condition = "self.gc.range_key != Some(key)";
        ensure!(
            sources[1].matches(condition).count() == 1,
            "generation mutation changed"
        );
        fs::write(
            &collector,
            sources[1].replace(
                condition,
                "self.gc.range_key.map(crate::slots::index) != Some(crate::slots::index(key))",
            ),
        )?;
        negative_test(
            &copy,
            &directory,
            "generation",
            "publication_null_initialization_and_generation_reuse_have_real_distinct_work",
            "index out of bounds",
        )?;
        fs::write(&collector, &sources[1])?;
        let fields = "    pub data: Vec<T>,\n    charge: Reservation,";
        ensure!(
            sources[2].matches(fields).count() == 1,
            "refund mutation changed"
        );
        fs::write(
            copy.join("crates/aside-vm/src/root_ranges.rs"),
            sources[2].replace(fields, "    charge: Reservation,\n    pub data: Vec<T>,"),
        )?;
        let result = Command::new("bash")
            .arg("crates/aside-vm/tests/support/run_root_range_dealloc_probe.sh")
            .current_dir(&copy)
            .output()?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        fs::write(directory.join("refund.log"), &text)?;
        ensure!(
            !result.status.success()
                && text.contains("budget refunded while allocator still owns bytes"),
            "refund negative control did not fail as expected"
        );
        Ok(())
    })();
    for (name, source) in names.iter().zip(sources) {
        ensure!(
            fs::read_to_string(root().join("crates/aside-vm/src").join(name))? == source,
            "production checkout changed"
        );
    }
    result?;
    fs::remove_dir_all(copy)?;
    println!(
        "All nine negative controls failed for the expected reason; production unchanged. Logs: {}",
        directory.display()
    );
    Ok(())
}

fn negative_test(
    copy: &Path,
    directory: &Path,
    label: &str,
    test: &str,
    reason: &str,
) -> Result<()> {
    for release in [false, true] {
        let mut command = Command::new("cargo");
        command
            .args(["test", "-p", "aside-vm", "--locked"])
            .env("CARGO_TARGET_DIR", directory.join("target"))
            .current_dir(copy);
        if release {
            command.arg("--release");
        }
        command.args(["--test", "frame_slots", test, "--", "--exact"]);
        let result = command.output()?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        fs::write(directory.join(format!("{label}-{release}.log")), &text)?;
        ensure!(
            !result.status.success() && text.contains(reason),
            "{label} release={release}: negative control did not fail as expected"
        );
        println!("{label} release={release}: expected failure ({reason})");
    }
    Ok(())
}

pub fn measure(build: &Path, target: &str, arguments: &[String]) -> Result<()> {
    let package = if matches!(target, "contracts" | "r01_measurements") {
        "reel-stage"
    } else {
        bail!("measurement supports only stage contracts/r01_measurements")
    };
    let mut executable = None;
    for line in fs::read_to_string(build)?.lines() {
        let row: serde_json::Value = serde_json::from_str(line)?;
        if row["reason"] == "compiler-artifact"
            && row["target"]["name"] == target
            && row["manifest_path"]
                .as_str()
                .is_some_and(|s| s.contains(&format!("/{package}/")))
            && let Some(path) = row["executable"].as_str()
        {
            executable = Some(path.to_owned());
        }
    }
    let executable = executable.context("test executable missing from Cargo JSON")?;
    let path = artifacts()?.join(format!("{target}-{}.log", std::process::id()));
    let log = fs::File::create(&path)?;
    let mut command = Command::new("time"); // GNU time measures this child only, excluding Cargo.
    command
        .args(["-v", "--"])
        .arg(if arguments.iter().any(|s| s == "--launch-control") {
            "/bin/true"
        } else {
            &executable
        });
    if !arguments.iter().any(|s| s == "--launch-control") {
        command
            .args(arguments)
            .args(["--test-threads=1", "--nocapture"]);
    }
    let status = command
        .stdout(log.try_clone()?)
        .stderr(log)
        .status()
        .context("GNU time must be installed")?;
    println!("{}: {status}", path.display());
    ensure!(status.success(), "measurement failed");
    Ok(())
}

pub fn prepare_experiment(upstream: &Path) -> Result<()> {
    let base = root().join("tools/masonry-contract-check");
    let target = artifacts()?.join("masonry-experiment");
    ensure!(
        !target.exists(),
        "experiment output already exists: {}",
        target.display()
    );
    let manifest = fs::read_to_string(base.join("upstream-source.sha256"))?;
    // Verify every source before writing the experiment.
    for line in manifest.lines() {
        let (digest, name) = line.split_once("  ").context("manifest line")?;
        ensure!(
            sha(&fs::read(upstream.join(name))?) == digest,
            "upstream hash: {name}"
        );
    }
    for line in manifest.lines() {
        let (_, name) = line.split_once("  ").unwrap();
        let dest = target.join("upstream").join(name);
        fs::create_dir_all(dest.parent().unwrap())?;
        fs::copy(upstream.join(name), dest)?;
    }
    let path = target.join("upstream/Cargo.toml");
    let mut text = fs::read_to_string(&path)?;
    let start = text.find("members = [").context("members")?;
    let end = start + text[start..].find(']').context("members end")? + 1;
    text.replace_range(start..end, "members = [\"masonry_core\", \"tree_arena\"]");
    fs::write(&path, text)?;
    for dry in [true, false] {
        let mut command = Command::new("patch");
        command
            .args(["-p1", "--batch", "--input"])
            .arg(base.join("patches/presented-geometry.patch"))
            .current_dir(target.join("upstream"));
        if dry {
            command.arg("--dry-run");
        }
        output(&mut command)?;
    }
    for name in ["masonry_core", "tree_arena"] {
        let path = target.join("upstream").join(name).join("Cargo.toml");
        fs::write(
            &path,
            fs::read_to_string(&path)?.replacen("[package]", "[package]\nworkspace = \"..\"", 1),
        )?;
    }
    let mut text = fs::read_to_string(base.join("Cargo.toml"))?;
    text.push_str(&format!("\n[[bin]]\nname = \"masonry-contract-check\"\npath = {}\n\n[patch.\"https://github.com/linebender/xilem\"]\nmasonry_core = {{ path = \"upstream/masonry_core\" }}\ntree_arena = {{ path = \"upstream/tree_arena\" }}\n",serde_json::to_string(&base.join("src/main.rs"))?));
    fs::write(target.join("Cargo.toml"), text)?;
    fs::copy(
        base.join("locks/experimental.lock"),
        target.join("Cargo.lock"),
    )?;
    println!("Pinned experiment prepared at {}", target.display());
    Ok(())
}

const SEMANTIC_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

fn compiler_sample(
    command: &mut Command,
    probe: &str,
    semantic_timeout: Duration,
) -> Result<crate::process::Outcome> {
    if probe == "semantic" {
        Ok(crate::process::run(command, semantic_timeout, None)?)
    } else {
        let output = command.output().context("GNU time must be installed")?;
        Ok(crate::process::Outcome {
            status: output.status,
            timed_out: false,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

fn save_compiler_sample(
    directory: &Path,
    records: &mut Vec<serde_json::Value>,
    mut record: serde_json::Value,
    result: crate::process::Outcome,
) -> Result<(String, bool)> {
    let stem = format!(
        "{}-{}-{}",
        record["mode"].as_str().context("mode")?,
        record["n"],
        record["repeat"]
    );
    let stdout_path = format!("{stem}.stdout");
    let stderr_path = format!("{stem}.stderr");
    fs::write(directory.join(&stdout_path), &result.stdout)?;
    fs::write(directory.join(&stderr_path), &result.stderr)?;
    record["stdout_raw"] = serde_json::json!(stdout_path);
    record["stderr_raw"] = serde_json::json!(stderr_path);
    record["exit"] = serde_json::json!(crate::process::return_code(result.status));
    record["timed_out"] = serde_json::json!(result.timed_out);
    // Keep exact byte files even when a failed child emits invalid UTF-8.
    record["stdout"] = serde_json::json!(String::from_utf8_lossy(&result.stdout));
    record["stderr"] = serde_json::json!(String::from_utf8_lossy(&result.stderr));
    let rejected = result.stdout.starts_with(b"rejected,");
    record["rejected"] = serde_json::json!(rejected);
    records.push(record);
    fs::write(
        directory.join("samples.json"),
        serde_json::to_string_pretty(records)? + "\n",
    )?;
    ensure!(
        !result.timed_out && result.status.success(),
        "probe failed or exceeded its timeout; see {}",
        directory.display()
    );
    Ok((String::from_utf8(result.stdout)?, rejected))
}

/// Measure already built Rust compiler probes. No bytecode dispatch or host calls.
pub fn compiler_probe(probe: &str, selected: Option<&str>) -> Result<()> {
    use serde_json::json;
    let workloads: Vec<(&str, Vec<usize>)> = match probe {
        "frontend" => vec![("parse", vec![0]), ("lex", vec![0]), ("named", vec![0])],
        "semantic" => vec![
            ("captures", vec![1000, 5000, 10000]),
            ("named", vec![1000, 5000, 10000]),
            ("shared", vec![1000, 5000, 10000]),
        ],
        "lowering" => vec![
            ("plain", vec![100, 1000, 5000, 10000]),
            ("captures", vec![100, 1000, 2000, 5000]),
            ("named", vec![100, 1000, 5000, 10000]),
            ("temporaries", vec![1000, 10000, 25000, 50000]),
            ("loops", vec![100, 1000, 5000, 10000, 20000]),
        ],
        _ => bail!("probe must be frontend, semantic or lowering"),
    };
    ensure!(
        selected.is_none_or(|mode| workloads.iter().any(|(name, _)| *name == mode)),
        "invalid probe mode"
    );
    let executable = root().join(format!("target/release/examples/{probe}_probe"));
    ensure!(
        executable.is_file(),
        "build {probe}_probe in release mode first"
    );
    let directory = artifacts()?.join(format!("compiler-{probe}-{}", std::process::id()));
    fs::create_dir(&directory)?;
    let mut records = Vec::new();
    let mut summaries = Vec::new();
    for (mode, sizes) in workloads {
        if selected.is_some_and(|selected| mode != selected) {
            continue;
        }
        for size in sizes {
            let repeats = if probe == "frontend" { 1 } else { 7 };
            let mut samples: std::collections::BTreeMap<String, Vec<u64>> =
                std::collections::BTreeMap::new();
            for repeat in 0..repeats {
                let mut command = Command::new("time");
                command.args(["-v", "--"]).arg(&executable);
                if probe != "frontend" {
                    command.arg(size.to_string());
                }
                command.arg(mode);
                let result = compiler_sample(&mut command, probe, SEMANTIC_PROBE_TIMEOUT)?;
                let (stdout, rejected) = save_compiler_sample(
                    &directory,
                    &mut records,
                    json!({"mode":mode,"n":size,"repeat":repeat}),
                    result,
                )?;
                if rejected {
                    break;
                }
                if probe != "frontend" {
                    let mut lines = stdout.lines();
                    let header = lines.next().context("missing CSV header")?;
                    let row = lines.next().context("missing CSV sample")?;
                    ensure!(
                        header.split(',').count() == row.split(',').count(),
                        "CSV field count"
                    );
                    for (field, value) in header.split(',').zip(row.split(',')) {
                        if field != "mode" {
                            samples
                                .entry(field.to_owned())
                                .or_default()
                                .push(value.parse()?);
                        }
                    }
                }
            }
            let mut medians = serde_json::Map::new();
            for (field, mut values) in samples {
                values.sort_unstable();
                let mid = values.len() / 2;
                let value = if values.len() % 2 == 1 {
                    values[mid] as f64
                } else {
                    values[mid - 1] as f64 / 2.0 + values[mid] as f64 / 2.0
                };
                medians.insert(field, json!(value));
            }
            summaries.push(json!({"mode":mode,"n":size,"medians":medians}));
        }
    }
    fs::write(
        directory.join("medians.json"),
        serde_json::to_string_pretty(&summaries)? + "\n",
    )?;
    let environment = json!({"revision":output(Command::new("git").args(["rev-parse","HEAD"]).current_dir(root()))?.trim(),"rustc":output(Command::new("rustc").arg("--version"))?.trim(),"os":std::env::consts::OS,"scope":"compiler/verification/codec only; no bytecode dispatch or host calls; GNU time measures each probe child separately"});
    fs::write(
        directory.join("environment.json"),
        serde_json::to_string_pretty(&environment)? + "\n",
    )?;
    println!("{}", directory.display());
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn semantic_sample_keeps_thirty_second_policy_and_timeout_failure_evidence() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        assert_eq!(SEMANTIC_PROBE_TIMEOUT, Duration::from_secs(30));
        let directory = artifacts().unwrap().join(format!(
            "semantic-timeout-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let pidfile = directory.join("descendant.pid");
        let result = compiler_sample(
            Command::new("/bin/sh")
                .args([
                    "-c",
                    "printf 'partial'; printf '\\377err' >&2; sleep 30 & echo $! > \"$1\"; wait",
                    "mock",
                ])
                .arg(&pidfile),
            "semantic",
            Duration::from_millis(200),
        )
        .unwrap();
        assert!(result.timed_out);
        assert_eq!(crate::process::return_code(result.status), Some(-9));
        let mut records = Vec::new();
        assert!(
            save_compiler_sample(
                &directory,
                &mut records,
                json!({"mode":"mock","n":1,"repeat":0}),
                result
            )
            .is_err()
        );
        let samples: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("samples.json")).unwrap()).unwrap();
        assert_eq!(samples[0]["timed_out"], true);
        assert_eq!(samples[0]["exit"], -9);
        assert_eq!(
            fs::read(directory.join(samples[0]["stdout_raw"].as_str().unwrap())).unwrap(),
            b"partial"
        );
        assert_eq!(
            fs::read(directory.join(samples[0]["stderr_raw"].as_str().unwrap())).unwrap(),
            b"\xfferr"
        );
        let pid = fs::read_to_string(pidfile).unwrap();
        if let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", pid.trim())) {
            assert_eq!(
                stat.split(") ").nth(1).unwrap().chars().next(),
                Some('Z'),
                "probe descendant still running"
            );
        }
        // A non-timeout failure must also persist its original exit and bytes.
        let result = compiler_sample(
            Command::new("/bin/sh").args(["-c", "printf 'failed'; exit 7"]),
            "semantic",
            SEMANTIC_PROBE_TIMEOUT,
        )
        .unwrap();
        assert!(!result.timed_out);
        assert!(
            save_compiler_sample(
                &directory,
                &mut records,
                json!({"mode":"mock","n":1,"repeat":1}),
                result
            )
            .is_err()
        );
        assert_eq!(records[1]["exit"], 7);
        assert_eq!(records[1]["stdout"], "failed");
    }
}
