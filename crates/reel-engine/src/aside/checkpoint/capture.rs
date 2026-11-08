//! Frozen, budgeted capture of the business values shared by a continuation.
use super::ContinuationRecord;
use crate::aside::{
    SessionVm,
    data::{LogicalReferences, TypedCapture, TypedDataError},
};
use aside_vm::{
    HeapError, HeapReservation, RootKind, RootToken,
    executor::TaskId,
    resume::{
        BindingKey, CaptureProgress, ContinuationDescriptor, ResumeCaptureCursor, ResumeError,
        ResumeSite,
    },
    typed_data::Registry,
};
use std::sync::Arc;

/// Failure before a detached business continuation can be published.
#[derive(Debug)]
pub enum ContinuationError {
    Resume(ResumeError),
    Data(TypedDataError),
    Heap(HeapError),
    Contract(&'static str),
}
impl From<ResumeError> for ContinuationError {
    fn from(e: ResumeError) -> Self {
        Self::Resume(e)
    }
}
impl From<TypedDataError> for ContinuationError {
    fn from(e: TypedDataError) -> Self {
        Self::Data(e)
    }
}
impl From<HeapError> for ContinuationError {
    fn from(e: HeapError) -> Self {
        Self::Heap(e)
    }
}
impl std::fmt::Display for ContinuationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resume(e) => write!(f, "{e}"),
            Self::Data(e) => write!(f, "{e}"),
            Self::Heap(e) => write!(f, "{e}"),
            Self::Contract(e) => f.write_str(e),
        }
    }
}
impl std::error::Error for ContinuationError {}

/// A detached record with its retained data and metadata charged to the source
/// session budget. Move this owner to the storage worker for encoding.
pub struct CapturedContinuation {
    record: Arc<ContinuationRecord>,
    _graph: HeapReservation,
    _metadata: HeapReservation,
}
impl CapturedContinuation {
    /// Borrow the detached record while retaining its memory accounting.
    pub fn record(&self) -> &ContinuationRecord {
        &self.record
    }
}

