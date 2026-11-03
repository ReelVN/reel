//! Bounded calling-thread observations joined by frame, attempt and pass.
#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
use reel_core::identity::FrameId;
#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
use std::cell::Cell;
use std::{
    io::{self, Write},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

/// Maximum retained events between explicit drains; recording never allocates.
pub const TRACE_CAPACITY: usize = 16_384;
/// `logical` is observed.
pub const KNOWN_LOGICAL: u32 = 1 << 0;
/// `storage` is observed.
pub const KNOWN_STORAGE: u32 = 1 << 1;
/// `source_extent` is observed.
pub const KNOWN_SOURCE_EXTENT: u32 = 1 << 2;
/// `output_extent` is observed.
pub const KNOWN_OUTPUT_EXTENT: u32 = 1 << 3;
/// `scissor` is observed, including an explicitly empty rectangle.
pub const KNOWN_SCISSOR: u32 = 1 << 4;
/// `write_region` is observed; its area describes allowed writes, not shading.
pub const KNOWN_WRITE_REGION: u32 = 1 << 5;
/// `store_region` is observed; it bounds allowed stores, not actual PBE traffic.
pub const KNOWN_STORE_REGION: u32 = 1 << 6;
/// `valid_region` is observed as the scene's exclusive right/bottom limit.
pub const KNOWN_VALID_REGION: u32 = 1 << 7;
/// `TraceData::wall_ns` is an observed elapsed wall interval.
pub const KNOWN_WALL: u32 = 1 << 8;
/// `error_code` contains an original native error code.
pub const KNOWN_ERROR_CODE: u32 = 1 << 9;
/// `stride_bytes` is the physical row stride.
pub const KNOWN_STRIDE: u32 = 1 << 10;
/// `target_format` is observed (1 RGBA8, 2 Coverage8).
pub const KNOWN_TARGET_FORMAT: u32 = 1 << 11;
/// `storage_kind` is observed (1 linear, 2 tiled).
pub const KNOWN_STORAGE_KIND: u32 = 1 << 12;
/// `texture_bindings` is the issued texture-unit count, not fetch instructions.
pub const KNOWN_TEXTURE_BINDINGS: u32 = 1 << 13;

/// Stable event categories in the version-1 diagnostic report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceKind {
    /// A render-present attempt starts, including attempts that only prepare.
    AttemptBegin,
    /// The attempt returned or its guard detected an incomplete exit.
    AttemptEnd,
    /// An issued operation starts, preserving production batching in normal mode.
    PassBegin,
    /// The matching operation returned or its guard detected an incomplete exit.
    PassEnd,
    /// The attempt acquired a final presentation submission identity.
    Submission,
    /// A target acquisition, construction stage or reshape observation.
    TargetAcquire,
    /// A target's initialization or proved overwrite decision.
    TargetInit,
    /// A target returns to frame-local reusable storage.
    TargetRecycle,
    /// A cache lookup, insertion, eviction or macro-owner selection.
    TargetCache,
    /// An effect-cache lookup or publication observation.
    EffectCache,
    /// A completed source is consumed again within its current parent window.
    SourceReuse,
    /// A final fragment notification was observed or its wait failed.
    Completion,
}
impl TraceKind {
    /// Stable version-1 TSV spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AttemptBegin => "attempt_begin",
            Self::AttemptEnd => "attempt_end",
            Self::PassBegin => "pass_begin",
            Self::PassEnd => "pass_end",
            Self::Submission => "submission",
            Self::TargetAcquire => "target_acquire",
            Self::TargetInit => "target_init",
            Self::TargetRecycle => "target_recycle",
            Self::TargetCache => "target_cache",
            Self::EffectCache => "effect_cache",
            Self::SourceReuse => "source_reuse",
            Self::Completion => "completion",
        }
    }
}

