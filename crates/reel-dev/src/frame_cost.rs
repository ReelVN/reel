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
        .and_then(|n| n.checked_add(activity))
        // Older version-1 traces omit diagnostic-output attribution.
        .and_then(|n| n.checked_add(values.get("session_diagnostics_us").copied().unwrap_or(0)));
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

fn quantities(rows: &[&Frame], columns: &BTreeMap<String, u64>) -> Value {
    let counted: Vec<_> = rows
        .iter()
        .filter(|row| row.values.get("vm_counts_valid") == Some(&1))
        .collect();
    let valid: Vec<_> = counted
        .iter()
        .filter(|row| {
            let opcodes: Vec<_> = row
                .values
                .iter()
                .filter(|(name, _)| name.starts_with("vm_opcode_") && name.ends_with("_count"))
                .collect();
            !opcodes.is_empty()
                && opcodes
                    .iter()
                    .try_fold(0u64, |sum, (_, n)| sum.checked_add(**n))
                    == row.values.get("vm_instructions_count").copied()
        })
        .collect();
    let mut metrics = BTreeMap::new();
    for name in columns.keys().filter(|name| {
        name.starts_with("vm_")
            && name.ends_with("_count")
            && !name.starts_with("vm_driver_")
            && !name.starts_with("vm_step_")
    }) {
        let mut values: Vec<u64> = valid.iter().map(|row| row.values[name]).collect();
        values.sort_unstable();
        if values.is_empty() {
            metrics.insert(name, json!({"samples":0}));
        } else {
            let percentile = |p: usize| values[(values.len() * p).div_ceil(100) - 1];
            metrics.insert(name, json!({"samples":values.len(),"total":values.iter().sum::<u64>(),
                "p50":percentile(50),"p95":percentile(95),"p99":percentile(99),"max":values.last()}));
        }
    }
    json!({"valid_intervals":valid.len(),"opcode_accounting_errors":counted.len()-valid.len(),
        "scope":"VM work and host frame-slot access; count-enabled diagnostics, not exclusive VmSlice time",
        "metrics":metrics})
}

fn vm_profile(rows: &[&Frame], columns: &BTreeMap<String, u64>) -> Value {
    if !columns.contains_key("vm_profile_valid") {
        return Value::Null;
    }
    let observed: Vec<_> = rows
        .iter()
        .copied()
        .filter(|row| row.values.get("vm_profile_valid") == Some(&1))
        .collect();
    let closed = |row: &&Frame| {
        let v = &row.values;
        let get = |name: &str| v.get(&format!("vm_driver_{name}_ns")).copied();
        let [
            Some(wall),
            Some(entry),
            Some(run),
            Some(exit),
            Some(scalar),
            Some(script),
            Some(native),
            Some(other),
            Some(clock),
            Some(calibration),
        ] = [
            "wall",
            "entry",
            "run",
            "exit",
            "scalar",
            "script",
            "native",
            "loop",
            "budget_clock",
            "calibration",
        ]
        .map(get)
        else {
            return false;
        };
        entry.checked_add(run).and_then(|n| n.checked_add(exit)) == Some(wall)
            && scalar
                .checked_add(script)
                .and_then(|n| n.checked_add(native))
                .and_then(|n| n.checked_add(other))
                == Some(run)
            && clock <= other
            && calibration <= entry
    };
    let valid: Vec<_> = observed.iter().copied().filter(closed).collect();
    let totals: BTreeMap<_, u64> = columns
        .keys()
        .filter(|name| name.starts_with("vm_driver_") || name.starts_with("vm_step_"))
        .map(|name| (name.clone(), valid.iter().map(|row| row.values[name]).sum()))
        .collect();
    let fraction = |part: &str, whole: &str| {
        let whole = totals
            .get(&format!("vm_driver_{whole}_ns"))
            .copied()
            .unwrap_or(0);
        (whole != 0).then(|| {
            totals
                .get(&format!("vm_driver_{part}_ns"))
                .copied()
                .unwrap_or(0) as f64
                / whole as f64
        })
    };
    json!({"valid_intervals":valid.len(),"ledger_errors":observed.len()-valid.len(),
        "measurement":"sampled whole-slice wall-time ledger; observer overhead included, not thread CPU time",
        "calibration":"raw empty clock-pair time is included in entry; no subtraction",
        "legacy_steps":"independent event samples; fetch/cost/execute overlap scalar/script; durations cannot be added",
        "totals":totals,
        "wall_fractions":{"entry":fraction("entry","wall"),"run":fraction("run","wall"),"exit":fraction("exit","wall")},
        "run_fractions":{"scalar":fraction("scalar","run"),"script":fraction("script","run"),"native":fraction("native","run"),"loop":fraction("loop","run")}})
}

