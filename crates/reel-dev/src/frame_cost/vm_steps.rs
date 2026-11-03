//! Keep eligibility and loss next to stage samples; correlated slice selection
//! cannot be extrapolated to a frame's CPU cost or added across nested stages.
use super::{BTreeMap, Frame, Value, json};

/// Normalize complete presentation intervals using their own VM counter deltas.
/// Live-thread phase windows and asynchronously published VM_STATE are excluded.
pub(super) fn execution(rows: &[&Frame]) -> Value {
    let Some(first) = rows.first() else {
        return json!({"status":"unavailable","reason":"no_complete_submission_intervals"});
    };
    if !first.values.contains_key("vm_counts_valid") {
        return json!({"status":"unavailable","reason":"vm_execution_counts_not_recorded",
            "required_feature":"reel-player/vm-execution-counts"});
    }
    let mut valid = Vec::new();
    let mut rejected = BTreeMap::<&str, usize>::new();
    let mut invalid_reasons = BTreeMap::<u64, usize>::new();
    let mut slice_call_mismatches = 0;
    for row in rows {
        let v = &row.values;
        let reason = if v.get("partial") != Some(&0) || v.get("present_calls") != Some(&1) {
            Some("partial_or_multi_present")
        } else if v.get("vm_counts_valid") != Some(&1) {
            if let Some(reason) = v.get("vm_counter_invalid_reason") {
                *invalid_reasons.entry(*reason).or_default() += 1;
            }
            Some("counter_interval_invalid")
        } else {
            let opcodes: Vec<_> = v
                .iter()
                .filter(|(name, _)| name.starts_with("vm_opcode_") && name.ends_with("_count"))
                .collect();
            let opcode_sum = opcodes
                .iter()
                .try_fold(0u64, |sum, (_, n)| sum.checked_add(**n));
            if opcodes.is_empty() || opcode_sum != v.get("vm_instructions_count").copied() {
                Some("opcode_accounting_error")
            } else if !interval_sequences_valid(v) {
                Some("interval_sequence_error")
            } else if ![
                "vm_slice_us",
                "vm_instructions_count",
                "vm_work_count",
                "vm_slices_count",
            ]
            .iter()
            .all(|name| v.contains_key(*name))
            {
                Some("counter_or_timing_column_missing")
            } else {
                None
            }
        };
        if let Some(reason) = reason {
            *rejected.entry(reason).or_default() += 1;
        } else {
            if let Some(calls) = v.get("vm_slice_calls")
                && Some(calls) != v.get("vm_slices_count")
            {
                // Rejected run_slice entries can consume wall time without an
                // admitted VM slice. Keep that overhead in its actual interval.
                slice_call_mismatches += 1;
            }
            valid.push(*row);
        }
    }
    let total = |name: &str| -> Option<u64> {
        if valid.is_empty() {
            return None;
        }
        valid
            .iter()
            .try_fold(0u64, |sum, row| sum.checked_add(*row.values.get(name)?))
    };
    let vm_us = total("vm_slice_us");
    let instructions = total("vm_instructions_count");
    let work = total("vm_work_count");
    let presents = total("present_calls");
    let updates = total("projection_updates_count");
    let ratio = |numerator: Option<u64>, denominator: Option<u64>, scale: f64| {
        numerator
            .zip(denominator)
            .and_then(|(n, d)| (d != 0).then_some(n as f64 * scale / d as f64))
    };
    let gc_us = total("session_gc_us")
        .zip(total("session_gc_begin_us"))
        .and_then(|(step, begin)| step.checked_add(begin));
    json!({
        "status":if valid.is_empty() {"no_valid_intervals"}else{"available"},
        "valid_intervals":valid.len(), "rejected_intervals":rejected,
        "counter_invalid_reasons":invalid_reasons,
        "measurement":"paired presentation-submission wall time and VM counter deltas; ratios use sums over the same valid intervals",
        "update_kind":"projection_published",
        "clock_update_counts":"unavailable",
        "derived_time":"included in vm_slice; not separately measured by these counters",
        "boundary_sequences_recorded":first.values.contains_key("loop_begin"),
        "slice_call_mismatch_intervals":slice_call_mismatches,
        "totals":{
            "vm_wall_us":vm_us,"instructions":instructions,"work":work,
            "slices":total("vm_slices_count"),"presents":presents,"projection_updates":updates,
            "native_entries":total("vm_native_entries_count"),"native_resumes":total("vm_native_resumes_count"),
            "host_dispatch_wall_us":total("host_call_us"),"host_dispatch_calls":total("host_call_calls"),
            "derived_starts":total("vm_derived_starts_count"),"compact_derived_starts":total("vm_compact_derived_starts_count"),
            "compact_commits":total("vm_compact_commits_count"),
            "gc_wall_us":gc_us,"gc_collections":total("vm_gc_collections_count"),"gc_units":total("vm_gc_units_count"),
            "heap_chunk_allocations":total("vm_heap_chunk_allocations_count")
        },
        "ratios":{
            "vm_wall_ns_per_instruction":ratio(vm_us,instructions,1000.),
            "vm_wall_ns_per_work":ratio(vm_us,work,1000.),
            "vm_wall_us_per_slice":ratio(vm_us,total("vm_slices_count"),1.),
            "vm_wall_us_per_present":ratio(vm_us,presents,1.),
            "vm_wall_us_per_projection_update":ratio(vm_us,updates,1.),
            "instructions_per_projection_update":ratio(instructions,updates,1.),
            "projection_updates_per_present":ratio(updates,presents,1.),
            "gc_wall_ns_per_unit":ratio(gc_us,total("vm_gc_units_count"),1000.),
            "host_dispatch_wall_us_per_call":ratio(total("host_call_us"),total("host_call_calls"),1.)
        }
    })
}