/// Explicit operation terminal state; begin rows use `Pending`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TraceStatus {
    /// The operation has begun or no terminal observation applies.
    #[default]
    Pending,
    /// The operation returned successfully.
    Ok,
    /// The exact candidate needs a later preparation attempt.
    Preparing,
    /// The operation skipped work under its existing contract.
    Skipped,
    /// The operation returned an error.
    Error,
    /// A bounded completion observation timed out.
    Timeout,
    /// An enabled scope ended without an explicit result.
    Aborted,
}
impl TraceStatus {
    /// Stable version-1 TSV spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ok => "ok",
            Self::Preparing => "preparing",
            Self::Skipped => "skipped",
            Self::Error => "error",
            Self::Timeout => "timeout",
            Self::Aborted => "aborted",
        }
    }
}

/// Fixed-size payload. Rectangles use x/y/width/height; unknown domains are
/// identified by `known`, rather than interpreting zero as measured coverage.
/// Target identities are opaque run-local owners, never SDK handles.
#[derive(Clone, Copy, Debug, Default)]
pub struct TraceData {
    /// Static operation/outcome label, written outside the measured operation.
    pub label: &'static str,
    /// Explicit terminal state or `Pending` for a begin observation.
    pub status: TraceStatus,
    /// `KNOWN_*` mask describing available extent and interval fields.
    pub known: u32,
    /// Compiled effect identity; all zeros means this is not an effect row.
    pub definition: [u8; 32],
    /// Author widget owner when an originating record is available.
    pub owner: u64,
    /// Physical source owner identity.
    pub source_target: u64,
    /// Physical output owner identity.
    pub output_target: u64,
    /// Source view identity within its physical owner.
    pub source_view_id: u64,
    /// Source writable-lease generation.
    pub source_lease_id: u64,
    /// Output view identity within its physical owner.
    pub view_id: u64,
    /// Output writable-lease generation.
    pub lease_id: u64,
    /// Logical viewport or requested target extent.
    pub logical: [u32; 2],
    /// Requested or owned physical storage extent, according to the row label.
    pub storage: [u32; 2],
    /// Physical sampled source view extent.
    pub source_extent: [u32; 2],
    /// Physical output view extent.
    pub output_extent: [u32; 2],
    /// Effective issued pixel-scissor bound, not a fragment invocation count.
    pub scissor: [u32; 4],
    /// Per-use initialization/write bound, including required filter padding.
    pub write_region: [u32; 4],
    /// Color-surface store clip when directly observed.
    pub store_region: [u32; 4],
    /// Scene valid-region exclusive right/bottom limit when observed.
    pub valid_region: [u32; 2],
    /// Actual admitted/retained bytes when the row describes memory.
    pub bytes: u64,
    /// Operation count; its meaning is given by the static row label.
    pub count: u64,
    /// Duration of this operation, distinct from the event's timestamp.
    pub wall_ns: u64,
    /// Construction-stage correlation, including attempts with no owner yet.
    pub operation_id: u64,
    /// Original native error code when `KNOWN_ERROR_CODE` is present.
    pub error_code: i64,
    /// Physical row stride when directly observed.
    pub stride_bytes: u64,
    /// 1 RGBA8, 2 Coverage8; unknown unless `KNOWN_TARGET_FORMAT` is present.
    pub target_format: u32,
    /// 1 linear, 2 tiled; unknown unless `KNOWN_STORAGE_KIND` is present.
    pub storage_kind: u32,
    /// Bound texture units, including duplicate planes, not texture-fetch calls.
    pub texture_bindings: u32,
}

/// One observation. Durations and timestamps are calling-thread wall clocks;
/// this trace does not claim hardware timestamps, fragment counts or PBE bytes.
#[derive(Clone, Copy, Debug)]
pub struct TraceEvent {
    /// Monotonic sequence, including gaps left by dropped observations.
    pub sequence: u64,
    /// Wall-clock nanoseconds since this explicitly enabled recording began.
    pub wall_ns: u64,
    /// Immutable input frame identity.
    pub frame_id: u64,
    /// Unique render-present attempt, including deferred preparation attempts.
    pub attempt_id: u64,
    /// Final submission identity, or zero before one has actually been assigned.
    pub submission_id: u64,
    /// Unique operation scope, or zero outside one.
    pub pass_id: u64,
    /// Enclosing operation scope; clear/native draws may nest inside an effect.
    pub parent_pass_id: u64,
    /// Stable event kind.
    pub kind: TraceKind,
    /// Fixed observation payload.
    pub data: TraceData,
}

