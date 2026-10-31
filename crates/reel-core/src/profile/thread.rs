//! Paired execution/wall observations for the calling thread. SDK access stays
//! in the platform adapter. No query result affects scheduling or rendering.
use super::Phase;
use std::{
    sync::{
        OnceLock,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::Instant,
};

/// Observed phases, in snapshot/report order. VM scopes sample one in 127;
/// replay and presentation scopes sample every call when a clock is installed.
pub const PHASES: [Phase; 3] = [Phase::VmSlice, Phase::Replay, Phase::Present];
/// Raw paired counters; times are microseconds, counts retain their suffix.
pub const METRICS: [&str; 13] = [
    "samples_count",
    "wall_us",
    "run_us",
    "query_calls_count",
    "query_wall_us",
    "query_failures_count",
    "identity_mismatches_count",
    "clock_regressions_count",
    "inconsistent_pairs_count",
    "cycle_samples_count",
    "cycle_ticks_count",
    "ambiguous_wraps_count",
    "clock_source_errors_count",
];
const SAMPLES: usize = 0;
const WALL: usize = 1;
const RUN: usize = 2;
const QUERIES: usize = 3;
const QUERY_WALL: usize = 4;
const FAILURES: usize = 5;
const IDENTITIES: usize = 6;
const REGRESSIONS: usize = 7;
const INCONSISTENT: usize = 8;
const CYCLE_SAMPLES: usize = 9;
const CYCLE_TICKS: usize = 10;
const AMBIGUOUS_WRAP: usize = 11;
const SOURCE: usize = 12;

/// Units and rollover contract of a platform execution counter.
#[derive(Clone, Copy, Debug)]
pub enum ThreadCounter {
    /// Cumulative execution microseconds from an immediately updated source.
    Microseconds(u64),
    /// Raw thread-local hardware cycle ticks. The frequency bound rejects
    /// intervals that could span a complete rollover; it is not a conversion
    /// factor. The platform keeps this counter running and owns its configuration.
    Cycles32 {
        /// Current unsigned hardware counter.
        ticks: u32,
        /// Maximum possible tick frequency during this interval, in Hz.
        max_frequency_hz: u64,
    },
}

/// A platform observation of one thread's cumulative execution time.
#[derive(Clone, Copy, Debug)]
pub struct ThreadTime {
    /// Stable process-local OS thread identity.
    pub thread: u64,
    /// Execution observation with its explicit units and rollover contract.
    pub counter: ThreadCounter,
}
type Clock = fn() -> Result<ThreadTime, i32>;
static CLOCK: OnceLock<Clock> = OnceLock::new();
static VM_CALLS: AtomicU32 = AtomicU32::new(0);
static COUNTERS: [[AtomicU64; METRICS.len()]; PHASES.len()] =
    [const { [const { AtomicU64::new(0) }; METRICS.len()] }; PHASES.len()];
static ERRORS: [AtomicU32; PHASES.len()] = [const { AtomicU32::new(0) }; PHASES.len()];

/// Install a process-lifetime observation function. It must query only the
/// caller, perform no allocation/IO, avoid panics and retain no native owner. Repeated calls
/// keep the first provider. Normal builds omit this interface and all queries.
pub fn install_thread_clock(clock: fn() -> Result<ThreadTime, i32>) {
    let _ = CLOCK.set(clock);
}

/// Raw counters for one phase. Pair wall/run include observation-boundary work;
/// query time is reported separately and is never silently subtracted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThreadCost {
    /// Values in `METRICS` order.
    pub values: [u64; METRICS.len()],
    /// Most recent failed native query, retaining the original signed code bits.
    pub last_error: u32,
}
/// Cumulative observation snapshot; phase fields are independently published.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThreadSnapshot {
    /// Whether a platform execution clock is installed.
    pub available: bool,
    /// Raw counters in `PHASES` order.
    pub costs: [ThreadCost; PHASES.len()],
}
impl ThreadSnapshot {
    /// Read fixed scalar storage without querying OS thread state.
    pub fn capture() -> Self {
        Self {
            available: CLOCK.get().is_some(),
            costs: std::array::from_fn(|phase| ThreadCost {
                values: std::array::from_fn(|metric| {
                    COUNTERS[phase][metric].load(Ordering::Relaxed)
                }),
                last_error: ERRORS[phase].load(Ordering::Relaxed),
            }),
        }
    }
    /// Difference consecutive observations. Missing clocks or counter resets
    /// leave the interval unavailable rather than reporting zero CPU use.
    pub fn checked_delta(&self, before: &Self) -> Option<Self> {
        if !self.available || !before.available {
            return None;
        }
        let mut result = Self {
            available: true,
            ..Self::default()
        };
        for (index, (after, before)) in self.costs.iter().zip(&before.costs).enumerate() {
            for metric in 0..METRICS.len() {
                result.costs[index].values[metric] =
                    after.values[metric].checked_sub(before.values[metric])?;
            }
            if result.costs[index].values[FAILURES] != 0 {
                result.costs[index].last_error = after.last_error;
            }
        }
        Some(result)
    }
}

