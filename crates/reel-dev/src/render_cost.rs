//! Read fixed-frame diagnostics without conflating wall intervals and GPU time.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

type Fields = BTreeMap<String, String>;

#[derive(Default)]
struct Block {
    metadata: Fields,
    events: Vec<Fields>,
    closed: bool,
    sample: Option<Fields>,
}

#[derive(Default)]
struct PassTotals {
    calls: u64,
    scope_wall_ns: u64,
    operation_wall_ns: u64,
    operation_calls: u64,
    scissor_pixels: u64,
    scissor_calls: u64,
}

struct ActivePass {
    started: u64,
    parent: u64,
    group: (String, String),
    nested_same_group: bool,
}

fn pairs(fields: &[&str]) -> Fields {
    fields
        .iter()
        .filter_map(|field| field.split_once('='))
        .map(|(key, value)| (key.into(), value.into()))
        .collect()
}

fn number(fields: &Fields, key: &str) -> Result<u64> {
    fields
        .get(key)
        .with_context(|| format!("missing {key}"))?
        .parse()
        .with_context(|| format!("invalid {key}"))
}

fn row(columns: &[String], fields: &[&str]) -> Result<Fields> {
    ensure!(
        !columns.is_empty() && columns.len() == fields.len(),
        "diagnostic row width"
    );
    Ok(columns
        .iter()
        .cloned()
        .zip(fields.iter().map(|v| (*v).to_owned()))
        .collect())
}

fn event_json(event: &Fields) -> Result<Value> {
    let known = number(event, "known")?;
    let groups: &[(u64, &[&str])] = &[
        (1, &["logical_w", "logical_h"]),
        (2, &["storage_w", "storage_h"]),
        (4, &["source_w", "source_h"]),
        (8, &["output_w", "output_h"]),
        (16, &["scissor_x", "scissor_y", "scissor_w", "scissor_h"]),
        (32, &["write_x", "write_y", "write_w", "write_h"]),
        (64, &["store_x", "store_y", "store_w", "store_h"]),
        (128, &["valid_w", "valid_h"]),
        (256, &["operation_wall_ns"]),
        (512, &["error_code"]),
        (1024, &["stride_bytes"]),
        (2048, &["target_format"]),
        (4096, &["storage_kind"]),
        (8192, &["texture_bindings"]),
    ];
    let mut output = serde_json::Map::new();
    for (key, value) in event {
        let unknown = groups
            .iter()
            .any(|(bit, keys)| known & bit == 0 && keys.contains(&key.as_str()));
        let value = if unknown {
            Value::Null
        } else if ["kind", "label", "status", "definition"].contains(&key.as_str()) {
            json!(value)
        } else if key == "error_code" {
            json!(value.parse::<i64>()?)
        } else {
            json!(
                value
                    .parse::<u64>()
                    .with_context(|| format!("invalid event field {key}"))?
            )
        };
        output.insert(key.clone(), value);
    }
    Ok(Value::Object(output))
}

