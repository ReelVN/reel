//! Optional desktop event-loop and polled render observations.
use super::{BTreeMap, Context, Result, Value, ensure, json, statistics};

pub(super) fn summarize(text: &str) -> Result<Option<Value>> {
    // The player writes pacing observations after this run's frame report.
    let Some((_, latest)) = text.rsplit_once("[reel-frame-report] begin") else {
        return Ok(None);
    };
    let Some((_, pacing)) = latest.split_once("\ndesktop_pacing_metadata\t") else {
        return Ok(None);
    };
    let mut lines = pacing.lines();
    let metadata = lines.next().context("missing desktop pacing metadata")?;
    let mut fields = metadata.split('\t');
    ensure!(fields.next() == Some("version=1"), "desktop pacing version");
    let dropped: u64 = fields
        .find_map(|field| field.strip_prefix("dropped="))
        .context("missing desktop pacing dropped count")?
        .parse()?;
    let mut wakes = BTreeMap::<&str, BTreeMap<&str, u64>>::new();
    let mut late_by_reason = BTreeMap::<&str, Vec<u64>>::new();
    let mut completions = BTreeMap::<&str, u64>::new();
    for line in lines {
        let fields: Vec<_> = line.split('\t').collect();
        match fields.as_slice() {
            ["desktop_wake", _, reason, _, _, cause, _, _, late] => {
                *wakes.entry(reason).or_default().entry(cause).or_default() += 1;
                if *cause == "deadline" {
                    late_by_reason
                        .entry(reason)
                        .or_default()
                        .push(late.parse()?);
                }
            }
            ["desktop_completion", _, kind, _, _] => {
                *completions.entry(kind).or_default() += 1;
            }
            ["desktop_wake" | "desktop_completion", ..] => {
                anyhow::bail!("invalid desktop pacing row width")
            }
            _ => {}
        }
    }
    let lateness: BTreeMap<_, _> = late_by_reason
        .into_iter()
        .map(|(reason, values)| (reason, statistics(values, None)))
        .collect();
    Ok(Some(json!({
        "clock":"frame_elapsed_us",
        "dropped_events":dropped,
        "wake_counts_by_reason_and_cause":wakes,
        "timer_lateness_by_reason":lateness,
        "polled_render_events":completions,
        "measurement":"host wake and completion observations in frames.tsv; GPU service time and display cadence are unmeasured"
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_timed_wakes_contribute_to_lateness_for_the_latest_recording() {
        let input = "[reel-frame-report] begin\n[reel-frame-report] end\n\
            desktop_pacing_metadata\tversion=1\tclock=frame_elapsed_us\tdropped=0\n\
            desktop_wake\t900\tstory\t10\t20\tdeadline\t15\t20\t880\n\
            [reel-frame-report] begin\n[reel-frame-report] end\n\
            desktop_pacing_metadata\tversion=1\tclock=frame_elapsed_us\tdropped=1\n\
            desktop_wake\t105\tstory\t10\t100\tdeadline\t14\t100\t5\n\
            desktop_wake\t190\tstory\t110\t200\tevent\t120\t200\t-\n\
            desktop_completion\t210\tframe\t1\t1\n";
        let summary = summarize(input).unwrap().unwrap();
        assert_eq!(summary["dropped_events"], 1);
        assert_eq!(
            summary["wake_counts_by_reason_and_cause"]["story"]["event"],
            1
        );
        assert_eq!(summary["timer_lateness_by_reason"]["story"]["samples"], 1);
        assert_eq!(summary["timer_lateness_by_reason"]["story"]["p95_us"], 5);
        assert_eq!(summary["polled_render_events"]["frame"], 1);
    }
}