pub(super) struct Probe {
    phase: usize,
    clock: Clock,
    before: ThreadTime,
    entered: Instant,
}
impl Probe {
    pub(super) fn begin(phase: Phase) -> Option<Self> {
        let phase = PHASES.iter().position(|p| *p as usize == phase as usize)?;
        let clock = *CLOCK.get()?;
        if phase == 0 && !VM_CALLS.fetch_add(1, Ordering::Relaxed).is_multiple_of(127) {
            return None;
        }
        let entered = Instant::now();
        let observed = clock();
        let query_wall = entered.elapsed().as_micros() as u64;
        record_query(phase, observed, query_wall);
        observed.ok().map(|before| Self {
            phase,
            clock,
            before,
            entered,
        })
    }
    pub(super) fn finish(self) {
        let query = Instant::now();
        let after = (self.clock)();
        let query_wall = query.elapsed().as_micros() as u64;
        let wall = self.entered.elapsed().as_micros() as u64;
        record_query(self.phase, after, query_wall);
        if let Ok(after) = after {
            match pair(self.before, after, wall) {
                Ok((run, cycles)) => {
                    add(self.phase, WALL, wall);
                    add(self.phase, RUN, run);
                    if let Some(cycles) = cycles {
                        add(self.phase, CYCLE_SAMPLES, 1);
                        add(self.phase, CYCLE_TICKS, cycles);
                    }
                    add(self.phase, SAMPLES, 1);
                }
                Err(metric) => add(self.phase, metric, 1),
            }
        }
    }
}
fn pair(before: ThreadTime, after: ThreadTime, wall: u64) -> Result<(u64, Option<u64>), usize> {
    if before.thread != after.thread {
        return Err(IDENTITIES);
    }
    match (before.counter, after.counter) {
        (ThreadCounter::Microseconds(before), ThreadCounter::Microseconds(after)) => {
            let run = after.checked_sub(before).ok_or(REGRESSIONS)?;
            if run > wall {
                return Err(INCONSISTENT);
            }
            Ok((run, None))
        }
        (
            ThreadCounter::Cycles32 {
                ticks: before,
                max_frequency_hz: before_hz,
            },
            ThreadCounter::Cycles32 {
                ticks: after,
                max_frequency_hz: after_hz,
            },
        ) => {
            if before_hz == 0 || before_hz != after_hz {
                return Err(SOURCE);
            }
            if u128::from(wall) * u128::from(before_hz) >= (1u128 << 32) * 1_000_000 {
                return Err(AMBIGUOUS_WRAP);
            }
            let ticks = u64::from(after.wrapping_sub(before));
            if u128::from(ticks) * 1_000_000 > u128::from(wall) * u128::from(before_hz) {
                return Err(INCONSISTENT);
            }
            Ok((0, Some(ticks)))
        }
        _ => Err(SOURCE),
    }
}
fn add(phase: usize, metric: usize, value: u64) {
    COUNTERS[phase][metric].fetch_add(value, Ordering::Relaxed);
}
fn record_query(phase: usize, observed: Result<ThreadTime, i32>, wall: u64) {
    add(phase, QUERIES, 1);
    add(phase, QUERY_WALL, wall);
    if let Err(error) = observed {
        ERRORS[phase].store(error as u32, Ordering::Relaxed);
        add(phase, FAILURES, 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pairs_separate_descheduled_time_and_reject_changed_identity_or_clock() {
        let before = ThreadTime {
            thread: 7,
            counter: ThreadCounter::Microseconds(100),
        };
        assert_eq!(
            pair(
                before,
                ThreadTime {
                    thread: 7,
                    counter: ThreadCounter::Microseconds(120)
                },
                200
            ),
            Ok((20, None))
        );
        assert_eq!(
            pair(
                before,
                ThreadTime {
                    thread: 8,
                    counter: ThreadCounter::Microseconds(120)
                },
                200
            ),
            Err(IDENTITIES)
        );
        assert_eq!(
            pair(
                before,
                ThreadTime {
                    thread: 7,
                    counter: ThreadCounter::Microseconds(99)
                },
                200
            ),
            Err(REGRESSIONS)
        );
        assert_eq!(
            pair(
                before,
                ThreadTime {
                    thread: 7,
                    counter: ThreadCounter::Microseconds(301)
                },
                200
            ),
            Err(INCONSISTENT)
        );
    }
    #[test]
    fn intervals_keep_raw_observer_time_and_missing_or_reset_data_explicit() {
        let mut before = ThreadSnapshot {
            available: true,
            ..Default::default()
        };
        let mut after = before;
        after.costs[0].values = [2, 400, 40, 4, 12, 0, 0, 0, 0, 0, 0, 0, 0];
        let delta = after.checked_delta(&before).unwrap();
        assert_eq!(delta.costs[0].values, after.costs[0].values);
        assert!(before.checked_delta(&after).is_none());
        before.available = false;
        assert!(after.checked_delta(&before).is_none());
    }

    #[test]
    fn raw_cycles_keep_units_and_reject_ambiguous_wraps_or_source_changes() {
        let clock = |ticks, max_frequency_hz| ThreadTime {
            thread: 7,
            counter: ThreadCounter::Cycles32 {
                ticks,
                max_frequency_hz,
            },
        };
        let before = clock(u32::MAX - 4, 500_000_000);
        assert_eq!(pair(before, clock(5, 500_000_000), 1), Ok((0, Some(10))));
        assert_eq!(
            pair(before, clock(5, 500_000_000), 10_000_000),
            Err(AMBIGUOUS_WRAP)
        );
        assert_eq!(pair(before, clock(5, 333_000_000), 1), Err(SOURCE));
        assert_eq!(pair(before, clock(5, 0), 1), Err(SOURCE));
        assert_eq!(pair(before, clock(5000, 500_000_000), 1), Err(INCONSISTENT));
    }
}
