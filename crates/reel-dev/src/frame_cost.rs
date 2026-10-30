//! Offline summaries of version-1 presentation-submission samples. Optional
//! phase limits are engineering targets supplied with the workload, not hardware
//! guarantees. Nested phase percentiles must be assessed separately, not summed.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

struct Frame {
    segment: u64,
    values: BTreeMap<String, u64>,
}
struct Trace {
    labels: BTreeMap<u64, String>,
    rows: Vec<Frame>,
    metadata: String,
}

fn derive_phases(values: &mut BTreeMap<String, u64>) {
    let names = [
        "session_us",
        "session_schedule_us",
        "vm_slice_us",
        "host_call_us",
        "session_gc_us",
        "session_activity_us",
    ];
    let [
        Some(session),
        Some(schedule),
        Some(vm),
        Some(host),
        Some(gc),
        Some(activity),
    ] = names.map(|key| values.get(key).copied())
    else {
        return;
    };
    // Derive each observation before taking percentiles; independent phase
    // percentiles can refer to different frames and cannot be subtracted.
    let other = schedule.checked_sub(vm);
    let known = schedule
        .checked_add(host)
        .and_then(|n| n.checked_add(gc))
        .and_then(|n| n.checked_add(activity));
    let residual = known.and_then(|known| session.checked_sub(known));
    values.insert("schedule_non_vm_us".into(), other.unwrap_or(0));
    values.insert("session_unattributed_us".into(), residual.unwrap_or(0));
    values.insert(
        "phase_hierarchy_errors".into(),
        u64::from(other.is_none() || residual.is_none()),
    );
}

fn normalize(text: &str) -> String {
    if !text.contains("*** TTY: ") {
        return text.into();
    }
    let mut output = String::new();
    for line in text.lines() {
        if let Some((_, fragment)) = line.split_once("*** TTY: ") {
            // Vita3K trims a newline when it shares a write with a literal, but
            // formats arguments as separate writes. Record prefixes recover
            // those boundaries; labels have already had tabs/newlines escaped.
            if [
                "[reel-frame-report]",
                "frame_metadata\t",
                "frame_segment\t",
                "frame_columns\t",
                "frame_sample\t",
            ]
            .iter()
            .any(|prefix| fragment.starts_with(prefix))
                && !output.ends_with('\n')
            {
                output.push('\n');
            }
            if fragment.is_empty() {
                output.push('\n');
            } else {
                output.push_str(fragment);
            }
        }
    }
    output
}
fn parse(text: &str) -> Result<Trace> {
    let text = normalize(text);
    let begin = text
        .rfind("[reel-frame-report] begin")
        .context("no frame report; use a frame-cost-enabled profiling build")?;
    let text = &text[begin..];
    let end = text
        .find("[reel-frame-report] end")
        .context("latest frame report is incomplete")?;
    let mut trace = Trace {
        labels: BTreeMap::new(),
        rows: Vec::new(),
        metadata: String::new(),
    };
    let mut columns = Vec::new();
    for line in text[..end].lines() {
        let fields: Vec<_> = line.split('\t').collect();
        match fields[0] {
            "frame_metadata" => {
                ensure!(
                    fields.contains(&"version=1"),
                    "unsupported frame report version"
                );
                trace.metadata = line.into();
            }
            "frame_segment" => {
                ensure!(fields.len() == 3, "segment width");
                trace.labels.insert(fields[1].parse()?, fields[2].into());
            }
            "frame_columns" => {
                columns = fields[1..].iter().map(|field| field.to_string()).collect()
            }
            "frame_sample" => {
                ensure!(
                    !columns.is_empty() && fields.len() == columns.len() + 1,
                    "frame sample width"
                );
                let mut values: BTreeMap<String, u64> = columns
                    .iter()
                    .cloned()
                    .zip(fields[1..].iter().map(|field| field.parse::<u64>()))
                    .map(|(key, value)| Ok((key, value?)))
                    .collect::<Result<_>>()?;
                for name in [
                    "segment",
                    "partial",
                    "present_calls",
                    "span_us",
                    "active_us",
                ] {
                    ensure!(values.contains_key(name), "missing {name}");
                }
                derive_phases(&mut values);
                trace.rows.push(Frame {
                    segment: values["segment"],
                    values,
                });
            }
            _ => {}
        }
    }
    ensure!(!trace.metadata.is_empty(), "missing version metadata");
    ensure!(!trace.rows.is_empty(), "no presentation samples");
    ensure!(
        trace
            .rows
            .iter()
            .all(|row| trace.labels.contains_key(&row.segment)),
        "unresolved frame segment"
    );
    Ok(trace)
}
fn statistics(mut values: Vec<u64>, limit: Option<u64>) -> Value {
    values.sort_unstable();
    if values.is_empty() {
        return json!({"samples":0});
    }
    let percentile = |p: usize| values[(values.len() * p).div_ceil(100) - 1];
    json!({"samples":values.len(),"p50_us":percentile(50),"p95_us":percentile(95),"p99_us":percentile(99),"max_us":values.last(),"limit_us":limit,"over_limit":limit.map(|limit|values.iter().filter(|&&value|value>limit).count())})
}

