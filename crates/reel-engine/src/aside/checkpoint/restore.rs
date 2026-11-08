//! Current-state and current-frame construction in an isolated candidate session.
use super::{ContinuationError, ContinuationRecord};
use crate::aside::{
    SessionVm, SliceBudget,
    budget::SliceMinimum,
    data::{LogicalReferences, TypedRestore},
};
use aside_vm::{
    HeapReservation, ObjectKind, RootKind, RootToken,
    executor::{SliceUsage, TaskId},
    resume::{
        ResumeFrameValues, ResumeProgress, ResumeRestoreCursor, ResumeValue, VerifiedResumePlan,
    },
    typed_data::Registry,
};
use std::sync::Arc;

/// Restores the shared typed graph and then the verified business call chain.
/// The candidate remains exclusively borrowed until completion or cancellation.
pub struct ContinuationRestore<'a> {
    runtime: &'a mut SessionVm,
    record: Arc<ContinuationRecord>,
    graph: Option<TypedRestore<'a>>,
    graph_root: Option<RootToken>,
    plan: Option<VerifiedResumePlan>,
    values: Vec<ResumeFrameValues>,
    roots: Vec<RootToken>,
    frame: usize,
    binding: usize,
    offset: u32,
    frames: Option<ResumeRestoreCursor>,
    built: Option<TaskId>,
    metadata: HeapReservation,
    complete: bool,
    failed: bool,
}
impl SessionVm {
    /// Build a business continuation after current module initialization and State
    /// registration, before starting a main task in the isolated candidate.
    pub fn restore_continuation_begin<'a>(
        &'a mut self,
        record: Arc<ContinuationRecord>,
        registry: &'a Registry,
        references: &'a dyn LogicalReferences,
    ) -> Result<ContinuationRestore<'a>, ContinuationError> {
        if self.task.is_some() || self.pending.is_some() || self.delivery.is_some() {
            return Err(ContinuationError::Contract(
                "continuation restoration requires an idle candidate",
            ));
        }
        let plan = self.resume.verify_plan(record.continuation())?;
        let metadata = self
            .vm
            .heap()
            .budget_usage()
            .reserve(size_of::<ContinuationRestore<'_>>() as u64)?;
        let graph = TypedRestore::new(
            self.vm.heap_mut(),
            self.root_owner,
            record.values().clone(),
            registry,
            references,
        )?;
        Ok(ContinuationRestore {
            runtime: self,
            record,
            graph: Some(graph),
            graph_root: None,
            plan: Some(plan),
            values: Vec::new(),
            roots: Vec::new(),
            frame: 0,
            binding: 0,
            offset: 0,
            frames: None,
            built: None,
            metadata,
            complete: false,
            failed: false,
        })
    }
}
impl ContinuationRestore<'_> {
    /// Advance graph work in `work` units or frame work in `frame_budget`. Each
    /// call advances one phase; a frame yield reports its required minimum budget.
    pub fn step(
        &mut self,
        work: u32,
        chunk: usize,
        frame_budget: SliceBudget,
        mut now: impl FnMut() -> u64,
    ) -> Result<ResumeProgress, ContinuationError> {
        if self.failed {
            return Err(ContinuationError::Contract(
                "continuation restoration failed",
            ));
        }
        let result = self.advance(work, chunk, frame_budget, &mut now);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn advance(
        &mut self,
        work: u32,
        chunk: usize,
        frame_budget: SliceBudget,
        now: &mut impl FnMut() -> u64,
    ) -> Result<ResumeProgress, ContinuationError> {
        if self.complete {
            return Ok(ResumeProgress::Complete(self.built.unwrap()));
        }
        if frame_budget
            .deadline_ns
            .is_some_and(|deadline| now() >= deadline)
        {
            return Ok(pending());
        }
        if self.built.is_none() {
            if let Some(graph) = &mut self.graph
                && self.graph_root.is_none()
            {
                if graph.step(self.runtime.vm.heap_mut(), work, chunk)? {
                    self.graph_root = Some(
                        graph.value_root(self.runtime.vm.heap_mut(), self.runtime.root_owner)?,
                    );
                }
                return Ok(pending());
            }
        }
        if self.built.is_some() {
            for _ in 0..work {
                if let Some(root) = self.roots.pop() {
                    self.runtime.vm.heap_mut().release_root(root)?;
                    continue;
                }
                if let Some(root) = self.graph_root.take() {
                    self.runtime.vm.heap_mut().release_root(root)?;
                    continue;
                }
                if let Some(graph) = self.graph.take() {
                    graph.release(self.runtime.vm.heap_mut())?;
                    continue;
                }
                self.complete = true;
                return Ok(ResumeProgress::Complete(self.built.unwrap()));
            }
            return Ok(pending());
        }
        if let Some(frames) = &mut self.frames {
            match frames.step(&mut self.runtime.vm, frame_budget, now)? {
                ResumeProgress::Complete(task) => {
                    self.built = Some(task);
                    self.frames = None;
                    return Ok(pending());
                }
                outcome => return Ok(outcome),
            }
        }
        let array = self
            .runtime
            .vm
            .heap()
            .root_value(self.graph_root.unwrap())?;
        for _ in 0..work {
            if self.frame == self.record.bindings().len() {
                let info = self.runtime.vm.heap().info(array)?;
                if info.kind != ObjectKind::Array || info.len != self.offset {
                    return Err(ContinuationError::Contract(
                        "continuation business value array length differs from its bindings",
                    ));
                }
                self.frames = Some(self.runtime.vm.restore_resume_cursor(
                    self.plan.take().unwrap(),
                    std::mem::take(&mut self.values),
                    self.runtime.root_owner,
                )?);
                return Ok(pending());
            }
            if self.values.len() == self.frame {
                let before = self.values.capacity();
                self.values.push(ResumeFrameValues::default());
                self.charge((self.values.capacity() - before) * size_of::<ResumeFrameValues>())?;
                continue;
            }
            let keys = &self.record.bindings()[self.frame];
            if self.binding == keys.len() {
                self.frame += 1;
                self.binding = 0;
                continue;
            }
            let key = keys[self.binding].clone();
            let value = self.runtime.vm.heap().sequence_get(array, self.offset)?;
            let root = self.runtime.vm.heap_mut().add_root(
                self.runtime.root_owner,
                RootKind::Host,
                value,
            )?;
            let before = self.roots.capacity();
            self.roots.push(root);
            let root_bytes = (self.roots.capacity() - before) * size_of::<RootToken>();
            let bindings = &mut self.values[self.frame].bindings;
            let before = bindings.capacity();
            let key_bytes = key.0.capacity();
            bindings.push(ResumeValue { key, root });
            let binding_bytes =
                (bindings.capacity() - before) * size_of::<ResumeValue>() + key_bytes;
            self.charge(root_bytes + binding_bytes)?;
            self.binding += 1;
            self.offset += 1;
        }
        Ok(pending())
    }
    fn charge(&mut self, bytes: usize) -> Result<(), ContinuationError> {
        self.metadata.resize(self.metadata.bytes() + bytes as u64)?;
        Ok(())
    }
    /// Adopt the completed task into the candidate's ordinary scheduler. Dropping
    /// before this call cancels all constructed execution and releases input roots.
    pub fn finish(mut self) -> Result<TaskId, ContinuationError> {
        if !self.complete || self.failed {
            return Err(ContinuationError::Contract(
                "continuation restoration incomplete",
            ));
        }
        let task = self.built.take().unwrap();
        self.runtime.task = Some(task);
        self.runtime.blocked_tasks_revision = None;
        self.runtime.main_minimum = SliceMinimum::default();
        self.runtime.main_wait = None;
        Ok(task)
    }
}
impl Drop for ContinuationRestore<'_> {
    fn drop(&mut self) {
        if let Some(mut frames) = self.frames.take() {
            let _ = frames.cancel(&mut self.runtime.vm);
        }
        if let Some(task) = self.built.take() {
            let _ = self.runtime.vm.cancel_task(task);
        }
        for root in self.roots.drain(..) {
            let _ = self.runtime.vm.heap_mut().release_root(root);
        }
        if let Some(root) = self.graph_root.take() {
            let _ = self.runtime.vm.heap_mut().release_root(root);
        }
        if let Some(graph) = self.graph.take() {
            let _ = graph.release(self.runtime.vm.heap_mut());
        }
    }
}
fn pending() -> ResumeProgress {
    ResumeProgress::BudgetYield(SliceUsage {
        required_work: 1,
        ..Default::default()
    })
}