/// State returned by one explicit drain; retain it with the corresponding rows.
#[derive(Clone, Copy, Debug)]
pub struct TraceDrain {
    /// Recording is still enabled.
    pub enabled: bool,
    /// Fixed collector capacity.
    pub capacity: usize,
    /// Number of rows returned by this drain.
    pub retained: usize,
    /// Lost observations since the preceding drain, including contention.
    pub dropped: u64,
    /// Render attempts still active at the drain boundary.
    pub active: u64,
    /// Recording was explicitly stopped after all attempts ended.
    pub ended: bool,
    /// Recording was stopped while an attempt was active.
    pub interrupted: bool,
}

#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
#[derive(Clone, Copy, Default)]
struct Context {
    frame: u64,
    attempt: u64,
    submission: u64,
    pass: u64,
    parent: u64,
}
#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
thread_local! { static CURRENT: Cell<Context> = const { Cell::new(Context { frame:0, attempt:0, submission:0, pass:0, parent:0 }) }; }
static ENABLED: AtomicBool = AtomicBool::new(false);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicU64 = AtomicU64::new(0);
static NEXT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static NEXT_ATTEMPT: AtomicU64 = AtomicU64::new(1);
static NEXT_PASS: AtomicU64 = AtomicU64::new(1);
struct State {
    rows: [Option<TraceEvent>; TRACE_CAPACITY],
    len: usize,
    origin: Option<Instant>,
    ended: bool,
    interrupted: bool,
}
static STATE: Mutex<State> = Mutex::new(State {
    rows: [None; TRACE_CAPACITY],
    len: 0,
    origin: None,
    ended: false,
    interrupted: false,
});