fn summarize(trace: &Trace, scope: &str, limits: &BTreeMap<String, u64>) -> Result<Value> {
    let columns = &trace.rows[0].values;
    for key in limits.keys() {
        ensure!(
            key.ends_with("_us") && key != "elapsed_us" && columns.contains_key(key),
            "unknown duration limit {key}"
        );
    }
    let groups: Vec<_> = trace.labels.iter().map(|(segment,label)| {
        let rows: Vec<_> = trace.rows.iter().filter(|row|row.segment==*segment).collect();
        let eligible: Vec<_> = rows.iter().filter(|row|row.values["partial"]==0 && row.values["present_calls"]==1).collect();
        let boundary: Vec<_> = rows.iter().filter(|row|row.values["partial"]!=0 || row.values["present_calls"]!=1).collect();
        let mut metrics = BTreeMap::new();
        let mut boundary_work = BTreeMap::new();
        for key in columns.keys().filter(|key|key.ends_with("_us") && key.as_str()!="elapsed_us") {
            metrics.insert(key, statistics(eligible.iter().filter(|row|!matches!(key.as_str(),"schedule_non_vm_us"|"session_unattributed_us") || row.values.get("phase_hierarchy_errors").copied().unwrap_or(0)==0).map(|row|row.values[key]).collect(),limits.get(key).copied()));
            if key.as_str()!="span_us" {
                boundary_work.insert(key,statistics(boundary.iter().filter(|row|!matches!(key.as_str(),"schedule_non_vm_us"|"session_unattributed_us") || row.values.get("phase_hierarchy_errors").copied().unwrap_or(0)==0).map(|row|row.values[key]).collect(),None));
            }
        }
        json!({"segment":segment,"label":label,"retained_frames":rows.len(),"eligible_intervals":eligible.len(),"sample_size":if eligible.len()>=120 {"at_least_120"}else{"short_descriptive_sample"},"partial_or_multi_present":rows.len()-eligible.len(),"phase_hierarchy_errors":eligible.iter().map(|row|row.values.get("phase_hierarchy_errors").copied().unwrap_or(0)).sum::<u64>(),"metrics":metrics,"boundary_work_metrics":boundary_work})
    }).collect();
    Ok(
        json!({"schema":1,"declared_environment":scope,"measurement":"present_submission_intervals_and_nested_wall_time","gpu_service_time":"unmeasured","display_cadence":"unmeasured","trace_metadata":trace.metadata,"limits_kind":"optional_workload_engineering_targets","groups":groups}),
    )
}

