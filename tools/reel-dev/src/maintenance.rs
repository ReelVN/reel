use crate::{files, root, sha};
use anyhow::{Context, Result, bail, ensure};
use std::{
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

/// Check the single local UI source and both headless and enabled native closures.
pub fn dependencies() -> Result<()> {
    let metadata: serde_json::Value =
        serde_json::from_str(&output(Command::new("cargo").current_dir(root()).args([
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--all-features",
        ]))?)?;
    ensure!(
        metadata["workspace_root"].as_str() == root().to_str(),
        "workspace root changed"
    );
    validate_ui_metadata(&metadata)?;
    let tree = output(Command::new("cargo").current_dir(root()).args([
        "tree",
        "--locked",
        "-p",
        "reel-ui",
        "-p",
        "reel-render",
        "-p",
        "reel-player",
        "--no-default-features",
        "-e",
        "normal,build",
        "--prefix",
        "none",
    ]))?;
    ensure!(
        !tree.lines().any(|line| matches!(
            line.split_whitespace().next(),
            Some("winit" | "wgpu" | "masonry_winit")
        )),
        "native dependency entered default-off closure"
    );
    println!(
        "One local reel-ui; enabled UI closure and default-off UI/render/player have no reverse native dependencies"
    );
    Ok(())
}

fn validate_ui_metadata(metadata: &serde_json::Value) -> Result<()> {
    let packages = metadata["packages"].as_array().context("packages")?;
    for name in [
        "masonry",
        "masonry_core",
        "tree_arena",
        "linebender_include_doc_path",
        "reel-stage",
    ] {
        ensure!(
            !packages.iter().any(|p| p["name"] == name),
            "retired UI package {name}"
        );
    }
    for name in ["reel-ui", "imaging", "kurbo", "peniko", "parley"] {
        ensure!(
            packages.iter().filter(|p| p["name"] == name).count() == 1,
            "duplicate UI types: {name}"
        );
    }
    let ui = packages
        .iter()
        .find(|p| p["name"] == "reel-ui")
        .context("reel-ui")?;
    ensure!(
        ui["manifest_path"].as_str() == root().join("crates/reel-ui/Cargo.toml").to_str()
            && ui["source"].is_null()
            && metadata["workspace_members"]
                .as_array()
                .context("members")?
                .contains(&ui["id"]),
        "reel-ui must be the local workspace member"
    );
    let nodes = metadata["resolve"]["nodes"].as_array().context("nodes")?;
    let mut pending = vec![ui["id"].clone()];
    let mut visited = Vec::new();
    while let Some(id) = pending.pop() {
        if visited.contains(&id) {
            continue;
        }
        visited.push(id.clone());
        let package = packages
            .iter()
            .find(|p| p["id"] == id)
            .context("dependency package")?;
        ensure!(
            !matches!(
                package["name"].as_str(),
                Some(
                    "wgpu"
                        | "wgpu-core"
                        | "wgpu-hal"
                        | "winit"
                        | "masonry_winit"
                        | "reel-render"
                        | "reel-player"
                )
            ),
            "reverse native dependency in UI: {}",
            package["name"]
        );
        let node = nodes
            .iter()
            .find(|n| n["id"] == id)
            .context("dependency node")?;
        for dep in node["deps"].as_array().context("deps")? {
            if dep["dep_kinds"]
                .as_array()
                .context("dep kinds")?
                .iter()
                .any(|k| k["kind"] != "dev")
            {
                pending.push(dep["pkg"].clone());
            }
        }
    }
    Ok(())
}

/// Audit legal attribution and single ownership, without constraining local source layout.
pub fn audit_ui() -> Result<()> {
    let base = root().join("crates/reel-ui");
    // All imported UI package licenses at the accepted base were identical.
    ensure!(
        sha(&fs::read(base.join("LICENSE"))?)
            == "a6cba85bc92e0cff7a450b1d873c0eaa2e9fc96bf472df0247a26bec77bf3ff9",
        "original UI license changed"
    );
    let provenance = fs::read_to_string(base.join("ORIGIN.md"))?;
    ensure!(
        provenance.contains("271a27a6d4a930f7878d404f9014e3c50a3a9b88")
            && provenance.contains("Reel"),
        "missing UI provenance"
    );
    for path in files(&base)? {
        if path.starts_with(base.join("src")) && path.extension().is_some_and(|x| x == "rs") {
            let text = fs::read_to_string(&path)?;
            ensure!(
                !text.contains("masonry_core::")
                    && !text.contains("use masonry::")
                    && !text.contains("include_doc_path!")
                    && !text.contains("tree_arena_unsafe"),
                "retired UI source boundary: {}",
                path.display()
            );
            ensure!(
                text.contains("// Copyright")
                    && text.contains("// SPDX-License-Identifier: Apache-2.0"),
                "UI attribution lost: {}",
                path.display()
            );
        }
    }
    for name in [
        "masonry",
        "masonry_core",
        "tree_arena",
        "include_doc_path",
        "reel-stage",
    ] {
        ensure!(
            !root().join("crates").join(name).exists(),
            "duplicate UI source: {name}"
        );
    }
    dependencies()?;
    println!(
        "Original Apache license/copyright, local provenance and single UI ownership verified"
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
        "reel-ui"
    } else {
        bail!("measurement supports only UI contracts/r01_measurements")
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

    fn ui_metadata() -> serde_json::Value {
        let mut packages = Vec::new();
        let mut nodes = Vec::new();
        for name in ["reel-ui", "imaging", "kurbo", "peniko", "parley", "wgpu"] {
            packages.push(json!({"id": name, "name": name, "source": null,
                "manifest_path": root().join("crates").join(name).join("Cargo.toml")}));
            nodes.push(json!({"id": name, "deps": []}));
        }
        json!({"packages": packages, "workspace_members": ["reel-ui"], "resolve": {"nodes": nodes}})
    }

    #[test]
    fn ui_ownership_rejects_retired_duplicate_or_remote_packages() {
        let baseline = ui_metadata();
        validate_ui_metadata(&baseline).unwrap();
        for name in [
            "masonry",
            "masonry_core",
            "tree_arena",
            "linebender_include_doc_path",
            "reel-stage",
            "reel-ui",
            "kurbo",
        ] {
            let mut changed = baseline.clone();
            changed["packages"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name": name}));
            assert!(validate_ui_metadata(&changed).is_err(), "accepted {name}");
        }
        let mut remote = baseline;
        remote["packages"][0]["source"] = json!("registry+unexpected");
        assert!(validate_ui_metadata(&remote).is_err());
    }

    #[test]
    fn ui_native_closure_checks_transitive_normal_build_edges_and_ignores_dev_edges() {
        for kind in [serde_json::Value::Null, json!("build")] {
            let mut changed = ui_metadata();
            changed["resolve"]["nodes"][0]["deps"] =
                json!([{"pkg": "imaging", "dep_kinds": [{"kind": kind}]}]);
            changed["resolve"]["nodes"][1]["deps"] =
                json!([{"pkg": "wgpu", "dep_kinds": [{"kind": null}]}]);
            let error = validate_ui_metadata(&changed).unwrap_err().to_string();
            assert!(error.contains("reverse native dependency"), "{error}");
        }
        let mut changed = ui_metadata();
        changed["resolve"]["nodes"][0]["deps"] =
            json!([{"pkg": "wgpu", "dep_kinds": [{"kind": "dev"}]}]);
        validate_ui_metadata(&changed).unwrap();
    }

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