fn interval_sequences_valid(v: &BTreeMap<String, u64>) -> bool {
    for (begin, end, count) in [
        ("loop_begin", "loop_end", "loops"),
        (
            "projection_update_begin",
            "projection_update_end",
            "projection_updates_count",
        ),
    ] {
        match (v.get(begin), v.get(end)) {
            // Earlier version-1 frame counters already used the presentation
            // boundary, but did not retain its explicit loop/update identities.
            (None, None) => {}
            (Some(begin), Some(end)) => match (end.checked_sub(*begin), v.get(count)) {
                (Some(delta), Some(count)) if delta == *count => {}
                _ => return false,
            },
            _ => return false,
        }
    }
    true
}

const GAPS: [&str; 4] = [
    "first_dispatch",
    "scalar_reentry",
    "classified_dispatch",
    "final_dispatch",
];
const OUTER_PHASES: [&str; 4] = ["scalar", "script", "native", "loop"];

fn unattributed_outer_closed(v: &BTreeMap<String, u64>) -> Option<bool> {
    let fields = OUTER_PHASES.map(|phase| format!("vm_driver_parts_unattributed_{phase}_ns"));
    if !fields.iter().any(|field| v.contains_key(field)) {
        return None;
    }
    let sum = fields
        .iter()
        .zip(OUTER_PHASES)
        .try_fold(0u64, |sum, (field, phase)| {
            let value = *v.get(field)?;
            if value > *v.get(&format!("vm_driver_{phase}_ns"))? {
                return None;
            }
            sum.checked_add(value)
        });
    Some(sum.is_some() && sum == v.get("vm_driver_parts_unattributed_ns").copied())
}

pub(super) fn parts_closed(v: &BTreeMap<String, u64>) -> Option<bool> {
    let run = *v.get("vm_driver_parts_run_ns")?;
    let fields: Vec<_> = v
        .iter()
        .filter(|(name, _)| name.starts_with("vm_driver_part_") && name.ends_with("_ns"))
        .collect();
    let sum = fields
        .iter()
        .try_fold(0u64, |sum, (name, ns)| {
            let calls = *v.get(&format!("{}_calls_count", name.strip_suffix("_ns")?))?;
            if **ns != 0 && calls == 0 {
                return None;
            }
            sum.checked_add(**ns)
        })
        .and_then(|sum| sum.checked_add(*v.get("vm_driver_parts_unattributed_ns")?))
        .and_then(|sum| sum.checked_add(*v.get("vm_driver_parts_observer_ns")?));
    Some(
        !fields.is_empty()
            && sum == Some(run)
            && v.get("vm_driver_run_ns") == Some(&run)
            && v.get("vm_driver_parts_errors_count") == Some(&0)
            && unattributed_outer_closed(v).unwrap_or(true),
    )
}

