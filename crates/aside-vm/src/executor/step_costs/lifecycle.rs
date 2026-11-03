//! Exclusive operation attribution within the driver's existing selected runs.
use std::{cell::Cell, time::Instant};

/// Outer dispatch order for [`LifecycleCost::unattributed_by_outer_ns`].
/// Loop contains run time outside scalar, script and native dispatch spans.
pub const LIFECYCLE_OUTER_NAMES: [&str; 4] = ["scalar", "script", "native", "loop"];
const LOOP: usize = 3;

/// Operation order for [`LifecycleCost::parts`]. Nested operations pause their
/// parent; these wall durations can be added within the same selected runs.
pub const LIFECYCLE_NAMES: [&str; 30] = [
    "class_spec_field",
    "class_spec_method",
    "class_spec_error",
    "class_final_field",
    "class_final_method",
    "class_final_error",
    "method_script_hit",
    "method_script_miss",
    "method_script_error",
    "method_native_hit",
    "method_native_miss",
    "method_native_error",
    "call_push_script",
    "call_push_native",
    "root_open",
    "root_release",
    "script_prepare",
    "script_exit",
    "named_validate",
    "named_scan",
    "named_eq",
    "named_commit",
    "structural_eq",
    "native_prepare",
    "native_resume",
    "native_commit",
    "native_release",
    "class_field_value",
    "other_table",
    "native_leaf_invoke",
];

#[derive(Clone, Copy)]
pub(crate) enum Part {
    ClassSpecField,
    ClassSpecMethod,
    ClassSpecError,
    ClassFinalField,
    ClassFinalMethod,
    ClassFinalError,
    MethodScriptHit,
    MethodScriptMiss,
    MethodScriptError,
    MethodNativeHit,
    MethodNativeMiss,
    MethodNativeError,
    CallPushScript,
    CallPushNative,
    RootOpen,
    RootRelease,
    ScriptPrepare,
    ScriptExit,
    NamedValidate,
    NamedScan,
    NamedEq,
    NamedCommit,
    StructuralEq,
    NativePrepare,
    NativeResume,
    NativeCommit,
    NativeRelease,
    ClassFieldValue,
    OtherTable,
    NativeLeafInvoke,
}

/// Completed operation calls and their exclusive sampled wall time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LifecyclePart {
    /// Completed calls inside selected runs, including errors and budget retries.
    pub calls: u64,
    /// Exclusive wall nanoseconds, excluding nested operations and marker pairs.
    pub wall_ns: u64,
}

/// Selected-run attribution. `run_ns = parts + unattributed_ns + observer_ns`.
/// These include descheduling; no CPU or whole-frame extrapolation is implied.
/// Markers measure publication plus the second clock read of each pair. The
/// first clock read remains in its preceding region; driver calibration stays raw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LifecycleCost {
    /// Completed operations in [`LIFECYCLE_NAMES`] order.
    pub parts: [LifecyclePart; LIFECYCLE_NAMES.len()],
    /// The same interpreter runs measured by `DriverCost::run_ns`.
    pub run_ns: u64,
    /// Run time outside named operations and measured marker intervals.
    pub unattributed_ns: u64,
    /// The same unattributed time partitioned by [`LIFECYCLE_OUTER_NAMES`].
    /// Existing driver boundary timestamps select the outer phase; named parts
    /// and observer markers remain separate, rather than being counted again.
    pub unattributed_by_outer_ns: [u64; LIFECYCLE_OUTER_NAMES.len()],
    /// Raw marker/reset intervals; never subtracted from another timer.
    pub observer_ns: u64,
    /// Additional Instant reads, excluding the driver's and the supplied now().
    pub observer_clock_queries: u64,
    /// Bad nesting, timestamp order, unfinished scopes or closure failures.
    pub errors: u64,
}

