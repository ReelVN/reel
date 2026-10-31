//! Keep eligibility and loss next to stage samples; correlated slice selection
//! cannot be extrapolated to a frame's CPU cost or added across nested stages.
use super::{BTreeMap, Frame, Value, json};

const GAPS: [&str; 4] = [
    "first_dispatch",
    "scalar_reentry",
    "classified_dispatch",
    "final_dispatch",
];

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
