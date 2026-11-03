//! A small view of existing frame/render reports; detailed observations stay intact.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const LIMIT: usize = 3;

/// Write a concise summary beside the existing reports, after their validation.
pub(super) fn report(directory: &Path) -> Result<()> {
    let read = |name| -> Result<Value> {
        let path = directory.join(name);
        serde_json::from_slice(
            &fs::read(&path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("parse {}", path.display()))
    };
    let frames = read("frames.json")?;
    let render = directory
        .join("render.json")
        .is_file()
        .then(|| read("render.json"))
        .transpose()?;
    let summary = summarize(&frames, render.as_ref())?;
    fs::write(
        directory.join("summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    print(&summary);
    Ok(())
}

fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn stats(mut values: Vec<u64>) -> Value {
    values.sort_unstable();
    if values.is_empty() {
        return json!({"samples":0});
    }
    let p = |percent: usize| values[(values.len() * percent).div_ceil(100) - 1];
    json!({"samples":values.len(),"p50":p(50),"p95":p(95),"max":values.last()})
}

fn duration(metric: &Value) -> Value {
    if metric["samples"].as_u64().unwrap_or(0) == 0 {
        return Value::Null;
    }
    json!({"samples":metric["samples"],"p50_us":metric["p50_us"],
        "p95_us":metric["p95_us"],"max_us":metric["max_us"]})
}

fn submission_fps(group: &Value) -> Option<f64> {
    let totals = &group["submission_interval_totals"];
    let span = totals["span_us"].as_u64()?;
    let count = totals["present_calls"].as_u64()?;
    (span != 0 && count != 0 && Some(count) == group["eligible_intervals"].as_u64())
        .then_some(1_000_000. * count as f64 / span as f64)
}

fn main_thread_wall(group: &Value) -> Value {
    let ledger = &group["exclusive_wall"];
    let count = ledger["valid_intervals"].as_u64().unwrap_or(0);
    let mut phases = Vec::new();
    if count != 0
        && let Some(metrics) = ledger["metrics"].as_object()
    {
        for (key, metric) in metrics {
            let Some(name) = key
                .strip_prefix("wall_")
                .and_then(|key| key.strip_suffix("_ns"))
            else {
                continue;
            };
            if ["span", "unscoped", "lost"].contains(&name) {
                continue;
            }
            if let Some(total) = metric["total"].as_u64().filter(|total| *total != 0) {
                phases.push(json!({"phase":name,"mean_us_per_interval":total as f64 / count as f64 / 1000.}));
            }
        }
        phases.sort_by(|a, b| {
            b["mean_us_per_interval"]
                .as_f64()
                .unwrap()
                .total_cmp(&a["mean_us_per_interval"].as_f64().unwrap())
        });
        phases.truncate(LIMIT);
        return json!({"measurement":"exclusive_wall_including_waits_not_cpu_execution",
            "valid_intervals":count,"top_phases":phases,
            "unscoped_mean_us_per_interval":ledger["metrics"]["wall_unscoped_ns"]["total"].as_u64().map(|n| n as f64 / count as f64 / 1000.)});
    }
    if let Some(metrics) = group["metrics"].as_object() {
        for (key, metric) in metrics {
            if !["span_us", "active_us", "max_loop_us"].contains(&key.as_str())
                && key.ends_with("_us")
                && metric["p95_us"].as_u64().unwrap_or(0) != 0
            {
                phases
                    .push(json!({"phase":key.trim_end_matches("_us"),"wall_us":duration(metric)}));
            }
        }
    }
    phases.sort_by_key(|value| std::cmp::Reverse(value["wall_us"]["p95_us"].as_u64().unwrap_or(0)));
    phases.truncate(LIMIT);
    json!({"measurement":"inclusive_nested_wall_individual_phases_not_additive",
        "valid_intervals":0,"top_phases":phases})
}

fn sampled_cpu(group: &Value) -> Value {
    let mut phases = Vec::new();
    if let Some(observed) = group["thread_runtime"]["phases"].as_object() {
        for (name, phase) in observed {
            if phase["status"] == "paired_observations" && phase["accounting_errors"] == 0 {
                phases.push(json!({"phase":name,"sample_stride":phase["sample_stride"],
                    "samples":phase["totals"]["samples_count"],"run_us":phase["totals"]["run_us"],
                    "wall_us":phase["totals"]["wall_us"]}));
            }
        }
    }
    json!({"status":if phases.is_empty() {"unavailable"} else {"paired_samples"},
        "measurement":"raw paired samples; different sampling strides are not added or extrapolated",
        "phases":phases})
}

fn scene(group: &Value, setup: bool) -> Value {
    json!({"label":group["label"],"segment":group["segment"],
        "scope":if setup {"capture_setup"} else {"normal_playback"},
        "eligible_intervals":group["eligible_intervals"],"sample_size":group["sample_size"],
        "excluded_boundary_intervals":group["partial_or_multi_present"],
        "observed_submission_fps":if setup {None} else {submission_fps(group)},
        "submission_intervals_us":duration(&group["metrics"]["span_us"]),
        "main_thread_wall":main_thread_wall(group),"sampled_cpu_execution":sampled_cpu(group),
        "targets":{"created_pixel_owners":null,"pool_lookup_hits":null,
            "creation_wall_us":duration(&group["metrics"]["target_create_us"]),
            "counts":"unavailable_in_frame_summary"}})
}

fn metadata_count(frames: &Value, key: &str) -> Option<u64> {
    frames["trace_metadata"]
        .as_str()?
        .split('\t')
        .find_map(|item| {
            let (name, value) = item.split_once('=')?;
            (name == key).then(|| value.parse().ok()).flatten()
        })
}

fn summarize(frames: &Value, render: Option<&Value>) -> Result<Value> {
    ensure!(
        frames["schema"] == 1
            && frames["measurement"] == "present_submission_intervals_and_nested_wall_time",
        "unsupported frame summary schema"
    );
    let groups = frames["groups"]
        .as_array()
        .context("frame groups missing")?;
    let frozen = render
        .map(|value| fixed_frame(value, &frames["declared_environment"]))
        .transpose()?;
    let mut gaps = BTreeSet::<String>::new();
    for (key, description) in [
        ("dropped", "frame_rows_dropped"),
        (
            "discarded_segment_loops",
            "segment_boundary_loops_discarded",
        ),
        ("pending_loops", "unpresented_tail_loops"),
    ] {
        if let Some(count) = metadata_count(frames, key).filter(|count| *count != 0) {
            gaps.insert(format!("{description}={count}"));
        }
    }
    let mut scenes: Vec<_> = groups
        .iter()
        .filter(|group| group["eligible_intervals"].as_u64().unwrap_or(0) != 0)
        .map(|group| scene(group, render.is_some()))
        .collect();
    for group in groups {
        if group["eligible_intervals"].as_u64().unwrap_or(0) == 0 {
            continue;
        }
        if render.is_none() && submission_fps(group).is_none() {
            gaps.insert("submission_fps_unavailable_without_interval_totals".into());
        }
        let ledger = &group["exclusive_wall"];
        if ["closure_errors", "stack_errors", "lost_ns"]
            .iter()
            .any(|key| ledger[*key].as_u64().unwrap_or(0) != 0)
        {
            gaps.insert("exclusive_wall_rejected_intervals_present".into());
        }
    }
    if scenes.is_empty() {
        gaps.insert("no_complete_single_present_intervals".into());
    }
    if scenes
        .iter()
        .all(|value| value["sampled_cpu_execution"]["status"] == "unavailable")
    {
        gaps.insert("cpu_execution_time_unavailable".into());
    }
    if render.is_none() {
        gaps.insert("target_reuse_and_creation_counts_unavailable".into());
    }
    scenes.sort_by(|a, b| {
        match (
            a["observed_submission_fps"].as_f64(),
            b["observed_submission_fps"].as_f64(),
        ) {
            (Some(a), Some(b)) => a.total_cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            _ => b["submission_intervals_us"]["p95_us"]
                .as_u64()
                .cmp(&a["submission_intervals_us"]["p95_us"].as_u64()),
        }
    });
    let scene_count = scenes.len();
    Ok(
        json!({"schema":1,"valid":true,"declared_environment":frames["declared_environment"],
        "mode":if render.is_some() {"fixed_frame"} else {"normal_playback"},
        "fps_measurement":"complete single-presentation submission count divided by their accumulated intervals; display cadence is unmeasured",
        "gpu_execution_time_us":null,"display_fps":null,
        "scene_count":scene_count,"scenes":scenes,"fixed_frame":frozen,
        "sampling_gaps":gaps,"details":{"frames":"frames.json","render":render.map(|_| "render.json")}}),
    )
}

fn fixed_frame(render: &Value, environment: &Value) -> Result<Value> {
    ensure!(
        render["schema"] == 1
            && render["measurement"] == "fixed_frame_submission_and_completion_observation",
        "unsupported render summary schema"
    );
    ensure!(
        render["valid"] == true && render["completed"] == true,
        "fixed-frame trace is incomplete or invalid; see render.json"
    );
    ensure!(
        &render["declared_environment"] == environment,
        "frame/render environments disagree"
    );
    let repetitions = render["repetitions"]
        .as_array()
        .context("fixed-frame repetitions missing")?;
    ensure!(
        !repetitions.is_empty() && repetitions.iter().all(|value| value["valid"] == true),
        "fixed-frame repetition is invalid"
    );
    ensure!(
        repetitions
            .iter()
            .all(|value| matches!(value["sample"]["mode"].as_str(), Some("batched" | "serial"))),
        "unknown fixed-frame mode"
    );
    Ok(
        json!({"title":render["title"],"cache_policy":render["metadata"]["cache"],
        "measurement":"render-only replay of one immutable accepted frame",
        "batched":mode(repetitions, "batched")?,"serial_attribution":mode(repetitions, "serial")?,
        "gpu_execution_time_us":null,"fragment_count":null,"texture_fetch_count":null,
        "resource_pixels_identity":render["metadata"]["resource_pixels_identity"],
        "accounting":"each pass scope stands alone; native child call wall is not added; serial drains change batching"}),
    )
}

fn mode(repetitions: &[Value], name: &str) -> Result<Value> {
    let rows: Vec<_> = repetitions
        .iter()
        .filter(|value| value["sample"]["mode"] == name)
        .collect();
    if rows.is_empty() {
        return Ok(Value::Null);
    }
    let times = |field: &str| -> Result<Value> {
        Ok(stats(
            rows.iter()
                .map(|row| {
                    number(&row["sample"][field])
                        .with_context(|| format!("missing fixed-frame {field}"))
                })
                .collect::<Result<_>>()?,
        ))
    };
    let mut effects = BTreeMap::<String, (u128, u64)>::new();
    for row in &rows {
        for pass in row["passes"]
            .as_array()
            .context("fixed-frame pass groups missing")?
        {
            if pass["label"] != "effect" && pass["label"] != "effect-diagonal" {
                continue;
            }
            let identity = pass["definition"]
                .as_str()
                .context("effect identity missing")?;
            let total = effects.entry(identity.into()).or_default();
            total.0 += u128::from(
                pass["inclusive_scope_wall_ns"]
                    .as_u64()
                    .context("effect scope wall missing")?,
            );
            total.1 += pass["calls"].as_u64().context("effect calls missing")?;
        }
    }
    let mut effects: Vec<_> = effects.into_iter().map(|(definition, (wall, calls))| json!({
        "definition":definition,"calls_total":calls,"mean_pass_scope_wall_us_per_repetition":wall as f64 / rows.len() as f64 / 1000.
    })).collect();
    effects.sort_by(|a, b| {
        b["mean_pass_scope_wall_us_per_repetition"]
            .as_f64()
            .unwrap()
            .total_cmp(
                &a["mean_pass_scope_wall_us_per_repetition"]
                    .as_f64()
                    .unwrap(),
            )
    });
    effects.truncate(LIMIT);
    Ok(
        json!({"repetitions":rows.len(),"submit_wall_us":times("submit_wall_us")?,
        "completion_tail_us":times("completion_tail_us")?,"total_wall_us":times("total_wall_us")?,
        "top_effects":effects,"effect_measurement":if name == "serial" {"serial inclusive pass wall including completion drains"} else {"batched CPU submission scope wall including internal waits"},
        "targets":target_activity(&rows)?}),
    )
}

fn target_activity(repetitions: &[&Value]) -> Result<Value> {
    let mut counts = BTreeMap::<&str, u64>::new();
    let mut capacities = Vec::new();
    for row in repetitions {
        let mut owners = BTreeMap::new();
        for event in row["events"]
            .as_array()
            .context("fixed-frame events missing")?
        {
            if event["status"] != "ok" {
                continue;
            }
            let kind = event["kind"].as_str().unwrap_or("");
            let label = event["label"].as_str().unwrap_or("");
            let counter = match (kind, label) {
                ("target_cache", "ordinary-hit") => Some("ordinary_pool_lookup_hits"),
                ("target_cache", "macro-hit") => Some("macro_pool_lookup_hits"),
                ("source_reuse", "same-frame-source") => Some("same_frame_source_reuses"),
                ("target_acquire", "new" | "macro-new") => Some("created_pixel_owners"),
                ("target_acquire", "reshape") => Some("reshaped_views"),
                ("target_acquire", "scratch-acquired") => Some("acquired_scratch_views"),
                ("target_acquire", "lease-begin") => Some("writable_leases"),
                _ => None,
            };
            if let Some(counter) = counter {
                *counts.entry(counter).or_default() += 1;
            }
            if kind == "target_acquire" && label == "lease-begin" {
                let owner = event["output_target"]
                    .as_u64()
                    .filter(|owner| *owner != 0)
                    .context("lease owner missing")?;
                let bytes = event["bytes"].as_u64().context("lease capacity missing")?;
                if let Some(previous) = owners.insert(owner, bytes) {
                    ensure!(
                        previous == bytes,
                        "physical owner capacity changed within repetition"
                    );
                }
            }
        }
        capacities.push(owners.values().copied().sum());
    }
    for field in [
        "ordinary_pool_lookup_hits",
        "macro_pool_lookup_hits",
        "same_frame_source_reuses",
        "created_pixel_owners",
        "reshaped_views",
        "acquired_scratch_views",
        "writable_leases",
    ] {
        counts.entry(field).or_default();
    }
    Ok(
        json!({"totals":counts,"observed_lease_owner_bytes_per_repetition":stats(capacities),
        "accounting":"owner bytes deduplicated within each repetition; lookup hits can include optional replacements",
        "native_render_target_pool_creations":null}),
    )
}

fn print(summary: &Value) {
    println!(
        "Performance summary: {} / {}",
        summary["declared_environment"]
            .as_str()
            .unwrap_or("unknown environment"),
        summary["mode"].as_str().unwrap_or("unknown scope")
    );
    if summary["mode"] == "normal_playback" {
        for scene in summary["scenes"].as_array().into_iter().flatten() {
            let rate = scene["observed_submission_fps"]
                .as_f64()
                .map(|n| format!("{n:.2} submission FPS"))
                .unwrap_or_else(|| "submission FPS unavailable".into());
            println!(
                "  {}: {rate}; p95 interval {:.2} ms ({} intervals)",
                scene["label"].as_str().unwrap_or("scene"),
                scene["submission_intervals_us"]["p95_us"]
                    .as_f64()
                    .unwrap_or(0.)
                    / 1000.,
                scene["eligible_intervals"]
            );
        }
    } else {
        let fixed = &summary["fixed_frame"];
        for (key, label) in [
            ("batched", "Fixed-frame batched replay"),
            ("serial_attribution", "Serial attribution"),
        ] {
            let mode = &fixed[key];
            if mode.is_null() {
                continue;
            }
            println!(
                "  {label}: p50 submit {:.2} ms, tail {:.2} ms, total {:.2} ms",
                mode["submit_wall_us"]["p50"].as_f64().unwrap_or(0.) / 1000.,
                mode["completion_tail_us"]["p50"].as_f64().unwrap_or(0.) / 1000.,
                mode["total_wall_us"]["p50"].as_f64().unwrap_or(0.) / 1000.
            );
            println!(
                "    Targets across {} repetitions: {} ordinary hits, {} macro hits, {} shared sources, {} new pixel owners, {} reshaped views",
                mode["repetitions"],
                mode["targets"]["totals"]["ordinary_pool_lookup_hits"],
                mode["targets"]["totals"]["macro_pool_lookup_hits"],
                mode["targets"]["totals"]["same_frame_source_reuses"],
                mode["targets"]["totals"]["created_pixel_owners"],
                mode["targets"]["totals"]["reshaped_views"]
            );
            if key == "serial_attribution" {
                let effects = mode["top_effects"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|effect| {
                        let definition = effect["definition"].as_str().unwrap_or("unknown");
                        format!(
                            "{} {:.2} ms",
                            &definition[..definition.len().min(12)],
                            effect["mean_pass_scope_wall_us_per_repetition"]
                                .as_f64()
                                .unwrap_or(0.)
                                / 1000.
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                if !effects.is_empty() {
                    println!("    Effect pass wall per repetition: {effects}");
                }
            }
        }
    }
    if let Some(scene) = summary["scenes"]
        .as_array()
        .and_then(|scenes| scenes.first())
    {
        let phases = scene["main_thread_wall"]["top_phases"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|phase| {
                let time = phase["mean_us_per_interval"]
                    .as_f64()
                    .or_else(|| phase["wall_us"]["p95_us"].as_f64())
                    .unwrap_or(0.);
                format!(
                    "{} {:.2} ms",
                    phase["phase"].as_str().unwrap_or("unknown"),
                    time / 1000.
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        if !phases.is_empty() {
            println!(
                "  Main-thread wall [{}; {}; {}]: {phases}",
                scene["label"].as_str().unwrap_or("scene"),
                scene["scope"].as_str().unwrap_or("unknown scope"),
                if scene["main_thread_wall"]["measurement"]
                    == "exclusive_wall_including_waits_not_cpu_execution"
                {
                    "exclusive means"
                } else {
                    "individual nested p95"
                }
            );
        }
    }
    let gaps = summary["sampling_gaps"]
        .as_array()
        .into_iter()
        .flatten()
        .take(LIMIT)
        .filter_map(Value::as_str)
        .map(|gap| gap.replace('_', " "))
        .collect::<Vec<_>>()
        .join("; ");
    println!(
        "  GPU execution time unmeasured. Gaps: {}. Details: summary.json, frames.json{}",
        if gaps.is_empty() {
            "none in retained samples"
        } else {
            &gaps
        },
        if summary["fixed_frame"].is_null() {
            ""
        } else {
            ", render.json"
        }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames() -> Value {
        json!({"schema":1,"declared_environment":"host","measurement":"present_submission_intervals_and_nested_wall_time", "trace_metadata":"frame_metadata\tversion=1\tdropped=0",
            "groups":[{"label":"scene","segment":1,"eligible_intervals":3,"partial_or_multi_present":1,
                "submission_interval_totals":{"span_us":1_000_000,"present_calls":3},
                "metrics":{"span_us":{"samples":3,"p50_us":10_000,"p95_us":980_000,"max_us":980_000},
                    "active_us":{"samples":3,"p50_us":1,"p95_us":1}, "session_us":{"samples":3,"p50_us":900,"p95_us":900}, "vm_slice_us":{"samples":3,"p50_us":800,"p95_us":800}},
                "exclusive_wall":{"valid_intervals":3,"closure_errors":0,"stack_errors":0,"lost_ns":0,
                    "metrics":{"wall_span_ns":{"total":1_000_000_000}, "wall_session_ns":{"total":300_000},"wall_vm_slice_ns":{"total":600_000},"wall_unscoped_ns":{"total":999_100_000}}}}]})
    }

    fn render() -> Value {
        let repetition = |mode, wall| {
            json!({"valid":true,
            "sample":{"mode":mode,"submit_wall_us":"10","completion_tail_us":"20","total_wall_us":"30"},
            "passes":[{"label":if mode == "serial" { "effect-diagonal" } else { "effect" },"definition":"effect-a","inclusive_scope_wall_ns":wall,"known_operation_wall_ns":999_000,"calls":1},
                {"label":"native-draw","definition":"effect-a","inclusive_scope_wall_ns":wall,"calls":1}],
            "events":[{"kind":"target_acquire","label":"lease-begin","status":"ok","output_target":7,"bytes":4096},
                {"kind":"target_acquire","label":"lease-begin","status":"ok","output_target":7,"bytes":4096},
                {"kind":"target_cache","label":"ordinary-hit","status":"ok"},
                {"kind":"source_reuse","label":"same-frame-source","status":"ok"}]})
        };
        json!({"schema":1,"declared_environment":"host","measurement":"fixed_frame_submission_and_completion_observation","valid":true,"completed":true,"title":"captured scene", "metadata":{"cache":"replay"},"repetitions":[repetition("batched",100_000),repetition("serial",200_000)]})
    }

    #[test]
    fn submission_fps_uses_accumulated_intervals_and_keeps_stalls_visible() {
        let summary = summarize(&frames(), None).unwrap();
        let scene = &summary["scenes"][0];
        assert_eq!(scene["observed_submission_fps"], 3.0);
        assert_eq!(scene["submission_intervals_us"]["p95_us"], 980_000);
        let mut legacy = frames();
        legacy["groups"][0]
            .as_object_mut()
            .unwrap()
            .remove("submission_interval_totals");
        let old = summarize(&legacy, None).unwrap();
        assert!(old["scenes"][0]["observed_submission_fps"].is_null());
        assert_eq!(
            old["scenes"][0]["submission_intervals_us"]["p50_us"],
            10_000
        );
    }

    #[test]
    fn exclusive_costs_ignore_nested_inclusive_totals_and_preserve_cpu_unknown() {
        let summary = summarize(&frames(), None).unwrap();
        let scene = &summary["scenes"][0];
        assert_eq!(
            scene["main_thread_wall"]["top_phases"][0]["phase"],
            "vm_slice"
        );
        assert_eq!(
            scene["main_thread_wall"]["top_phases"][0]["mean_us_per_interval"],
            200.0
        );
        assert_eq!(
            scene["main_thread_wall"]["top_phases"][1]["mean_us_per_interval"],
            100.0
        );
        assert!(scene["main_thread_wall"].get("total_cpu_us").is_none());
        assert_eq!(scene["sampled_cpu_execution"]["status"], "unavailable");
        assert!(summary["gpu_execution_time_us"].is_null());
    }

    #[test]
    fn fixed_setup_has_no_fps_and_modes_keep_parent_and_native_costs_separate() {
        let summary = summarize(&frames(), Some(&render())).unwrap();
        assert!(summary["scenes"][0]["observed_submission_fps"].is_null());
        assert_eq!(summary["scenes"][0]["scope"], "capture_setup");
        let fixed = &summary["fixed_frame"];
        assert_eq!(
            fixed["batched"]["top_effects"][0]["mean_pass_scope_wall_us_per_repetition"],
            100.0
        );
        assert_eq!(
            fixed["serial_attribution"]["top_effects"][0]["mean_pass_scope_wall_us_per_repetition"],
            200.0
        );
        assert_eq!(
            fixed["batched"]["targets"]["observed_lease_owner_bytes_per_repetition"]["p50"],
            4096
        );
        assert_eq!(
            fixed["batched"]["targets"]["totals"]["created_pixel_owners"],
            0
        );
        assert_eq!(
            fixed["batched"]["targets"]["totals"]["same_frame_source_reuses"],
            1
        );
    }

    #[test]
    fn invalid_trace_is_rejected_and_every_scene_remains_in_the_report() {
        let mut invalid = render();
        invalid["valid"] = false.into();
        assert!(summarize(&frames(), Some(&invalid)).is_err());
        invalid = render();
        invalid["repetitions"][1]["valid"] = false.into();
        assert!(summarize(&frames(), Some(&invalid)).is_err());
        let mut many = frames();
        let group = many["groups"][0].clone();
        many["groups"] = json!(vec![group; 8]);
        assert_eq!(
            summarize(&many, None).unwrap()["scenes"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
    }
}
