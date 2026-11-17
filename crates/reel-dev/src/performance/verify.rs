//! Release acceptance for a target export and its recorded gameplay.
use super::{Manifest, collect, save};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    version: u32,
    target: String,
    environment: String,
    scenes: Vec<Scene>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scene {
    label: String,
    min_intervals: u64,
    max_p95_submit_us: Option<u64>,
    max_submit_us: Option<u64>,
    max_vm_bytes: Option<u64>,
    max_gpu_bytes: Option<u64>,
}

impl Policy {
    fn read(path: &Path) -> Result<Self> {
        let policy: Self = serde_json::from_slice(&fs::read(path)?)
            .with_context(|| format!("read release policy {}", path.display()))?;
        ensure!(policy.version == 1, "unsupported release policy version");
        reel_package::resource_target::ResourceTarget::parse(&policy.target)?;
        let environment_matches = if policy.target == "vita" {
            matches!(policy.environment.as_str(), "vita" | "emulator")
        } else {
            policy.environment == "host"
        };
        ensure!(
            environment_matches,
            "release environment must match its target"
        );
        ensure!(
            !policy.scenes.is_empty(),
            "release policy requires at least one scene"
        );
        let mut labels = BTreeSet::new();
        for scene in &policy.scenes {
            ensure!(
                !scene.label.trim().is_empty(),
                "scene label must not be empty"
            );
            ensure!(
                labels.insert(&scene.label),
                "duplicate scene {:?}",
                scene.label
            );
            ensure!(
                scene.min_intervals > 0,
                "scene {:?} needs a positive min_intervals",
                scene.label
            );
        }
        Ok(policy)
    }
}

pub(super) fn execute(directory: &Path, policy_path: &Path) -> Result<()> {
    evaluate(directory, policy_path, |_| Ok(()))
}

pub(super) fn evaluate(
    directory: &Path,
    policy_path: &Path,
    play: impl FnOnce(&Manifest) -> Result<()>,
) -> Result<()> {
    let policy = Policy::read(policy_path)?;
    let mut report = json!({
        "schema":1, "state":"failed", "phase":"export",
        "target":policy.target, "environment":policy.environment,
        "measurement":"complete presentation submission intervals",
        "scenes":[],
    });
    let result = verify(directory, &policy, &mut report, play);
    let artifacts: Vec<_> = [
        "build.log",
        "export.log",
        "player.log",
        "device.log",
        "transport.log",
        "reports/status.json",
        "reports/frames.tsv",
    ]
    .into_iter()
    .filter(|name| directory.join(name).is_file())
    .collect();
    report["artifacts"] = json!(artifacts);
    match &result {
        Ok(()) => report["state"] = json!("passed"),
        Err(error) => report["diagnostic"] = json!(format!("{error:#}")),
    }
    let path = directory.join("verification.json");
    save(&path, &report)?;
    println!(
        "Release verification: {} ({})",
        path.display(),
        if result.is_ok() { "passed" } else { "failed" }
    );
    result
}

