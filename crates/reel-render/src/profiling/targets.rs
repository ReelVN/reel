//! Target construction spans recorded in the shared bounded trace collector.
use super::{KNOWN_ERROR_CODE, KNOWN_WALL, TraceData, TraceKind, TraceStatus, record_trace};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Process-local target identities survive descriptor reshaping and pointer reuse.
pub(crate) fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// A failed constructor still emits its last reached stage and total call wall
/// time. These spans include SDK blocking and provide no GPU execution timing.
pub(crate) struct Operation {
    id: u64,
    label: &'static str,
    started: Instant,
    stage_started: Instant,
    stage: &'static str,
    data: TraceData,
    finished: bool,
    enabled: bool,
}

impl Operation {
    pub(crate) fn new(label: &'static str, data: TraceData) -> Self {
        let now = Instant::now();
        Self {
            id: next_id(),
            label,
            started: now,
            stage_started: now,
            stage: "layout",
            data,
            finished: false,
            enabled: super::trace_enabled(),
        }
    }

    pub(crate) fn data_mut(&mut self) -> &mut TraceData {
        &mut self.data
    }

    fn record_stage(&self, status: TraceStatus, error_code: Option<i32>) {
        if !self.enabled {
            return;
        }
        let mut data = self.data;
        data.label = self.stage;
        data.status = status;
        data.operation_id = self.id;
        data.wall_ns = self.stage_started.elapsed().as_nanos() as u64;
        data.known |= KNOWN_WALL;
        if let Some(code) = error_code {
            data.error_code = i64::from(code);
            data.known |= KNOWN_ERROR_CODE;
        }
        record_trace(TraceKind::TargetAcquire, data);
    }

    pub(crate) fn stage(&mut self, stage: &'static str) {
        if !self.enabled {
            return;
        }
        self.record_stage(TraceStatus::Ok, None);
        self.stage = stage;
        self.stage_started = Instant::now();
    }

    pub(crate) fn fail(&mut self, error_code: Option<i32>) {
        if !self.enabled {
            self.finished = true;
            return;
        }
        self.record_stage(TraceStatus::Error, error_code);
        let mut data = self.data;
        data.label = self.label;
        data.status = TraceStatus::Error;
        data.operation_id = self.id;
        data.wall_ns = self.started.elapsed().as_nanos() as u64;
        data.known |= KNOWN_WALL;
        if let Some(code) = error_code {
            data.error_code = i64::from(code);
            data.known |= KNOWN_ERROR_CODE;
        }
        record_trace(TraceKind::TargetAcquire, data);
        self.finished = true;
    }

    pub(crate) fn complete(mut self, mut data: TraceData) {
        if !self.enabled {
            self.finished = true;
            return;
        }
        self.record_stage(TraceStatus::Ok, None);
        data.label = self.label;
        data.status = TraceStatus::Ok;
        data.operation_id = self.id;
        data.wall_ns = self.started.elapsed().as_nanos() as u64;
        data.known |= KNOWN_WALL;
        record_trace(TraceKind::TargetAcquire, data);
        self.finished = true;
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if !self.finished {
            self.fail(None);
        }
    }
}
