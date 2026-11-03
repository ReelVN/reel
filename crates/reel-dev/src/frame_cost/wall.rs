use super::{BTreeMap, Frame, Value, json};

fn stats(mut values: Vec<u64>) -> Value {
    values.sort_unstable();
    if values.is_empty() {
        return json!({"samples":0});
    }
    let p = |n: usize| values[(values.len() * n).div_ceil(100) - 1];
    json!({"samples":values.len(), "total":values.iter().sum::<u64>(),
        "p50":p(50),"p95":p(95),"p99":p(99),"max":values.last()})
}

pub(super) fn summarize(rows: &[&Frame]) -> Value {
    let observed: Vec<_> = rows
        .iter()
        .copied()
        .filter(|r| r.values.get("wall_observed") == Some(&1))
        .collect();
    let mut valid = Vec::new();
    let mut closure_errors = 0;
    let mut stack_errors = 0;
    let mut lost_ns = 0;
    for row in &observed {
        let v = &row.values;
        // Require a pair for every named phase; missing fields must not become zero.
        let names: Vec<_> = v
            .keys()
            .filter(|k| {
                k.starts_with("wall_")
                    && k.ends_with("_ns")
                    && !matches!(
                        k.as_str(),
                        "wall_span_ns" | "wall_unscoped_ns" | "wall_lost_ns"
                    )
            })
            .collect();
        let phases = (v.get("wall_phase_count") == Some(&(names.len() as u64))
            && !names.is_empty())
        .then(|| {
            names.iter().try_fold(0u64, |sum, key| {
                v.get(&format!("{}_calls", key.strip_suffix("_ns").unwrap()))?;
                sum.checked_add(v[*key])
            })
        })
        .flatten();
        let total = phases
            .and_then(|sum| sum.checked_add(*v.get("wall_unscoped_ns")?))
            .and_then(|sum| sum.checked_add(*v.get("wall_lost_ns")?));
        let closed = total.is_some() && total == v.get("wall_span_ns").copied();
        closure_errors += u64::from(!closed);
        stack_errors += v.get("wall_errors").copied().unwrap_or(0);
        lost_ns += v.get("wall_lost_ns").copied().unwrap_or(0);
        if closed && v.get("wall_errors") == Some(&0) && v.get("wall_lost_ns") == Some(&0) {
            valid.push(*row);
        }
    }
    let mut metrics = BTreeMap::new();
    if let Some(first) = valid.first() {
        for key in first
            .values
            .keys()
            .filter(|k| k.starts_with("wall_") && k.as_str() != "wall_observed")
        {
            metrics.insert(key, stats(valid.iter().map(|r| r.values[key]).collect()));
        }
    }
    json!({"observed_intervals":observed.len(),"unobserved_intervals":rows.len()-observed.len(),
        "valid_intervals":valid.len(),"closure_errors":closure_errors,"stack_errors":stack_errors,
        "lost_ns":lost_ns,"measurement":"calling-thread exclusive wall nanoseconds; includes waits and instrumentation; snapshot-to-snapshot boundaries",
        "metrics":metrics})
}

pub(super) fn native(rows: &[&Frame]) -> Value {
    let observed: Vec<_> = rows
        .iter()
        .copied()
        .filter(|r| r.values.get("native_observed") == Some(&1))
        .collect();
    let mut metrics = BTreeMap::new();
    if let Some(first) = observed.first() {
        for key in first
            .values
            .keys()
            .filter(|k| k.starts_with("native_") && k.as_str() != "native_observed")
        {
            metrics.insert(key, stats(observed.iter().map(|r| r.values[key]).collect()));
        }
    }
    json!({"observed_intervals":observed.len(),"unobserved_intervals":rows.len()-observed.len(),
        "measurement":"SDK call wall microseconds and completion observation counters; overlaps enclosing wall phases; pending fields are gauges; notification age includes queueing and delayed polling",
        "metrics":metrics})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_missing_or_unbalanced_ledgers_and_keeps_loss_visible() {
        let mut v = BTreeMap::from([
            ("wall_observed".into(), 1),
            ("wall_span_ns".into(), 100),
            ("wall_unscoped_ns".into(), 40),
            ("wall_lost_ns".into(), 0),
            ("wall_errors".into(), 0),
        ]);
        v.insert("wall_phase_count".into(), 2);
        for name in ["input", "vm_slice"] {
            v.insert(format!("wall_{name}_ns"), 0);
            v.insert(format!("wall_{name}_calls"), 0);
        }
        v.insert("wall_vm_slice_ns".into(), 60);
        let valid = Frame {
            segment: 1,
            values: v.clone(),
        };
        v.insert("wall_vm_slice_ns".into(), 61);
        let unbalanced = Frame {
            segment: 1,
            values: v.clone(),
        };
        v.insert("wall_vm_slice_ns".into(), 50);
        v.insert("wall_lost_ns".into(), 10);
        v.insert("wall_errors".into(), 1);
        let loss = Frame {
            segment: 1,
            values: v.clone(),
        };
        v.remove("wall_input_calls");
        let missing = Frame {
            segment: 1,
            values: v,
        };
        let absent = Frame {
            segment: 1,
            values: BTreeMap::new(),
        };
        let report = summarize(&[&valid, &unbalanced, &loss, &missing, &absent]);
        assert_eq!(report["observed_intervals"], 4);
        assert_eq!(report["valid_intervals"], 1);
        assert_eq!(report["closure_errors"], 2);
        assert_eq!(report["lost_ns"], 20);
        assert_eq!(report["stack_errors"], 2);
        assert_eq!(report["metrics"]["wall_vm_slice_ns"]["p50"], 60);
    }
}