fn verify(
    directory: &Path,
    policy: &Policy,
    report: &mut Value,
    play: impl FnOnce(&Manifest) -> Result<()>,
) -> Result<()> {
    let manifest = Manifest::read(directory).with_context(|| {
        fs::read_to_string(directory.join("export.log"))
            .ok()
            .and_then(|log| {
                log.lines()
                    .rev()
                    .find(|line| line.starts_with("error["))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "no completed target export".into())
    })?;
    ensure!(
        directory.join("game/game.asb").is_file(),
        "exported game.asb is missing"
    );
    report["phase"] = json!("target");
    ensure!(
        manifest.target == policy.target && manifest.environment == policy.environment,
        "run is {} / {}, policy requires {} / {}",
        manifest.target,
        manifest.environment,
        policy.target,
        policy.environment,
    );
    report["phase"] = json!("playback");
    ensure!(
        manifest.request.frame.is_none(),
        "release verification requires normal gameplay"
    );
    let run = play(&manifest);
    if let Err(error) = &run {
        // The player status may carry a more precise fault than the process exit.
        report["run_error"] = json!(format!("{error:#}"));
    }
    let summary = collect::analyze(directory, &manifest)
        .context("playback did not produce a complete report")?;
    report["phase"] = json!("scenes");
    let scenes = summary["scenes"]
        .as_array()
        .context("missing scene timing data")?;
    let playback = summary["playback_segments"]["segments"]
        .as_array()
        .context("missing playback totals")?;
    let checks: Vec<_> = policy
        .scenes
        .iter()
        .map(|scene| check_scene(scene, scenes, playback))
        .collect();
    let failed: Vec<_> = checks
        .iter()
        .filter(|check| check["passed"] == false)
        .filter_map(|check| check["label"].as_str())
        .collect();
    let failure = failed.join(", ");
    report["scenes"] = json!(checks);
    ensure!(
        failure.is_empty(),
        "scene verification failed for {failure}"
    );
    run
}

fn check_scene(policy: &Scene, scenes: &[Value], playback: &[Value]) -> Value {
    let mut reports = playback
        .iter()
        .filter(|scene| scene["label"] == policy.label);
    let mut timings = scenes.iter().filter(|scene| scene["label"] == policy.label);
    let (Some(playback), None) = (reports.next(), reports.next()) else {
        return failed_scene(
            policy,
            "required scene is missing or its label was used more than once",
        );
    };
    let (Some(scene), None) = (timings.next(), timings.next()) else {
        return failed_scene(
            policy,
            "required scene has no unique set of submission intervals",
        );
    };

    let intervals = &scene["submission_intervals_us"];
    let samples = intervals["samples"].as_u64().unwrap_or(0);
    let presents = playback["present_calls"].as_u64().unwrap_or(0);
    let mut issues = Vec::new();
    if samples.checked_add(1) != Some(presents) {
        issues
            .push(json!({"check":"complete_intervals","intervals":samples,"submissions":presents}));
    }
    if samples < policy.min_intervals {
        issues
            .push(json!({"check":"min_intervals","observed":samples,"limit":policy.min_intervals}));
    }
    let metrics = [
        (
            "max_p95_submit_us",
            &intervals["p95_us"],
            policy.max_p95_submit_us,
        ),
        ("max_submit_us", &intervals["max_us"], policy.max_submit_us),
        (
            "max_vm_bytes",
            &playback["vm_peak_bytes"],
            policy.max_vm_bytes,
        ),
        (
            "max_gpu_bytes",
            &playback["gpu_peak_bytes"],
            policy.max_gpu_bytes,
        ),
    ];
    for (check, value, limit) in metrics {
        if let Some(limit) = limit
            && value.as_u64().is_none_or(|observed| observed > limit)
        {
            issues.push(json!({"check":check,"observed":value,"limit":limit}));
        }
    }
    let hotspots = &scene["main_thread_wall"]["top_phases"];
    let failed = |check: &str| issues.iter().any(|issue| issue["check"] == check);
    let advice = if failed("max_vm_bytes") {
        Some("Inspect the retained script data, history and UI objects in this scene.")
    } else if failed("max_gpu_bytes") {
        Some("Inspect the texture sizes and offscreen targets retained in this scene.")
    } else if failed("max_p95_submit_us") || failed("max_submit_us") {
        hotspots
            .get(0)
            .and_then(|phase| phase["phase"].as_str())
            .and_then(advice)
    } else {
        None
    };
    json!({
        "label":policy.label, "passed":issues.is_empty(), "issues":issues,
        "intervals":intervals, "vm_peak_bytes":playback["vm_peak_bytes"], "gpu_peak_bytes":playback["gpu_peak_bytes"],
        "main_thread_wall":scene["main_thread_wall"], "advice":advice,
    })
}

fn failed_scene(policy: &Scene, reason: &str) -> Value {
    json!({"label":policy.label,"passed":false,"issues":[{"check":"scene","reason":reason}]})
}

fn advice(phase: &str) -> Option<&'static str> {
    match phase {
        "session_gc" | "vm_slice" | "ui_layout" | "ui_widget_sync" => {
            Some("Inspect repeated object allocation and full UI rebuilds in this scene.")
        }
        "asset_pump" | "image_coverage" | "image_convert" | "image_transfer" => {
            Some("Inspect first-use image sizes and repeated requests for the same art.")
        }
        "surface_acquire" | "present" | "render_fence" => {
            Some("Inspect presentation wait and frame pacing on the target device.")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::performance::{CachePolicy, FrameCaptureOptions, Request};

    fn policy() -> Scene {
        serde_json::from_value(json!({
            "label":"Gallery scroll","min_intervals":2,"max_p95_submit_us":17000,"max_vm_bytes":1024,
        }))
        .unwrap()
    }

    #[test]
    fn policy_rejects_a_different_target_and_fixed_frames_before_playing() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join("game")).unwrap();
        fs::write(root.join("game/game.asb"), b"").unwrap();
        let mut manifest = Manifest {
            schema: 1,
            target: "vita".into(),
            environment: "vita".into(),
            project: "project".into(),
            exporter: "reel".into(),
            request: Request {
                version: 1,
                package: "ux0:/game/game.asb".into(),
                saves: "ux0:/game/saves".into(),
                output: "ux0:/results/run".into(),
                vm_timing: false,
                source_atlas: None,
                frame: None,
            },
        };
        let policy = root.join("policy.json");
        save(
            &policy,
            &json!({
                "version":1, "target":"vita", "environment":"emulator",
                "scenes":[{"label":"History scroll","min_intervals":100}]
            }),
        )
        .unwrap();

        let reject = |manifest: &Manifest, phase: &str, message: &str| {
            save(&root.join("run.json"), manifest).unwrap();
            let error =
                evaluate(root, &policy, |_| panic!("invalid run reached playback")).unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
            let report: Value =
                serde_json::from_slice(&fs::read(root.join("verification.json")).unwrap()).unwrap();
            assert_eq!(report["state"], "failed");
            assert_eq!(report["phase"], phase);
        };
        reject(&manifest, "target", "policy requires vita / emulator");
        manifest.environment = "emulator".into();
        manifest.request.frame = Some(FrameCaptureOptions {
            label: "History scroll".into(),
            accepted_frame: 1,
            repetitions: 1,
            cache: CachePolicy::Replay,
            measure_passes: false,
            save_frame: false,
        });
        reject(&manifest, "playback", "requires normal gameplay");
    }

    #[test]
    fn checks_only_the_requested_scene_with_complete_intervals() {
        let scenes = [json!({
            "label":"Gallery scroll",
            "submission_intervals_us":{"samples":2,"p95_us":16500,"max_us":16500},
            "main_thread_wall":{"top_phases":[{"phase":"ui_layout"}]},
        })];
        let playback = [json!({"label":"Gallery scroll","present_calls":3,"vm_peak_bytes":900})];
        let mut policy = policy();
        let result = check_scene(&policy, &scenes, &playback);
        assert_eq!(result["passed"], true);

        policy.max_p95_submit_us = Some(16000);
        policy.max_vm_bytes = Some(800);
        let result = check_scene(&policy, &scenes, &playback);
        assert_eq!(result["passed"], false);
        assert_eq!(result["issues"][0]["observed"], 16500);
        assert_eq!(result["issues"][1]["observed"], 900);
        assert!(result["advice"].is_string());

        let incomplete = json!({"label":"Gallery scroll","present_calls":4,"vm_peak_bytes":900});
        let result = check_scene(&policy, &scenes, &[incomplete]);
        assert_eq!(result["issues"][0]["check"], "complete_intervals");
    }

    #[test]
    fn a_missing_or_ambiguous_scene_cannot_pass() {
        let playback = json!({"label":"Gallery scroll","present_calls":3});
        let result = check_scene(&policy(), &[], &[playback.clone(), playback]);
        assert_eq!(result["passed"], false);
        assert_eq!(result["issues"][0]["check"], "scene");
        assert_eq!(check_scene(&policy(), &[], &[])["passed"], false);
    }
}