/// Summarize an existing probe log and optional JSON map of duration-name to
/// microsecond limit. The declared environment is retained without verification.
/// `elapsed_us` is an absolute timestamp and cannot be assigned a phase limit.
pub fn report(scope: &str, input: &Path, output: &Path, budget: Option<&Path>) -> Result<()> {
    ensure!(
        matches!(scope, "vita" | "emulator" | "host"),
        "environment must be vita, emulator or host"
    );
    let trace = parse(&fs::read_to_string(input).context("read probe log")?)?;
    let limits: BTreeMap<String, u64> = match budget {
        Some(path) => serde_json::from_slice(&fs::read(path)?)?,
        None => BTreeMap::new(),
    };
    let mut report = summarize(&trace, scope, &limits)?;
    report["input"] = json!(input);
    fs::write(output, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "{} frame samples, {} labelled segments; {}",
        trace.rows.len(),
        trace.labels.len(),
        output.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn residuals_use_each_frames_phase_hierarchy_and_flag_invalid_accounting() {
        let mut values = BTreeMap::from([
            ("session_us".into(), 100),
            ("session_schedule_us".into(), 60),
            ("vm_slice_us".into(), 40),
            ("host_call_us".into(), 20),
            ("session_gc_us".into(), 5),
            ("session_activity_us".into(), 5),
        ]);
        derive_phases(&mut values);
        assert_eq!(values["schedule_non_vm_us"], 20);
        assert_eq!(values["session_unattributed_us"], 10);
        assert_eq!(values["phase_hierarchy_errors"], 0);
        values.insert("session_schedule_us".into(), 30);
        derive_phases(&mut values);
        assert_eq!(values["phase_hierarchy_errors"], 1);
    }
    const TRACE: &str = "[reel-frame-report] begin\nframe_metadata\tversion=1\tdropped=0\nframe_segment\t1\tanimation\nframe_columns\tsegment\tpartial\tpresent_calls\tspan_us\tactive_us\tvm_slice_us\nframe_sample\t1\t1\t1\t0\t3000\t2000\nframe_sample\t1\t0\t1\t17000\t4000\t2500\nframe_sample\t1\t0\t1\t16000\t6000\t5000\n[reel-frame-report] end\n";
    #[test]
    fn phase_limits_keep_intervals_and_exclude_partial_work_without_claiming_gpu_time() {
        let trace = parse(TRACE).unwrap();
        let report = summarize(
            &trace,
            "emulator",
            &BTreeMap::from([("vm_slice_us".into(), 3000)]),
        )
        .unwrap();
        let group = &report["groups"][0];
        assert_eq!(group["eligible_intervals"], 2);
        assert_eq!(group["metrics"]["span_us"]["p99_us"], 17000);
        assert_eq!(group["metrics"]["vm_slice_us"]["over_limit"], 1);
        assert_eq!(group["boundary_work_metrics"]["active_us"]["p50_us"], 3000);
        assert!(group["boundary_work_metrics"].get("span_us").is_none());
        assert_eq!(report["gpu_service_time"], "unmeasured");
        assert!(summarize(&trace, "vita", &BTreeMap::from([("gpu_us".into(), 1)])).is_err());
    }
    #[test]
    fn latest_incomplete_run_cannot_fall_back_to_an_older_complete_run() {
        assert!(parse(&format!("{TRACE}[reel-frame-report] begin\n")).is_err());
        assert!(parse(&TRACE.replace("version=1", "version=2")).is_err());
        assert!(parse(&TRACE.replace("\t2500\n", "\t2500\t7\n")).is_err());
    }
    #[test]
    fn emulator_tty_fragments_reassemble_before_parsing() {
        let text = TRACE
            .lines()
            .map(|line| {
                format!(
                    "[time] |T| [write_file]: *** TTY: {line}\n[time] |T| [write_file]: *** TTY: \n"
                )
            })
            .collect::<String>();
        assert_eq!(parse(&text).unwrap().rows.len(), 3);
        let trimmed = TRACE
            .lines()
            .map(|line| format!("[time] |T| [write_file]: *** TTY: {line}\n"))
            .collect::<String>();
        assert_eq!(parse(&trimmed).unwrap().rows.len(), 3);
        assert_eq!(statistics(vec![1, 2, 3, 100], None)["p99_us"], 100);
    }
}