pub(super) fn lifecycle(rows: &[&Frame]) -> Value {
    let observed: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.values
                .get("vm_driver_samples_count")
                .is_some_and(|n| *n > 0)
        })
        .filter_map(|row| parts_closed(&row.values))
        .collect();
    json!({"observed_intervals":observed.len(), "unobserved_intervals":rows.len()-observed.len(),
        "accounting_errors":observed.iter().filter(|valid| !**valid).count(),
        "measurement":"exclusive operation buckets in the same selected driver runs; parts + unattributed + observer = run; wall includes scheduling delays",
        "observer":"raw marker/reset intervals include publication and the second clock read; first clock reads remain in their preceding regions; raw driver calibration is not subtracted",
        "scope":"run_slice internals only; external HostCall and task-start work outside run_slice keep their existing frame columns",
        "unattributed_by_outer":unattributed_outer(rows)})
}

fn unattributed_outer(rows: &[&Frame]) -> Value {
    if !rows
        .iter()
        .any(|row| unattributed_outer_closed(&row.values).is_some())
    {
        return json!({"status":"unavailable","reason":"outer_unattributed_not_recorded"});
    }
    let observed: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.values
                .get("vm_driver_samples_count")
                .is_some_and(|n| *n > 0)
        })
        .collect();
    let valid: Vec<_> = observed
        .iter()
        .filter(|row| {
            unattributed_outer_closed(&row.values) == Some(true)
                && parts_closed(&row.values) == Some(true)
        })
        .collect();
    let total: u64 = valid
        .iter()
        .map(|row| row.values["vm_driver_parts_unattributed_ns"])
        .sum();
    let mut totals = BTreeMap::new();
    let mut fractions = BTreeMap::new();
    for phase in OUTER_PHASES {
        let ns: u64 = valid
            .iter()
            .map(|row| row.values[&format!("vm_driver_parts_unattributed_{phase}_ns")])
            .sum();
        totals.insert(format!("{phase}_ns"), ns);
        fractions.insert(phase, (total != 0).then(|| ns as f64 / total as f64));
    }
    json!({"status":if observed.is_empty(){"no_observed_runs"}else if valid.is_empty(){"rejected_observations"}else{"available"},
        "measurement":"unattributed run time partitioned by existing outer dispatch timestamps; named parts and observer are excluded; no additional clock reads",
        "valid_intervals":valid.len(),"rejected_intervals":observed.len()-valid.len(),
        "total_ns":total,"totals":totals,"fractions_of_unattributed":fractions})
}

pub(super) fn gaps_closed(v: &BTreeMap<String, u64>) -> Option<bool> {
    if !v
        .keys()
        .any(|k| k.starts_with("vm_driver_") && k.contains("_gap"))
    {
        return None;
    }
    let sum = GAPS.iter().try_fold(0u64, |sum, gap| {
        let duration = *v.get(&format!("vm_driver_{gap}_gap_ns"))?;
        let calls = *v.get(&format!("vm_driver_{gap}_gaps_count"))?;
        if duration != 0 && calls == 0 {
            return None;
        }
        sum.checked_add(duration)
    });
    let counts = [
        "vm_driver_first_dispatch_gaps_count",
        "vm_driver_final_dispatch_gaps_count",
        "vm_driver_samples_count",
    ]
    .map(|name| v.get(name).copied());
    Some(
        sum.is_some()
            && sum == v.get("vm_driver_loop_ns").copied()
            && v.get("vm_driver_loop_gap_errors_count") == Some(&0)
            && matches!(counts, [Some(first), Some(finals), Some(samples)] if first <= finals && finals <= samples),
    )
}

pub(super) fn gaps(rows: &[&Frame]) -> Value {
    let observed: Vec<_> = rows
        .iter()
        .filter_map(|row| gaps_closed(&row.values))
        .collect();
    json!({"observed_intervals":observed.len(),"unobserved_intervals":rows.len()-observed.len(),
        "accounting_errors":observed.iter().filter(|valid| !**valid).count(),
        "measurement":"disjoint gaps between existing outer-dispatch timestamps; sum equals loop_ns; includes preflight, handoff, observer publication, and waits; budget clock is a child, not additive"})
}