fn summarize(block: &Block) -> Result<Value> {
    let mut errors = Vec::new();
    let mut operation_failures = Vec::new();
    if !block.closed {
        errors.push("trace end missing".to_string());
    }
    for field in ["dropped", "active"] {
        if number(&block.metadata, field)? != 0 {
            errors.push(format!("trace {field} is nonzero"));
        }
    }
    if block.metadata.get("interrupted").map(String::as_str) != Some("false") {
        errors.push("trace recording interrupted".into());
    }
    if number(&block.metadata, "retained")? != block.events.len() as u64 {
        errors.push("trace retained count disagrees with rows".into());
    }
    let mut attempts = BTreeMap::new();
    let mut passes: BTreeMap<(u64, u64), ActivePass> = BTreeMap::new();
    let mut submissions = BTreeMap::new();
    let mut groups: BTreeMap<(String, String), PassTotals> = BTreeMap::new();
    let mut target_events: BTreeMap<String, u64> = BTreeMap::new();
    let mut previous = None;
    for event in &block.events {
        let seq = number(event, "sequence")?;
        let wall = number(event, "wall_ns")?;
        let attempt = number(event, "attempt_id")?;
        let pass = number(event, "pass_id")?;
        let submission = number(event, "submission_id")?;
        let frame = number(event, "frame_id")?;
        let kind = event.get("kind").context("missing event kind")?.as_str();
        let status = event
            .get("status")
            .context("missing event status")?
            .as_str();
        if let Some((last_seq, last_wall)) = previous {
            if seq != last_seq + 1 || wall < last_wall {
                errors.push(format!("event sequence/time discontinuity at {seq}"));
            }
        }
        previous = Some((seq, wall));
        if matches!(status, "error" | "timeout" | "aborted") {
            operation_failures.push(json!({"kind":kind,"attempt":attempt,"pass":pass,
                "label":event.get("label"),"status":status,"error_code":event.get("error_code")}));
        }
        match kind {
            "attempt_begin" => {
                if attempts.insert(attempt, (frame, wall)).is_some() {
                    errors.push("duplicate attempt".into());
                }
            }
            "attempt_end" => match attempts.remove(&attempt) {
                Some((f, t)) if f == frame && t <= wall => {}
                _ => errors.push(format!("unmatched attempt {attempt}")),
            },
            "pass_begin" => {
                if !attempts.contains_key(&attempt) {
                    errors.push(format!("pass outside attempt {attempt}"));
                }
                let parent = number(event, "parent_pass_id")?;
                if parent != 0 && !passes.contains_key(&(attempt, parent)) {
                    errors.push(format!("missing parent pass {parent}"));
                }
                let group = (event["label"].clone(), event["definition"].clone());
                let mut ancestor = parent;
                let mut nested_same_group = false;
                for _ in 0..passes.len() {
                    let Some(active) = passes.get(&(attempt, ancestor)) else {
                        break;
                    };
                    nested_same_group |= active.group == group;
                    ancestor = active.parent;
                }
                if passes
                    .insert(
                        (attempt, pass),
                        ActivePass {
                            started: wall,
                            parent,
                            group,
                            nested_same_group,
                        },
                    )
                    .is_some()
                {
                    errors.push("duplicate pass".into());
                }
            }
            "pass_end" => {
                let active = passes.remove(&(attempt, pass));
                let span = active
                    .as_ref()
                    .and_then(|active| wall.checked_sub(active.started));
                if span.is_none() {
                    errors.push(format!("unmatched pass {attempt}/{pass}"));
                }
                let group = groups
                    .entry((event["label"].clone(), event["definition"].clone()))
                    .or_default();
                // A logical paint scope can contain a same-named native draw.
                // Count its bounds and inclusive wall interval once; retain the
                // native operation measurement as a separate subset.
                let outer = active
                    .as_ref()
                    .is_some_and(|active| !active.nested_same_group);
                if outer {
                    group.calls += 1;
                    group.scope_wall_ns += span.unwrap_or(0);
                }
                let known = number(event, "known")?;
                if known & 256 != 0 {
                    group.operation_wall_ns += number(event, "operation_wall_ns")?;
                    group.operation_calls += 1;
                }
                if outer && known & 16 != 0 {
                    group.scissor_pixels += number(event, "scissor_w")?
                        .checked_mul(number(event, "scissor_h")?)
                        .context("scissor area overflow")?;
                    group.scissor_calls += 1;
                }
            }
            "submission" => {
                submissions.insert(submission, false);
            }
            "completion" => {
                if status == "ok" && submission != 0 {
                    if let Some(completed) = submissions.get_mut(&submission) {
                        *completed = true;
                    } else {
                        errors.push(format!("completion without submission {submission}"));
                    }
                }
            }
            "target_acquire" | "target_init" | "target_recycle" | "target_cache" => {
                *target_events
                    .entry(format!("{kind}/{}", event["label"]))
                    .or_default() += 1;
            }
            _ => {}
        }
    }
    if !attempts.is_empty() || !passes.is_empty() {
        errors.push("unfinished attempt/pass".into());
    }
    if submissions.is_empty() || submissions.values().any(|done| !done) {
        errors.push("unobserved final submission".into());
    }
    if block.sample.is_none() {
        errors.push("repetition sample missing".into());
    }
    let groups: Vec<_> = groups.into_iter().map(|((label, definition), total)|
        json!({"label":label,"definition":definition,"calls":total.calls,"inclusive_scope_wall_ns":total.scope_wall_ns,
            "known_operation_wall_ns":(total.operation_calls != 0).then_some(total.operation_wall_ns),
            "known_operation_calls":total.operation_calls,
            "scissor_bounds_pixels":(total.scissor_calls != 0).then_some(total.scissor_pixels),
            "known_scissor_calls":total.scissor_calls,"shaded_fragments":null})).collect();
    Ok(
        json!({"metadata":block.metadata,"sample":block.sample,"valid":errors.is_empty(),
        "errors":errors,"operation_failures":operation_failures,"passes":groups,"target_event_counts":target_events,
        "events":block.events.iter().map(event_json).collect::<Result<Vec<_>>>()?}),
    )
}