fn thread_runtime(rows: &[&Frame], columns: &BTreeMap<String, u64>) -> Value {
    if !columns.contains_key("thread_profile_valid") {
        return Value::Null;
    }
    let valid: Vec<_> = rows
        .iter()
        .copied()
        .filter(|row| row.values.get("thread_profile_valid") == Some(&1))
        .collect();
    let mut phases = BTreeMap::new();
    for (phase, stride) in [("vm_slice", 127), ("replay", 1), ("present", 1)] {
        let prefix = format!("thread_{phase}_");
        let mut totals = BTreeMap::<String, u64>::new();
        let mut errors = std::collections::BTreeSet::new();
        let mut invalid = 0;
        for row in &valid {
            let get = |metric| row.values.get(&format!("{prefix}{metric}")).copied();
            let (Some(samples), Some(wall), Some(run)) =
                (get("samples_count"), get("wall_us"), get("run_us"))
            else {
                invalid += 1;
                continue;
            };
            if run > wall || samples == 0 && (run != 0 || wall != 0) {
                invalid += 1;
                continue;
            }
            for (key, value) in row
                .values
                .iter()
                .filter(|(key, _)| key.starts_with(&prefix))
            {
                let metric = key.strip_prefix(&prefix).unwrap();
                if metric == "last_error_code" {
                    if *value != 0 {
                        errors.insert(format!("{value:#010x}"));
                    }
                } else {
                    *totals.entry(metric.into()).or_default() += value;
                }
            }
        }
        let wall = totals.get("wall_us").copied().unwrap_or(0);
        let run = totals.get("run_us").copied().unwrap_or(0);
        let rejected = [
            "query_failures_count",
            "identity_mismatches_count",
            "clock_regressions_count",
            "inconsistent_pairs_count",
            "ambiguous_wraps_count",
            "clock_source_errors_count",
        ]
        .iter()
        .any(|key| totals.get(*key).is_some_and(|value| *value != 0));
        let status = if invalid != 0 || rejected {
            "rejected_observations"
        } else if wall == 0 {
            "no_paired_time"
        } else if totals
            .get("cycle_samples_count")
            .is_some_and(|value| *value != 0)
        {
            "raw_cycle_counter"
        } else if run == 0 {
            "counter_did_not_advance"
        } else {
            "paired_observations"
        };
        let ratio = (status == "paired_observations").then(|| run as f64 / wall as f64);
        phases.insert(phase, json!({"sample_stride":stride,"totals":totals,"accounting_errors":invalid,"query_error_codes":errors,"status":status,"sampled_run_over_wall":ratio}));
    }
    json!({"available_intervals":valid.len(),"measurement":"paired calling-thread execution counters and wall microseconds; cycle ticks retain their raw units; raw observations include query-boundary work; VM samples are not extrapolated to whole-frame CPU time","phases":phases})
}