pub(super) fn summarize(rows: &[&Frame]) -> Value {
    let Some(first) = rows.first() else {
        return Value::Null;
    };
    let phases: Vec<_> = first
        .values
        .keys()
        .filter_map(|key| {
            key.strip_prefix("vm_step_")?
                .strip_suffix("_eligible_slices_count")
        })
        .collect();
    if phases.is_empty() {
        return Value::Null;
    }
    let mut result = BTreeMap::new();
    for phase in phases {
        let mut totals = BTreeMap::<String, u64>::new();
        let mut rejected = 0;
        for row in rows {
            let get = |name: &str| {
                row.values
                    .get(&format!("vm_step_{phase}_{name}_count"))
                    .copied()
            };
            let [
                Some(calls),
                Some(eligible),
                Some(selected),
                Some(samples),
                Some(dropped),
                Some(missing),
            ] = [
                "calls",
                "eligible_calls",
                "selected_calls",
                "samples",
                "location_dropped_samples",
                "location_missing_samples",
            ]
            .map(get)
            else {
                rejected += 1;
                continue;
            };
            if eligible > calls
                || selected > eligible
                || samples > selected
                || !dropped
                    .checked_add(missing)
                    .is_some_and(|lost| lost <= samples)
            {
                rejected += 1;
                continue;
            }
            for (key, value) in &row.values {
                if let Some(name) = key.strip_prefix(&format!("vm_step_{phase}_")) {
                    *totals.entry(name.into()).or_default() += value;
                }
            }
            *totals.entry("unsampled_calls_count".into()).or_default() += calls - selected;
            *totals.entry("unfinished_samples_count".into()).or_default() += selected - samples;
        }
        result.insert(
            phase,
            json!({"rejected_intervals":rejected,"totals":totals}),
        );
    }
    json!({"selection":"one eligible stage per VM slice, rotating; event stride applies within eligible entries; correlated sampling, no whole-frame extrapolation",
        "timing":"inclusive samples; post-body overhead measures only the observed subset of counter/hotspot publication, not total probe overhead",
        "root_slot_time":"included in its calling stage; separate root-slot execution time is unmeasured",
        "stages":result})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn execution_frame(vm_us: u64, instructions: u64, work: u64) -> Frame {
        Frame {
            segment: 1,
            values: BTreeMap::from([
                ("partial".into(), 0),
                ("present_calls".into(), 1),
                ("vm_counts_valid".into(), 1),
                ("vm_slice_us".into(), vm_us),
                ("vm_instructions_count".into(), instructions),
                ("vm_opcode_Move_count".into(), instructions),
                ("vm_work_count".into(), work),
                ("vm_slices_count".into(), 2),
                ("vm_slice_calls".into(), 2),
                ("loop_begin".into(), 1),
                ("loop_end".into(), 3),
                ("loops".into(), 2),
                ("projection_update_begin".into(), 2),
                ("projection_update_end".into(), 3),
                ("projection_updates_count".into(), 1),
                ("session_gc_us".into(), 30),
                ("session_gc_begin_us".into(), 10),
                ("vm_gc_units_count".into(), 20),
            ]),
        }
    }

    #[test]
    fn execution_uses_paired_totals_and_keeps_missing_capabilities_explicit() {
        let a = execution_frame(1000, 100, 200);
        let b = execution_frame(3000, 3000, 6000);
        let result = execution(&[&a, &b]);
        assert_eq!(result["valid_intervals"], 2);
        assert_eq!(result["totals"]["vm_wall_us"], 4000);
        assert_eq!(result["totals"]["instructions"], 3100);
        let ns = result["ratios"]["vm_wall_ns_per_instruction"]
            .as_f64()
            .unwrap();
        assert!((ns - 4_000_000. / 3100.).abs() < 1e-9);
        assert_eq!(result["ratios"]["gc_wall_ns_per_unit"], 2000.);
        assert!(result["totals"]["native_entries"].is_null());
        assert_eq!(result["clock_update_counts"], "unavailable");
        let zero = execution_frame(0, 0, 0);
        assert!(execution(&[&zero])["ratios"]["vm_wall_ns_per_instruction"].is_null());
        let mut disabled = execution_frame(1, 1, 1);
        disabled.values.remove("vm_counts_valid");
        assert_eq!(
            execution(&[&disabled])["reason"],
            "vm_execution_counts_not_recorded"
        );
        assert_eq!(execution(&[])["reason"], "no_complete_submission_intervals");
    }

    #[test]
    fn execution_rejects_reset_misaligned_and_partial_intervals() {
        let good = execution_frame(100, 10, 20);
        let mut reset = execution_frame(900, 90, 180);
        reset.values.insert("vm_counts_valid".into(), 0);
        reset.values.insert("vm_counter_invalid_reason".into(), 4);
        let mut shifted = execution_frame(900, 90, 180);
        shifted.values.insert("loop_end".into(), 4);
        let mut opcode = execution_frame(900, 90, 180);
        opcode.values.insert("vm_opcode_Move_count".into(), 89);
        let mut partial = execution_frame(900, 90, 180);
        partial.values.insert("partial".into(), 1);
        let result = execution(&[&good, &reset, &shifted, &opcode, &partial]);
        assert_eq!(result["valid_intervals"], 1);
        assert_eq!(result["totals"]["vm_wall_us"], 100);
        for reason in [
            "counter_interval_invalid",
            "interval_sequence_error",
            "opcode_accounting_error",
            "partial_or_multi_present",
        ] {
            assert_eq!(result["rejected_intervals"][reason], 1);
        }
        assert_eq!(result["counter_invalid_reasons"]["4"], 1);
        let mut legacy = execution_frame(100, 10, 20);
        for name in [
            "loop_begin",
            "loop_end",
            "projection_update_begin",
            "projection_update_end",
        ] {
            legacy.values.remove(name);
        }
        assert_eq!(execution(&[&legacy])["valid_intervals"], 1);
        assert_eq!(execution(&[&legacy])["boundary_sequences_recorded"], false);
    }
    #[test]
    fn lifecycle_requires_disjoint_complete_buckets_and_raw_observer_time() {
        assert_eq!(parts_closed(&BTreeMap::new()), None);
        let mut v = BTreeMap::from([
            ("vm_driver_run_ns".into(), 100),
            ("vm_driver_parts_run_ns".into(), 100),
            ("vm_driver_part_root_open_ns".into(), 30),
            ("vm_driver_part_root_open_calls_count".into(), 4),
            ("vm_driver_part_class_final_field_ns".into(), 40),
            ("vm_driver_part_class_final_field_calls_count".into(), 2),
            ("vm_driver_parts_unattributed_ns".into(), 20),
            ("vm_driver_parts_observer_ns".into(), 10),
            ("vm_driver_parts_errors_count".into(), 0),
        ]);
        assert_eq!(parts_closed(&v), Some(true));
        v.insert("vm_driver_parts_observer_ns".into(), 0);
        assert_eq!(parts_closed(&v), Some(false));
        v.insert("vm_driver_parts_observer_ns".into(), 10);
        v.insert("vm_driver_parts_errors_count".into(), 1);
        assert_eq!(parts_closed(&v), Some(false));
        v.insert("vm_driver_parts_errors_count".into(), 0);
        v.remove("vm_driver_part_root_open_calls_count");
        assert_eq!(parts_closed(&v), Some(false));
    }
    #[test]
    fn outer_unattributed_requires_all_buckets_and_preserves_legacy_absence() {
        let mut v = BTreeMap::from([
            ("vm_driver_samples_count".into(), 1),
            ("vm_driver_run_ns".into(), 100),
            ("vm_driver_scalar_ns".into(), 20),
            ("vm_driver_script_ns".into(), 50),
            ("vm_driver_native_ns".into(), 10),
            ("vm_driver_loop_ns".into(), 20),
            ("vm_driver_parts_run_ns".into(), 100),
            ("vm_driver_part_root_open_ns".into(), 70),
            ("vm_driver_part_root_open_calls_count".into(), 4),
            ("vm_driver_parts_unattributed_ns".into(), 20),
            ("vm_driver_parts_observer_ns".into(), 10),
            ("vm_driver_parts_errors_count".into(), 0),
        ]);
        assert_eq!(unattributed_outer_closed(&v), None);
        let legacy = Frame {
            segment: 1,
            values: v.clone(),
        };
        let result = lifecycle(&[&legacy]);
        assert_eq!(result["accounting_errors"], 0);
        assert_eq!(result["unattributed_by_outer"]["status"], "unavailable");
        assert!(result["unattributed_by_outer"].get("totals").is_none());
        for (phase, ns) in OUTER_PHASES.into_iter().zip([5, 7, 3, 5]) {
            v.insert(format!("vm_driver_parts_unattributed_{phase}_ns"), ns);
        }
        assert_eq!(parts_closed(&v), Some(true));
        let first = Frame {
            segment: 1,
            values: v.clone(),
        };
        v.insert("vm_driver_parts_unattributed_scalar_ns".into(), 10);
        v.insert("vm_driver_parts_unattributed_script_ns".into(), 2);
        let second = Frame {
            segment: 1,
            values: v.clone(),
        };
        let result = lifecycle(&[&first, &second]);
        let outer = &result["unattributed_by_outer"];
        assert_eq!(outer["valid_intervals"], 2);
        assert_eq!(outer["total_ns"], 40);
        assert_eq!(outer["totals"]["scalar_ns"], 15);
        assert_eq!(outer["fractions_of_unattributed"]["scalar"], 0.375);
        v.insert("vm_driver_parts_unattributed_scalar_ns".into(), 11);
        assert_eq!(parts_closed(&v), Some(false));
        v.insert("vm_driver_parts_unattributed_scalar_ns".into(), 10);
        v.insert("vm_driver_scalar_ns".into(), 9);
        assert_eq!(unattributed_outer_closed(&v), Some(false));
        v.remove("vm_driver_parts_unattributed_loop_ns");
        assert_eq!(unattributed_outer_closed(&v), Some(false));
        let invalid = Frame {
            segment: 1,
            values: v,
        };
        let result = lifecycle(&[&first, &invalid]);
        assert_eq!(result["accounting_errors"], 1);
        assert_eq!(result["unattributed_by_outer"]["valid_intervals"], 1);
        assert_eq!(result["unattributed_by_outer"]["rejected_intervals"], 1);
        assert_eq!(result["unattributed_by_outer"]["total_ns"], 20);
    }
    #[test]
    fn gap_ledger_requires_every_boundary_counter_and_exact_partition() {
        let mut values = BTreeMap::from([
            ("vm_driver_loop_ns".into(), 10),
            ("vm_driver_loop_gap_errors_count".into(), 0),
            ("vm_driver_samples_count".into(), 1),
        ]);
        assert_eq!(gaps_closed(&BTreeMap::new()), None);
        for (i, gap) in GAPS.iter().enumerate() {
            values.insert(format!("vm_driver_{gap}_gap_ns"), i as u64 + 1);
            values.insert(format!("vm_driver_{gap}_gaps_count"), 1);
        }
        assert_eq!(gaps_closed(&values), Some(true));
        values.insert("vm_driver_final_dispatch_gaps_count".into(), 2);
        assert_eq!(gaps_closed(&values), Some(false));
        values.insert("vm_driver_samples_count".into(), 2);
        assert_eq!(gaps_closed(&values), Some(true));
        values.insert("vm_driver_first_dispatch_gaps_count".into(), 3);
        assert_eq!(gaps_closed(&values), Some(false));
        values.insert("vm_driver_first_dispatch_gaps_count".into(), 1);
        values.insert("vm_driver_final_dispatch_gap_ns".into(), 5);
        assert_eq!(gaps_closed(&values), Some(false));
        values.insert("vm_driver_final_dispatch_gap_ns".into(), 4);
        values.insert("vm_driver_loop_gap_errors_count".into(), 1);
        assert_eq!(gaps_closed(&values), Some(false));
        values.insert("vm_driver_loop_gap_errors_count".into(), 0);
        values.remove("vm_driver_scalar_reentry_gaps_count");
        assert_eq!(gaps_closed(&values), Some(false));
    }
    #[test]
    fn reports_denominators_and_unfinished_samples_without_treating_them_as_zero_time() {
        let mut values = BTreeMap::new();
        for (name, value) in [
            ("calls", 100),
            ("eligible_slices", 2),
            ("eligible_calls", 20),
            ("selected_calls", 4),
            ("samples", 3),
            ("location_dropped_samples", 1),
            ("location_missing_samples", 1),
        ] {
            values.insert(format!("vm_step_fetch_{name}_count"), value);
        }
        let good = Frame {
            segment: 1,
            values: values.clone(),
        };
        values.insert("vm_step_fetch_selected_calls_count".into(), 21);
        let bad = Frame { segment: 1, values };
        let report = summarize(&[&good, &bad]);
        let stage = &report["stages"]["fetch"];
        assert_eq!(stage["rejected_intervals"], 1);
        assert_eq!(stage["totals"]["unsampled_calls_count"], 96);
        assert_eq!(stage["totals"]["unfinished_samples_count"], 1);
        assert_eq!(stage["totals"]["location_dropped_samples_count"], 1);
    }
}