impl LifecycleCost {
    /// Subtract cumulative diagnostics after validating the parent VM identity.
    pub fn checked_delta(&self, before: &Self) -> Option<Self> {
        let mut parts = [LifecyclePart::default(); LIFECYCLE_NAMES.len()];
        for (i, part) in parts.iter_mut().enumerate() {
            part.calls = self.parts[i].calls.checked_sub(before.parts[i].calls)?;
            part.wall_ns = self.parts[i].wall_ns.checked_sub(before.parts[i].wall_ns)?;
        }
        let mut unattributed_by_outer_ns = [0; LIFECYCLE_OUTER_NAMES.len()];
        for (i, value) in unattributed_by_outer_ns.iter_mut().enumerate() {
            *value =
                self.unattributed_by_outer_ns[i].checked_sub(before.unattributed_by_outer_ns[i])?;
        }
        Some(Self {
            parts,
            run_ns: self.run_ns.checked_sub(before.run_ns)?,
            unattributed_ns: self.unattributed_ns.checked_sub(before.unattributed_ns)?,
            unattributed_by_outer_ns,
            observer_ns: self.observer_ns.checked_sub(before.observer_ns)?,
            observer_clock_queries: self
                .observer_clock_queries
                .checked_sub(before.observer_clock_queries)?,
            errors: self.errors.checked_sub(before.errors)?,
        })
    }
    pub(super) fn add(&mut self, row: &Self) {
        for (part, next) in self.parts.iter_mut().zip(row.parts) {
            part.calls += next.calls;
            part.wall_ns += next.wall_ns;
        }
        self.run_ns += row.run_ns;
        self.unattributed_ns += row.unattributed_ns;
        for (value, next) in self
            .unattributed_by_outer_ns
            .iter_mut()
            .zip(row.unattributed_by_outer_ns)
        {
            *value += next;
        }
        self.observer_ns += row.observer_ns;
        self.observer_clock_queries += row.observer_clock_queries;
        self.errors += row.errors;
    }
}

const DEPTH: usize = 8;
#[derive(Clone, Copy)]
struct Frame {
    part: Part,
    wall_ns: u64,
}
pub(crate) struct Span {
    depth: usize,
}

#[derive(Default)]
pub(super) struct State {
    enabled: Cell<bool>,
    began: Cell<Option<Instant>>,
    last: Cell<Option<Instant>>,
    depth: Cell<usize>,
    stack: [Cell<Option<Frame>>; DEPTH],
    values: [Cell<LifecyclePart>; LIFECYCLE_NAMES.len()],
    other: Cell<u64>,
    outer: Cell<usize>,
    other_by_outer: [Cell<u64>; LIFECYCLE_OUTER_NAMES.len()],
    observer: Cell<u64>,
    clocks: Cell<u64>,
    errors: Cell<u64>,
    run: Cell<u64>,
}