fn summarize(trace: &Trace, scope: &str, limits: &BTreeMap<String, u64>) -> Result<Value> {
    let columns = &trace.rows[0].values;
    for key in limits.keys() {
        ensure!(
            key.ends_with("_us")
                && !key.starts_with("thread_")
                && key != "elapsed_us"
                && columns.contains_key(key),
            "unknown duration limit {key}"
        );
    }
    let groups: Vec<_> = trace.labels.iter().map(|(segment,label)| {
        let rows: Vec<_> = trace.rows.iter().filter(|row|row.segment==*segment).collect();
        let eligible: Vec<_> = rows.iter().copied().filter(|row|row.values["partial"]==0 && row.values["present_calls"]==1).collect();
        let boundary: Vec<_> = rows.iter().copied().filter(|row|row.values["partial"]!=0 || row.values["present_calls"]!=1).collect();
        let mut metrics = BTreeMap::new();
        let mut boundary_work = BTreeMap::new();
        for key in columns.keys().filter(|key|key.ends_with("_us") && !key.starts_with("thread_") && key.as_str()!="elapsed_us") {
            metrics.insert(key, statistics(eligible.iter().filter(|row|!matches!(key.as_str(),"schedule_non_vm_us"|"session_unattributed_us") || row.values.get("phase_hierarchy_errors").copied().unwrap_or(0)==0).map(|row|row.values[key]).collect(),limits.get(key).copied()));
            if key.as_str()!="span_us" {
                boundary_work.insert(key,statistics(boundary.iter().filter(|row|!matches!(key.as_str(),"schedule_non_vm_us"|"session_unattributed_us") || row.values.get("phase_hierarchy_errors").copied().unwrap_or(0)==0).map(|row|row.values[key]).collect(),None));
            }
        }
        json!({"segment":segment,"label":label,"retained_frames":rows.len(),"eligible_intervals":eligible.len(),"sample_size":if eligible.len()>=120 {"at_least_120"}else{"short_descriptive_sample"},"partial_or_multi_present":rows.len()-eligible.len(),"phase_hierarchy_errors":eligible.iter().map(|row|row.values.get("phase_hierarchy_errors").copied().unwrap_or(0)).sum::<u64>(),"metrics":metrics,"boundary_work_metrics":boundary_work,"quantities":quantities(&eligible,columns),"vm_profile":vm_profile(&eligible,columns),"thread_runtime":thread_runtime(&eligible,columns)})
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
    fn thread_observations_keep_sampling_and_clock_failures_separate_from_frame_times() {
        let values = BTreeMap::from([
            ("thread_profile_valid".into(), 1),
            ("thread_vm_slice_samples_count".into(), 2),
            ("thread_vm_slice_wall_us".into(), 400),
            ("thread_vm_slice_run_us".into(), 40),
            ("thread_vm_slice_query_wall_us".into(), 12),
            ("thread_vm_slice_query_failures_count".into(), 1),
            ("thread_vm_slice_last_error_code".into(), 0x80020001),
        ]);
        let valid = Frame {
            segment: 1,
            values: values.clone(),
        };
        let mut invalid = Frame {
            segment: 1,
            values: values.clone(),
        };
        invalid.values.insert("thread_vm_slice_run_us".into(), 401);
        let result = thread_runtime(&[&valid, &invalid], &values);
        let vm = &result["phases"]["vm_slice"];
        assert_eq!(vm["totals"]["run_us"], 40);
        assert_eq!(vm["totals"]["query_wall_us"], 12);
        assert!(vm["sampled_run_over_wall"].is_null());
        assert_eq!(vm["status"], "rejected_observations");
        assert_eq!(vm["sample_stride"], 127);
        assert_eq!(vm["accounting_errors"], 1);
        assert_eq!(vm["query_error_codes"][0], "0x80020001");
        let mut clean = Frame {
            segment: 1,
            values: values.clone(),
        };
        clean
            .values
            .insert("thread_vm_slice_query_failures_count".into(), 0);
        let result = thread_runtime(&[&clean], &values);
        assert_eq!(result["phases"]["vm_slice"]["sampled_run_over_wall"], 0.1);
        clean.values.insert("thread_vm_slice_run_us".into(), 0);
        let result = thread_runtime(&[&clean], &values);
        assert!(result["phases"]["vm_slice"]["sampled_run_over_wall"].is_null());
        assert_eq!(
            result["phases"]["vm_slice"]["status"],
            "counter_did_not_advance"
        );
        clean
            .values
            .insert("thread_vm_slice_cycle_samples_count".into(), 2);
        clean
            .values
            .insert("thread_vm_slice_cycle_ticks_count".into(), 1000);
        let result = thread_runtime(&[&clean], &values);
        let vm = &result["phases"]["vm_slice"];
        assert_eq!(vm["status"], "raw_cycle_counter");
        assert_eq!(vm["totals"]["cycle_ticks_count"], 1000);
        assert!(vm["sampled_run_over_wall"].is_null());
        assert!(thread_runtime(&[], &BTreeMap::new()).is_null());
    }
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
        values.insert("session_diagnostics_us".into(), 7);
        derive_phases(&mut values);
        assert_eq!(values["session_unattributed_us"], 3);
        assert_eq!(values["phase_hierarchy_errors"], 0);
        values.insert("session_diagnostics_us".into(), 11);
        derive_phases(&mut values);
        assert_eq!(values["phase_hierarchy_errors"], 1);
        values.insert("session_diagnostics_us".into(), 7);
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

    #[test]
    fn quantities_exclude_missing_snapshots_partial_frames_and_bad_opcode_sums() {
        let trace = "[reel-frame-report] begin\nframe_metadata\tversion=1\nframe_segment\t1\tanimation\nframe_columns\tsegment\tpartial\tpresent_calls\tspan_us\tactive_us\tvm_counts_valid\tvm_instructions_count\tvm_opcode_Clear_count\nframe_sample\t1\t1\t1\t0\t100\t1\t100\t100\nframe_sample\t1\t0\t1\t200\t100\t0\t0\t0\nframe_sample\t1\t0\t1\t200\t100\t1\t7\t7\nframe_sample\t1\t0\t1\t200\t100\t1\t8\t7\n[reel-frame-report] end\n";
        let report = summarize(&parse(trace).unwrap(), "host", &BTreeMap::new()).unwrap();
        let counts = &report["groups"][0]["quantities"];
        assert_eq!(counts["valid_intervals"], 1);
        assert_eq!(counts["opcode_accounting_errors"], 1);
        assert_eq!(counts["metrics"]["vm_instructions_count"]["total"], 7);
        assert_eq!(counts["metrics"]["vm_instructions_count"]["p50"], 7);
        assert!(
            counts["metrics"]["vm_instructions_count"]
                .get("p50_us")
                .is_none()
        );
    }

    #[test]
    fn sampled_driver_ledger_rejects_invalid_nesting_and_keeps_raw_observer_cost() {
        let values = BTreeMap::from([
            ("vm_profile_valid".into(), 1),
            ("vm_driver_wall_ns".into(), 100),
            ("vm_driver_entry_ns".into(), 10),
            ("vm_driver_run_ns".into(), 80),
            ("vm_driver_exit_ns".into(), 10),
            ("vm_driver_scalar_ns".into(), 20),
            ("vm_driver_script_ns".into(), 30),
            ("vm_driver_native_ns".into(), 10),
            ("vm_driver_loop_ns".into(), 20),
            ("vm_driver_budget_clock_ns".into(), 5),
            ("vm_driver_calibration_ns".into(), 3),
            ("vm_driver_samples_count".into(), 2),
            ("vm_step_script_sample_wall_ns".into(), 999),
        ]);
        let valid = Frame {
            segment: 1,
            values: values.clone(),
        };
        let mut bad = Frame {
            segment: 1,
            values: values.clone(),
        };
        bad.values.insert("vm_driver_budget_clock_ns".into(), 21);
        let mut missing = Frame {
            segment: 1,
            values: values.clone(),
        };
        missing.values.remove("vm_driver_exit_ns");
        let mut wrong_sum = Frame {
            segment: 1,
            values: values.clone(),
        };
        wrong_sum.values.insert("vm_driver_run_ns".into(), 81);
        let report = vm_profile(&[&valid, &bad, &missing, &wrong_sum], &values);
        assert_eq!(report["valid_intervals"], 1);
        assert_eq!(report["ledger_errors"], 3);
        assert_eq!(report["totals"]["vm_driver_wall_ns"], 100);
        assert_eq!(report["totals"]["vm_driver_calibration_ns"], 3);
        assert_eq!(report["totals"]["vm_step_script_sample_wall_ns"], 999);
        assert_eq!(report["run_fractions"]["script"], 0.375);
        assert!(vm_profile(&[], &BTreeMap::new()).is_null());
    }
}