fn analyze(scope: &str, input: &str) -> Result<Value> {
    ensure!(
        ["vita", "emulator", "host"].contains(&scope),
        "environment must be vita, emulator or host"
    );
    let text = crate::frame_cost::normalize(input);
    let start = text
        .rfind("[reel-frozen-report] begin")
        .context("no fixed-frame report")?;
    let text = &text[start..];
    let end = text
        .find("[reel-frozen-report] end")
        .context("latest fixed-frame report is incomplete")?;
    let mut metadata = Fields::new();
    let mut title = String::new();
    let mut workload = Fields::new();
    let mut blocks: Vec<Block> = Vec::new();
    let mut columns = Vec::new();
    let mut sample_columns = Vec::new();
    let mut details = Vec::new();
    let mut complete = false;
    for line in text[..end].lines() {
        let fields: Vec<_> = line.split('\t').collect();
        match fields[0] {
            "frozen_metadata" => metadata = pairs(&fields[1..]),
            "frozen_title" => title = fields.get(1).unwrap_or(&"").to_string(),
            "frozen_workload" => workload = pairs(&fields[1..]),
            "frozen_columns" => sample_columns = fields[1..].iter().map(|v| (*v).into()).collect(),
            "trace_begin" => {
                ensure!(blocks.last().is_none_or(|b| b.closed), "nested trace block");
                blocks.push(Block::default());
                columns.clear();
            }
            "trace_metadata" => {
                let block = blocks.last_mut().context("trace metadata outside block")?;
                block.metadata = pairs(&fields[1..]);
                ensure!(
                    block.metadata.get("version").map(String::as_str) == Some("1"),
                    "unsupported trace version"
                );
            }
            "trace_columns" => columns = fields[1..].iter().map(|v| (*v).into()).collect(),
            "trace_event" => {
                let block = blocks.last_mut().context("trace event outside block")?;
                ensure!(!block.closed, "event after trace end");
                block.events.push(row(&columns, &fields[1..])?);
            }
            "trace_end" => {
                let block = blocks.last_mut().context("trace end outside block")?;
                ensure!(!block.closed, "duplicate trace end");
                for (key, value) in pairs(&fields[1..]) {
                    ensure!(
                        block.metadata.get(&key) == Some(&value),
                        "trace metadata/end disagree: {key}"
                    );
                }
                block.closed = true;
            }
            "frozen_sample" => {
                let block = blocks.last_mut().context("repetition without trace")?;
                ensure!(
                    block.closed && block.sample.is_none(),
                    "repetition trace boundary"
                );
                block.sample = Some(row(&sample_columns, &fields[1..])?);
            }
            "frozen_result" => complete = fields.get(1) == Some(&"complete"),
            token if token.starts_with("frozen_") => {
                details.push(fields.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>())
            }
            _ => {}
        }
    }
    ensure!(
        metadata.get("version").map(String::as_str) == Some("1"),
        "unsupported fixed-frame version"
    );
    let repetitions = number(&metadata, "repetitions")?;
    let blocks = blocks.iter().map(summarize).collect::<Result<Vec<_>>>()?;
    let valid = complete
        && repetitions == blocks.len() as u64
        && !blocks.is_empty()
        && blocks.iter().all(|b| b["valid"] == true);
    Ok(
        json!({"schema":1,"declared_environment":scope,"measurement":"fixed_frame_submission_and_completion_observation",
        "title":title,"metadata":metadata,"workload":workload,"valid":valid,"completed":complete,
        "gpu_service_time":null,"fragment_count":null,"texture_fetch_count":null,
        "phase_accounting":"same-group nested scopes count once; different groups still overlap; scissor area is an upper bound; native call wall is a separate subset including internal waits",
        "comparison_requirements":["same command identity and resource contents","same cache/serial/queue settings","same clocks and surface","complete trace without drops"],
        "repetitions":blocks,"auxiliary_rows":details}),
    )
}