/// Exclusively borrows the session while its business graph is frozen. Each step
/// reads a bounded number of bindings or graph edges; drop releases temporary roots.
pub struct ContinuationCapture<'a> {
    runtime: &'a mut SessionVm,
    cursor: ResumeCaptureCursor,
    keys: Vec<Vec<BindingKey>>,
    descriptor: ContinuationDescriptor,
    wrapper: Option<RootToken>,
    graph: Option<TypedCapture<'a>>,
    registry: &'a Registry,
    references: &'a dyn LogicalReferences,
    metadata: Option<HeapReservation>,
    complete: bool,
    failed: bool,
}
impl SessionVm {
    /// Freeze an actual registered waiting task and capture its live business
    /// bindings together, preserving aliases between parent and child activations.
    pub fn capture_continuation_begin<'a>(
        &'a mut self,
        task: TaskId,
        registry: &'a Registry,
        references: &'a dyn LogicalReferences,
    ) -> Result<ContinuationCapture<'a>, ContinuationError> {
        let cursor = self.vm.capture_resume_cursor(&self.resume, task)?;
        let descriptor = cursor.descriptor().clone();
        let keys = vec![Vec::new(); descriptor.frames.len()];
        let metadata = self
            .vm
            .heap()
            .budget_usage()
            .reserve(metadata_bytes(&descriptor, &keys))?;
        let array = self.vm.heap_mut().alloc_array(&[])?;
        let wrapper = self
            .vm
            .heap_mut()
            .add_root(self.root_owner, RootKind::Host, array)?;
        Ok(ContinuationCapture {
            runtime: self,
            cursor,
            keys,
            descriptor,
            wrapper: Some(wrapper),
            graph: None,
            registry,
            references,
            metadata: Some(metadata),
            complete: false,
            failed: false,
        })
    }
}
impl ContinuationCapture<'_> {
    /// Consume at most `work` continuation/graph units. `chunk` bounds each copied
    /// string or byte segment. A failed capture cannot produce a record.
    pub fn step(&mut self, work: u32, chunk: usize) -> Result<bool, ContinuationError> {
        if self.failed {
            return Err(ContinuationError::Contract("continuation capture failed"));
        }
        let result = self.advance(work, chunk);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn advance(&mut self, work: u32, chunk: usize) -> Result<bool, ContinuationError> {
        for _ in 0..work {
            if self.complete {
                return Ok(true);
            }
            if let Some(graph) = &mut self.graph {
                self.complete = graph.step(self.runtime.vm.heap(), 1, chunk)?.0;
                continue;
            }
            match self.cursor.step(&self.runtime.vm, 1)? {
                CaptureProgress::Binding(binding) => {
                    let frame = binding.frame;
                    let key = binding.key.clone();
                    let value = binding.value;
                    let before = self.keys[frame].capacity();
                    let key_bytes = key.0.capacity();
                    self.keys[frame].push(key);
                    let added = (self.keys[frame].capacity() - before) * size_of::<BindingKey>()
                        + key_bytes;
                    let metadata = self.metadata.as_mut().unwrap();
                    metadata.resize(metadata.bytes() + added as u64)?;
                    let heap = self.runtime.vm.heap_mut();
                    let array = heap.root_value(self.wrapper.unwrap())?;
                    heap.array_push(array, value)?;
                }
                CaptureProgress::BudgetYield => {}
                CaptureProgress::Complete => {
                    let heap = self.runtime.vm.heap_mut();
                    let array = heap.root_value(self.wrapper.unwrap())?;
                    self.graph = Some(TypedCapture::new(
                        heap,
                        self.runtime.root_owner,
                        array,
                        self.registry,
                        self.references,
                    )?);
                }
            }
        }
        Ok(self.complete)
    }
    /// Finish only after all shared values have been detached from the source heap.
    pub fn finish(mut self) -> Result<CapturedContinuation, ContinuationError> {
        if !self.complete || self.failed {
            return Err(ContinuationError::Contract(
                "continuation capture incomplete",
            ));
        }
        let (values, graph) = self
            .graph
            .take()
            .unwrap()
            .finish(self.runtime.vm.heap_mut())?;
        self.runtime
            .vm
            .heap_mut()
            .release_root(self.wrapper.take().unwrap())?;
        let record = ContinuationRecord::new(
            std::mem::take(&mut self.descriptor),
            std::mem::take(&mut self.keys),
            values,
        )
        .map_err(ContinuationError::Contract)?;
        Ok(CapturedContinuation {
            record: Arc::new(record),
            _graph: graph,
            _metadata: self.metadata.take().unwrap(),
        })
    }
}
impl Drop for ContinuationCapture<'_> {
    fn drop(&mut self) {
        if let Some(graph) = self.graph.take() {
            let _ = graph.cancel(self.runtime.vm.heap_mut());
        }
        if let Some(root) = self.wrapper.take() {
            let _ = self.runtime.vm.heap_mut().release_root(root);
        }
    }
}
fn metadata_bytes(descriptor: &ContinuationDescriptor, keys: &Vec<Vec<BindingKey>>) -> u64 {
    (size_of::<CapturedContinuation>()
        + size_of::<ContinuationRecord>()
        + 2 * size_of::<usize>()
        + descriptor.frames.capacity() * size_of::<aside_vm::resume::FrameDescriptor>()
        + descriptor
            .frames
            .iter()
            .map(|f| {
                f.flow.0.capacity()
                    + match &f.site {
                        ResumeSite::Point(k) => k.0.capacity(),
                        ResumeSite::Return(k) => k.0.capacity(),
                    }
            })
            .sum::<usize>()
        + keys.capacity() * size_of::<Vec<BindingKey>>()
        + keys
            .iter()
            .map(|v| {
                v.capacity() * size_of::<BindingKey>()
                    + v.iter().map(|k| k.0.capacity()).sum::<usize>()
            })
            .sum::<usize>()) as u64
}