/// Enable a fresh bounded recording or stop it explicitly. Call at an idle probe
/// boundary. Enabling while attempts remain active preserves the old recording
/// and marks it interrupted instead of discarding unmatched scopes.
pub fn set_trace_enabled(enabled: bool) {
    let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
    if enabled {
        if ACTIVE.load(Ordering::Relaxed) != 0 {
            state.interrupted = true;
            return;
        }
        state.len = 0;
        state.origin = Some(Instant::now());
        state.ended = false;
        state.interrupted = false;
        DROPPED.store(0, Ordering::Relaxed);
        NEXT_SEQUENCE.store(1, Ordering::Relaxed);
        NEXT_ATTEMPT.store(1, Ordering::Relaxed);
        NEXT_PASS.store(1, Ordering::Relaxed);
    } else {
        state.ended = ACTIVE.load(Ordering::Relaxed) == 0;
        state.interrupted |= !state.ended;
    }
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether diagnostic event recording is explicitly enabled.
pub fn trace_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
fn record(context: Context, kind: TraceKind, data: TraceData) {
    let sequence = NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let Ok(mut state) = STATE.try_lock() else {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if state.len == TRACE_CAPACITY {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let Some(origin) = state.origin else {
        return;
    };
    let event = TraceEvent {
        sequence,
        wall_ns: origin.elapsed().as_nanos() as u64,
        frame_id: context.frame,
        attempt_id: context.attempt,
        submission_id: context.submission,
        pass_id: context.pass,
        parent_pass_id: context.parent,
        kind,
        data,
    };
    let index = state.len;
    state.rows[index] = Some(event);
    state.len += 1;
}

#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
pub(crate) fn record_trace(kind: TraceKind, data: TraceData) {
    if trace_enabled() {
        CURRENT.with(|current| record(current.get(), kind, data));
    }
}

#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
pub(crate) fn trace_submission(id: u64) {
    if trace_enabled() {
        CURRENT.with(|current| {
            let mut context = current.get();
            context.submission = id;
            current.set(context);
        });
        record_trace(
            TraceKind::Submission,
            TraceData {
                label: "final",
                status: TraceStatus::Ok,
                ..Default::default()
            },
        );
    }
}

#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
#[derive(Clone, Copy)]
pub(crate) struct TraceLink {
    context: Context,
    started: Instant,
}
#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
pub(crate) fn trace_link() -> Option<TraceLink> {
    trace_enabled().then(|| TraceLink {
        context: CURRENT.with(|current| current.get()),
        started: Instant::now(),
    })
}
#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
pub(crate) fn trace_completed(link: TraceLink, mut data: TraceData) {
    data.wall_ns = link.started.elapsed().as_nanos() as u64;
    data.known |= KNOWN_WALL;
    data.status = TraceStatus::Ok;
    record(link.context, TraceKind::Completion, data);
}

#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
pub(crate) struct TraceAttempt {
    previous: Context,
    finished: bool,
}
#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
impl TraceAttempt {
    pub(crate) fn begin(frame: FrameId) -> Option<Self> {
        if !trace_enabled() {
            return None;
        }
        let context = Context {
            frame: frame.0,
            attempt: NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed),
            ..Default::default()
        };
        let previous = CURRENT.with(|current| current.replace(context));
        ACTIVE.fetch_add(1, Ordering::Relaxed);
        record(
            context,
            TraceKind::AttemptBegin,
            TraceData {
                label: "render_present",
                ..Default::default()
            },
        );
        Some(Self {
            previous,
            finished: false,
        })
    }
    pub(crate) fn finish(mut self, status: TraceStatus, mut data: TraceData) {
        data.status = status;
        if data.label.is_empty() {
            data.label = "render_present";
        }
        let context = CURRENT.with(|current| current.get());
        record(context, TraceKind::AttemptEnd, data);
        self.finished = true;
    }
}
#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
impl Drop for TraceAttempt {
    fn drop(&mut self) {
        if !self.finished {
            record(
                CURRENT.with(|current| current.get()),
                TraceKind::AttemptEnd,
                TraceData {
                    label: "render_present",
                    status: TraceStatus::Aborted,
                    ..Default::default()
                },
            );
        }
        CURRENT.with(|current| current.set(self.previous));
        ACTIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
pub(crate) struct TracePass {
    previous: Context,
    context: Context,
    label: &'static str,
    finished: bool,
}
#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
impl TracePass {
    pub(crate) fn begin(label: &'static str, mut data: TraceData) -> Option<Self> {
        if !trace_enabled() {
            return None;
        }
        let previous = CURRENT.with(|current| current.get());
        let context = Context {
            pass: NEXT_PASS.fetch_add(1, Ordering::Relaxed),
            parent: previous.pass,
            ..previous
        };
        CURRENT.with(|current| current.set(context));
        data.label = label;
        data.status = TraceStatus::Pending;
        record(context, TraceKind::PassBegin, data);
        Some(Self {
            previous,
            context,
            label,
            finished: false,
        })
    }
    pub(crate) fn finish(mut self, status: TraceStatus, mut data: TraceData) {
        data.status = status;
        if data.label.is_empty() {
            data.label = self.label;
        }
        record(self.context, TraceKind::PassEnd, data);
        self.finished = true;
    }
}
#[cfg(any(all(feature = "gxm", target_os = "vita"), test))]
impl Drop for TracePass {
    fn drop(&mut self) {
        if !self.finished {
            record(
                self.context,
                TraceKind::PassEnd,
                TraceData {
                    label: self.label,
                    status: TraceStatus::Aborted,
                    ..Default::default()
                },
            );
        }
        CURRENT.with(|current| {
            let active = current.get();
            let mut previous = self.previous;
            if active.attempt == previous.attempt {
                previous.submission = active.submission;
            }
            current.set(previous);
        });
    }
}

/// Drain rows in sequence order. Reserve the caller's vector outside measured
/// work; this function may allocate while copying into that output vector.
pub fn drain_trace(output: &mut Vec<TraceEvent>) -> TraceDrain {
    let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
    output.clear();
    output.extend(state.rows[..state.len].iter().flatten().copied());
    let retained = state.len;
    state.len = 0;
    TraceDrain {
        enabled: trace_enabled(),
        capacity: TRACE_CAPACITY,
        retained,
        dropped: DROPPED.swap(0, Ordering::Relaxed),
        active: ACTIVE.load(Ordering::Relaxed),
        ended: state.ended,
        interrupted: state.interrupted,
    }
}

/// Write stable version-1 TSV outside the measured rendering interval. Fields
/// named coverage/store remain bounds; GPU counters and timestamps are unavailable.
pub fn write_trace_rows(
    out: &mut impl Write,
    rows: &[TraceEvent],
    state: TraceDrain,
) -> io::Result<()> {
    writeln!(out, "trace_begin\tversion=1")?;
    writeln!(
        out,
        "trace_metadata\tversion=1\ttime=wall_ns\tgpu_timestamp=false\tcoverage=issued_bounds\tenabled={}\tcapacity={}\tretained={}\tdropped={}\tactive={}\tended={}\tinterrupted={}",
        state.enabled,
        state.capacity,
        state.retained,
        state.dropped,
        state.active,
        state.ended,
        state.interrupted
    )?;
    writeln!(
        out,
        "trace_columns\tsequence\twall_ns\tframe_id\tattempt_id\tsubmission_id\tpass_id\tparent_pass_id\tkind\tlabel\tstatus\tknown\tdefinition\towner\tsource_target\toutput_target\tsource_view_id\tsource_lease_id\tview_id\tlease_id\tlogical_w\tlogical_h\tstorage_w\tstorage_h\tsource_w\tsource_h\toutput_w\toutput_h\tscissor_x\tscissor_y\tscissor_w\tscissor_h\twrite_x\twrite_y\twrite_w\twrite_h\tstore_x\tstore_y\tstore_w\tstore_h\tvalid_w\tvalid_h\tbytes\tcount\toperation_wall_ns\toperation_id\terror_code\tstride_bytes\ttarget_format\tstorage_kind\ttexture_bindings"
    )?;
    for row in rows {
        let d = &row.data;
        write!(
            out,
            "trace_event\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t",
            row.sequence,
            row.wall_ns,
            row.frame_id,
            row.attempt_id,
            row.submission_id,
            row.pass_id,
            row.parent_pass_id,
            row.kind.as_str(),
            d.label,
            d.status.as_str(),
            d.known
        )?;
        for byte in d.definition {
            write!(out, "{byte:02x}")?;
        }
        for value in [
            d.owner,
            d.source_target,
            d.output_target,
            d.source_view_id,
            d.source_lease_id,
            d.view_id,
            d.lease_id,
        ] {
            write!(out, "\t{value}")?;
        }
        for value in d
            .logical
            .into_iter()
            .chain(d.storage)
            .chain(d.source_extent)
            .chain(d.output_extent)
            .chain(d.scissor)
            .chain(d.write_region)
            .chain(d.store_region)
            .chain(d.valid_region)
        {
            write!(out, "\t{value}")?;
        }
        writeln!(
            out,
            "\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            d.bytes,
            d.count,
            d.wall_ns,
            d.operation_id,
            d.error_code,
            d.stride_bytes,
            d.target_format,
            d.storage_kind,
            d.texture_bindings
        )?;
    }
    writeln!(
        out,
        "trace_end\tretained={}\tdropped={}\tactive={}\tended={}\tinterrupted={}",
        state.retained, state.dropped, state.active, state.ended, state.interrupted
    )?;
    Ok(())
}

/// Drain and write a complete version-1 block outside measured rendering.
/// A disabled, empty recorder emits no report. The returned state reports loss
/// and incomplete scopes; IO failures are returned through the ordinary result.
pub fn write_trace(out: &mut impl Write) -> io::Result<TraceDrain> {
    let mut rows = Vec::new();
    let state = drain_trace(&mut rows);
    if !rows.is_empty() || state.enabled || state.dropped != 0 {
        write_trace_rows(out, &rows, state)?;
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlated_scopes_finish_abort_and_report_bounded_loss_without_fake_domains() {
        set_trace_enabled(true);
        let attempt = TraceAttempt::begin(FrameId(7)).unwrap();
        let pass = TracePass::begin("test-pass", TraceData::default()).unwrap();
        record_trace(
            TraceKind::TargetInit,
            TraceData {
                label: "test-init",
                known: KNOWN_WRITE_REGION,
                write_region: [32, 64, 64, 32],
                ..Default::default()
            },
        );
        pass.finish(TraceStatus::Ok, TraceData::default());
        trace_submission(11);
        let completion = trace_link().unwrap();
        attempt.finish(TraceStatus::Ok, TraceData::default());
        trace_completed(
            completion,
            TraceData {
                label: "final-flight-observed",
                ..Default::default()
            },
        );
        let attempt = TraceAttempt::begin(FrameId(7)).unwrap();
        drop(TracePass::begin("aborted-pass", TraceData::default()).unwrap());
        drop(attempt);
        set_trace_enabled(false);
        let mut rows = Vec::new();
        let state = drain_trace(&mut rows);
        assert!(state.ended);
        assert!(!state.interrupted);
        assert_eq!((state.active, state.dropped), (0, 0));
        let first = rows
            .iter()
            .find(|row| row.kind == TraceKind::AttemptBegin)
            .unwrap();
        let completed = rows
            .iter()
            .find(|row| row.kind == TraceKind::Completion)
            .unwrap();
        assert_eq!(
            (
                completed.frame_id,
                completed.attempt_id,
                completed.submission_id
            ),
            (7, first.attempt_id, 11)
        );
        for begin in rows.iter().filter(|row| row.kind == TraceKind::PassBegin) {
            assert_eq!(
                rows.iter()
                    .filter(|end| end.kind == TraceKind::PassEnd && end.pass_id == begin.pass_id)
                    .count(),
                1
            );
        }
        assert_eq!(
            rows.iter()
                .filter(|row| row.data.status == TraceStatus::Aborted)
                .count(),
            2
        );
        let init = rows
            .iter()
            .find(|row| row.kind == TraceKind::TargetInit)
            .unwrap();
        assert_eq!(init.data.known, KNOWN_WRITE_REGION);
        assert_eq!(init.data.known & KNOWN_STORE_REGION, 0);
        let mut text = Vec::new();
        write_trace_rows(&mut text, &rows, state).unwrap();
        let text = String::from_utf8(text).unwrap();
        let columns = text
            .lines()
            .find(|line| line.starts_with("trace_columns\t"))
            .unwrap()
            .split('\t')
            .count();
        assert!(
            text.lines()
                .filter(|line| line.starts_with("trace_event\t"))
                .all(|line| line.split('\t').count() == columns)
        );

        set_trace_enabled(true);
        for _ in 0..TRACE_CAPACITY + 3 {
            record_trace(TraceKind::TargetCache, TraceData::default());
        }
        set_trace_enabled(false);
        let state = drain_trace(&mut rows);
        assert_eq!((state.retained, state.dropped), (TRACE_CAPACITY, 3));
        assert_eq!(rows.len(), TRACE_CAPACITY);
        let mut disabled = Vec::new();
        write_trace(&mut disabled).unwrap();
        assert!(disabled.is_empty());

        set_trace_enabled(true);
        let attempt = TraceAttempt::begin(FrameId(9)).unwrap();
        set_trace_enabled(false);
        drop(attempt);
        let state = drain_trace(&mut rows);
        assert!(state.interrupted);
        assert!(!state.ended);
        assert_eq!(state.active, 0);

        set_trace_enabled(true);
        let held = STATE.lock().unwrap();
        record_trace(TraceKind::TargetCache, TraceData::default());
        drop(held);
        set_trace_enabled(false);
        assert_eq!(drain_trace(&mut rows).dropped, 1);
    }
}