/// Summarize the latest complete fixed-frame report, including trace integrity.
pub fn report(scope: &str, input: &Path, output: &Path) -> Result<Value> {
    let text = fs::read_to_string(input).context("read render probe log")?;
    let mut result = analyze(scope, &text)?;
    result["input"] = json!(input);
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, serde_json::to_vec_pretty(&result)?)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(dropped: u64) -> String {
        format!(
            "[reel-frozen-report] begin\nfrozen_metadata\tversion=1\trepetitions=1\nfrozen_columns\trepetition\ntrace_begin\tversion=1\ntrace_metadata\tversion=1\tretained=4\tdropped={dropped}\tactive=0\tinterrupted=false\ntrace_columns\tsequence\twall_ns\tframe_id\tattempt_id\tsubmission_id\tpass_id\tparent_pass_id\tkind\tlabel\tstatus\tknown\tscissor_w\tscissor_h\ntrace_event\t1\t10\t7\t1\t1\t0\t0\tattempt_begin\tframe\tpending\t0\t0\t0\ntrace_event\t2\t15\t7\t1\t1\t0\t0\tsubmission\tframe\tok\t0\t0\t0\ntrace_event\t3\t20\t7\t1\t1\t0\t0\tattempt_end\tframe\tok\t0\t0\t0\ntrace_event\t4\t30\t7\t1\t1\t0\t0\tcompletion\tframe\tok\t0\t0\t0\ntrace_end\tretained=4\tdropped={dropped}\tactive=0\tinterrupted=false\nfrozen_sample\t0\nfrozen_result\tcomplete\n[reel-frozen-report] end\n"
        )
    }

    #[test]
    fn unknown_bounds_remain_unknown_and_loss_invalidates_comparison() {
        let result = analyze("vita", &input(0)).unwrap();
        assert_eq!(result["valid"], true);
        assert!(result["repetitions"][0]["events"][0]["scissor_w"].is_null());
        assert_eq!(analyze("vita", &input(2)).unwrap()["valid"], false);
    }

    #[test]
    fn incomplete_new_capture_does_not_fall_back_to_an_old_success() {
        let text = input(0) + "[reel-frozen-report] begin\nfrozen_metadata\tversion=1\n";
        assert!(analyze("vita", &text).is_err());
    }

    #[test]
    fn missing_attempt_end_invalidates_a_complete_outer_report() {
        let text = input(0).replace("attempt_end", "target_cache");
        assert_eq!(analyze("vita", &text).unwrap()["valid"], false);
    }

    #[test]
    fn nested_native_paint_keeps_one_logical_area_and_separate_call_cost() {
        let mut events = Vec::new();
        for (kind, wall, pass, parent, known, label) in [
            ("attempt_begin", 0, 0, 0, 0, "frame"),
            ("pass_begin", 10, 1, 0, 16, "paint"),
            ("pass_begin", 20, 2, 1, 272, "paint"),
            ("pass_end", 30, 2, 1, 272, "paint"),
            ("pass_end", 40, 1, 0, 16, "paint"),
            ("submission", 50, 0, 0, 0, "frame"),
            ("attempt_end", 60, 0, 0, 0, "frame"),
            ("completion", 70, 0, 0, 0, "frame"),
        ] {
            events.push(pairs(&[
                &format!("sequence={}", events.len() + 1),
                &format!("wall_ns={wall}"),
                "frame_id=1",
                "attempt_id=1",
                "submission_id=1",
                &format!("pass_id={pass}"),
                &format!("parent_pass_id={parent}"),
                &format!("kind={kind}"),
                &format!("label={label}"),
                "definition=zero",
                "status=ok",
                &format!("known={known}"),
                "scissor_w=10",
                "scissor_h=10",
                "operation_wall_ns=10",
            ]));
        }
        let block = Block {
            metadata: pairs(&["dropped=0", "active=0", "interrupted=false", "retained=8"]),
            events,
            closed: true,
            sample: Some(Fields::new()),
        };
        let result = summarize(&block).unwrap();
        assert_eq!(result["valid"], true);
        let paint = &result["passes"][0];
        assert_eq!(paint["calls"], 1);
        assert_eq!(paint["inclusive_scope_wall_ns"], 30);
        assert_eq!(paint["scissor_bounds_pixels"], 100);
        assert_eq!(paint["known_operation_calls"], 1);
        assert_eq!(paint["known_operation_wall_ns"], 10);
    }
}