impl State {
    pub(super) fn reset(&self) {
        self.enabled.set(false);
    }
    pub(super) fn begin(&self, began: Instant) {
        self.begin_at(began);
        // Charge initialization and this clock read to an explicit observer bin.
        self.marker(began, Instant::now(), 1);
    }
    fn begin_at(&self, began: Instant) {
        self.enabled.set(true);
        self.began.set(Some(began));
        self.last.set(Some(began));
        self.depth.set(0);
        for value in &self.values {
            value.set(LifecyclePart::default());
        }
        self.other.set(0);
        self.outer.set(LOOP);
        for value in &self.other_by_outer {
            value.set(0);
        }
        self.observer.set(0);
        self.clocks.set(0);
        self.errors.set(0);
        self.run.set(0);
    }
    fn error(&self) {
        self.errors.set(self.errors.get() + 1);
    }
    fn elapsed(&self, end: Instant, begin: Instant) -> u64 {
        end.checked_duration_since(begin).map_or_else(
            || {
                self.error();
                0
            },
            |v| v.as_nanos().min(u128::from(u64::MAX)) as u64,
        )
    }
    fn account(&self, ended: Instant) {
        let Some(last) = self.last.get() else {
            self.error();
            return;
        };
        let duration = self.elapsed(ended, last);
        if self.depth.get() == 0 {
            self.other.set(self.other.get() + duration);
            let outer = &self.other_by_outer[self.outer.get()];
            outer.set(outer.get() + duration);
        } else {
            let slot = &self.stack[self.depth.get() - 1];
            let Some(mut frame) = slot.get() else {
                self.error();
                return;
            };
            frame.wall_ns += duration;
            slot.set(Some(frame));
        }
    }
    fn marker(&self, began: Instant, ended: Instant, clocks: u64) {
        self.observer
            .set(self.observer.get() + self.elapsed(ended, began));
        self.clocks.set(self.clocks.get() + clocks);
        self.last.set(Some(ended));
    }
    /// Reuse an existing driver timestamp. Boundary bookkeeping remains in the
    /// following region; this operation issues no additional observer clocks.
    pub(super) fn outer_at(&self, phase: usize, at: Instant) {
        if self.enabled.get() {
            self.account(at);
            self.last.set(Some(at));
            self.outer.set(phase);
        }
    }
    pub(super) fn outer_end(&self, at: Instant) {
        self.outer_at(LOOP, at);
    }
    #[inline]
    pub(super) fn start(&self, part: Part) -> Option<Span> {
        if !self.enabled.get() {
            return None;
        }
        let began = Instant::now();
        let span = self.enter_at(part, began);
        self.marker(began, Instant::now(), 2);
        Some(span)
    }
    fn enter_at(&self, part: Part, began: Instant) -> Span {
        self.account(began);
        let depth = self.depth.get();
        if depth == DEPTH {
            self.error();
            return Span { depth: usize::MAX };
        }
        self.stack[depth].set(Some(Frame { part, wall_ns: 0 }));
        self.depth.set(depth + 1);
        Span { depth }
    }
    #[inline]
    pub(super) fn finish(&self, span: Option<Span>, part: Option<Part>) {
        let Some(span) = span else { return };
        let began = Instant::now();
        self.leave_at(span, part, began);
        self.marker(began, Instant::now(), 2);
    }
    fn leave_at(&self, span: Span, part: Option<Part>, ended: Instant) {
        self.account(ended);
        if span.depth.checked_add(1) != Some(self.depth.get()) {
            self.error();
            return;
        }
        let Some(frame) = self.stack[span.depth].take() else {
            self.error();
            return;
        };
        self.depth.set(span.depth);
        let value = &self.values[part.unwrap_or(frame.part) as usize];
        let mut row = value.get();
        row.calls += 1;
        row.wall_ns += frame.wall_ns;
        value.set(row);
    }
    pub(super) fn end(&self, ended: Instant) {
        self.account(ended);
        self.enabled.set(false);
        if self.depth.get() != 0 {
            self.error();
        }
        let began = self.began.get().expect("selected run began");
        let run = self.elapsed(ended, began);
        self.run.set(run);
        let sum = self.values.iter().map(|v| v.get().wall_ns).sum::<u64>()
            + self.other.get()
            + self.observer.get();
        if sum != run {
            self.error();
        }
        if self.other_by_outer.iter().map(Cell::get).sum::<u64>() != self.other.get() {
            self.error();
        }
    }
    pub(super) fn snapshot(&self) -> LifecycleCost {
        LifecycleCost {
            parts: std::array::from_fn(|i| self.values[i].get()),
            run_ns: self.run.get(),
            unattributed_ns: self.other.get(),
            unattributed_by_outer_ns: std::array::from_fn(|i| self.other_by_outer[i].get()),
            observer_ns: self.observer.get(),
            observer_clock_queries: self.clocks.get(),
            errors: self.errors.get(),
        }
    }
}

#[cfg(test)]
mod tests;
