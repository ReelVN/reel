//! Frozen, budgeted capture of the business values shared by a continuation.
use super::context::{ContextSlots, MemberName, Source};
use super::{CheckpointBinding, CheckpointRecord};
use crate::aside::{
    NativeBudget, NativeDispatch, SessionVm,
    data::{TypedCapture, TypedDataError},
    media::MediaCapture,
    stage::resources::SceneCapture,
};
use crate::{Session, reading::checkpoint::ReadingCapture};
use aside_vm::{
    HeapError, HeapReservation, RootKind, RootOwner, RootToken, Value,
    resume::{
        CaptureProgress, ContinuationDescriptor, ResumeCaptureCursor, ResumeError, ResumeSite,
        RootKey,
    },
};
use std::sync::Arc;

/// Failure before a detached business continuation can be published.
#[derive(Debug)]
pub enum CheckpointError {
    Vm(crate::aside::VmError),
    Resume(ResumeError),
    Data(TypedDataError),
    Heap(HeapError),
    /// The candidate still requires more capacity after garbage collection.
    ResourceBlocked(aside_vm::executor::ResourceBlocked),
    Contract(&'static str),
}
impl From<crate::aside::VmError> for CheckpointError {
    fn from(error: crate::aside::VmError) -> Self {
        Self::Vm(error)
    }
}
impl From<ResumeError> for CheckpointError {
    fn from(e: ResumeError) -> Self {
        Self::Resume(e)
    }
}
impl From<TypedDataError> for CheckpointError {
    fn from(e: TypedDataError) -> Self {
        Self::Data(e)
    }
}
impl From<HeapError> for CheckpointError {
    fn from(e: HeapError) -> Self {
        Self::Heap(e)
    }
}
impl From<crate::aside::AdapterError> for CheckpointError {
    fn from(error: crate::aside::AdapterError) -> Self {
        Self::Data(error.into())
    }
}
impl From<crate::SessionError> for CheckpointError {
    fn from(error: crate::SessionError) -> Self {
        Self::from(crate::aside::AdapterError::from(error))
    }
}
impl std::fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Vm(e) => write!(f, "{e:?}"),
            Self::Resume(e) => write!(f, "{e}"),
            Self::Data(e) => write!(f, "{e}"),
            Self::Heap(e) => write!(f, "{e}"),
            Self::ResourceBlocked(blocked) => write!(
                f,
                "checkpoint resource {:?}: used {}, requires {} after garbage collection",
                blocked.resource, blocked.current, blocked.required
            ),
            Self::Contract(e) => f.write_str(e),
        }
    }
}
impl std::error::Error for CheckpointError {}

/// A detached record with its retained data and metadata charged to the source
/// session budget. Keep this owner on the session thread while a worker encodes
/// the shared, code-independent record.
pub struct CapturedCheckpoint {
    record: Arc<CheckpointRecord>,
    _graph: HeapReservation,
    _metadata: HeapReservation,
}
impl CapturedCheckpoint {
    /// Borrow the detached record while retaining its memory accounting.
    pub fn record(&self) -> &CheckpointRecord {
        &self.record
    }
    /// Share pure data with a storage worker. Retain this accounting owner until
    /// that worker reports completion.
    pub fn shared_record(&self) -> Arc<CheckpointRecord> {
        self.record.clone()
    }
}

/// Movable cursor over one frozen session. The owner keeps business execution
/// paused and supplies the same session to each step, finish or cancellation.
pub struct CheckpointCapture {
    owner: RootOwner,
    root: RootKey,
    cursor: ResumeCaptureCursor,
    keys: Vec<Vec<CheckpointBinding>>,
    descriptor: ContinuationDescriptor,
    wrapper: Option<RootToken>,
    graph: Option<TypedCapture>,
    reading: Option<ReadingCapture>,
    graph_complete: bool,
    reading_bytes: u64,
    metadata: Option<HeapReservation>,
    complete: bool,
    failed: bool,
    required_work: u32,
    media: Option<MediaCapture>,
    voice: Value,
    context: Option<ContextSlots>,
    member: Option<MemberBinding>,
}
struct MemberBinding {
    frame: usize,
    key: aside_vm::resume::BindingKey,
    name: MemberName,
    charge: HeapReservation,
}
impl SessionVm {
    /// Freeze an actual registered waiting task and capture its live business
    /// bindings together, preserving aliases between parent and child activations.
    pub fn capture_checkpoint_begin(
        &mut self,
        session: &Session,
    ) -> Result<CheckpointCapture, CheckpointError> {
        self.begin_checkpoint(session, true)
    }

    /// Capture a dialogue boundary for the in-memory history window. The
    /// transcript is reattached by group identity if the player jumps back.
    pub fn capture_history_begin(
        &mut self,
        session: &Session,
    ) -> Result<CheckpointCapture, CheckpointError> {
        self.begin_checkpoint(session, false)
    }

    fn begin_checkpoint(
        &mut self,
        session: &Session,
        include_history: bool,
    ) -> Result<CheckpointCapture, CheckpointError> {
        self.live(session)?;
        let active = self
            .flow
            .active
            .as_ref()
            .ok_or(CheckpointError::Contract("no active business flow"))?;
        let root = self.resume.table().root.as_ref().unwrap().key.clone();
        let state = self.vm.heap().root_value(active.root_state)?;
        let context =
            ContextSlots::new(self.vm.heap(), self.vm.heap().root_value(active.context)?)?;
        let cursor = self
            .vm
            .capture_resume_cursor(&self.resume, active.task, active.boundary)?;
        let descriptor = cursor.descriptor().clone();
        let keys = vec![Vec::new(); descriptor.frames.len()];
        let metadata = self
            .vm
            .heap()
            .budget_usage()
            .reserve(metadata_bytes(&descriptor, &keys) + root.0.capacity() as u64)?;
        let media = self.media.capture_logical(self.vm.heap())?;
        let voice = self.dialogue_voice()?;
        let array = self.vm.heap_mut().alloc_array(&[state])?;
        let wrapper = self
            .vm
            .heap_mut()
            .add_root(self.root_owner, RootKind::Host, array)?;
        Ok(CheckpointCapture {
            owner: self.root_owner,
            root,
            cursor,
            keys,
            descriptor,
            wrapper: Some(wrapper),
            graph: None,
            reading: Some(ReadingCapture::with_history(
                session,
                self.checkpoint_display(),
                include_history,
            )),
            graph_complete: false,
            reading_bytes: 0,
            metadata: Some(metadata),
            complete: false,
            failed: false,
            required_work: 1,
            media: Some(media),
            voice,
            context: Some(context),
            member: None,
        })
    }
}
impl CheckpointCapture {
    /// Minimum native work required by the current operation descriptor.
    pub fn required_work(&self) -> u32 {
        self.required_work
    }
    /// Detached payload and temporary metadata retained while the source is frozen.
    pub fn retained_bytes(&self) -> u64 {
        self.metadata.as_ref().map_or(0, HeapReservation::bytes)
            + self.graph.as_ref().map_or(0, TypedCapture::retained_bytes)
            + self.media.as_ref().map_or(0, MediaCapture::retained_bytes)
            + self
                .context
                .as_ref()
                .map_or(0, ContextSlots::retained_bytes)
            + self.member.as_ref().map_or(0, |member| {
                member.name.retained_bytes() + member.charge.bytes()
            })
    }
    /// Consume at most `work` continuation/graph units. `chunk` bounds each copied
    /// string or byte segment. A failed capture cannot produce a record.
    pub fn step(
        &mut self,
        source: &mut SessionVm,
        session: &Session,
        budget: NativeBudget,
        chunk: usize,
    ) -> Result<bool, CheckpointError> {
        if self.failed {
            return Err(CheckpointError::Contract("continuation capture failed"));
        }
        source.live(session)?;
        let result = self.advance(source, session, budget, chunk);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn advance(
        &mut self,
        source: &mut SessionVm,
        session: &Session,
        budget: NativeBudget,
        chunk: usize,
    ) -> Result<bool, CheckpointError> {
        if let Some(media) = &mut self.media {
            match media.prepare(budget)? {
                NativeDispatch::Ready { .. } => self.required_work = 1,
                NativeDispatch::BudgetYield { required_work, .. } => {
                    self.required_work = required_work;
                    return Ok(false);
                }
                _ => {
                    self.required_work = media.required_work();
                    return Ok(false);
                }
            }
        }
        for _ in 0..budget.work() {
            if self.complete {
                return Ok(true);
            }
            if let Some(context) = &mut self.context
                && !context.complete()
            {
                context.step(source.vm.heap())?;
                continue;
            }
            if let Some(member) = &mut self.member {
                if member.name.step(source.vm.heap(), chunk)? {
                    let member = self.member.take().unwrap();
                    self.push_binding(
                        member.frame,
                        CheckpointBinding::CurrentContextMember {
                            key: member.key,
                            member: member.name.finish(),
                        },
                    )?;
                }
                continue;
            }
            if self.graph_complete {
                let reading = self.reading.as_mut().unwrap();
                self.complete = reading.step(session, source.checkpoint_display(), 1, chunk)?;
                let bytes = reading.retained_bytes();
                let metadata = self.metadata.as_mut().unwrap();
                metadata.resize(metadata.bytes() - self.reading_bytes + bytes)?;
                self.reading_bytes = bytes;
                continue;
            }
            if let Some(graph) = &mut self.graph {
                let references =
                    SceneCapture::new(&source.stage, &source.activities, source.read_limit)
                        .with_default_owner(
                            source.default_activity_owner(
                                source.flow.active.as_ref().unwrap().task,
                            )?,
                        )
                        .with_reading(session.reading_id)
                        .with_media(self.media.as_ref().unwrap());
                self.graph_complete = graph
                    .step(
                        source.vm.heap(),
                        source.flow.registry.as_ref().unwrap(),
                        &references,
                        1,
                        chunk,
                    )?
                    .0;
                if self.graph_complete {
                    self.media = None;
                }
                continue;
            }
            match self.cursor.step(&source.vm, 1)? {
                CaptureProgress::Binding(binding) => {
                    let frame = binding.frame;
                    let key = binding.key.clone();
                    let value = binding.value;
                    match self
                        .context
                        .as_ref()
                        .unwrap()
                        .source(source.vm.heap(), value)?
                    {
                        Source::Root => {
                            self.push_binding(frame, CheckpointBinding::CurrentContext(key))?
                        }
                        Source::Member(index) => {
                            self.member = Some(MemberBinding {
                                frame,
                                charge: source.vm.heap().budget_usage().reserve(
                                    (size_of::<MemberBinding>() + key.0.capacity()) as u64,
                                )?,
                                key,
                                name: self
                                    .context
                                    .as_ref()
                                    .unwrap()
                                    .copy_name(source.vm.heap(), index)?,
                            });
                        }
                        Source::Data => {
                            self.push_binding(frame, CheckpointBinding::Data(key))?;
                            let heap = source.vm.heap_mut();
                            let array = heap.root_value(self.wrapper.unwrap())?;
                            heap.array_push(array, value)?;
                        }
                    }
                }
                CaptureProgress::BudgetYield => {}
                CaptureProgress::Complete => {
                    self.context = None;
                    let heap = source.vm.heap_mut();
                    let array = heap.root_value(self.wrapper.unwrap())?;
                    if !self.voice.same_bits(Value::NULL) {
                        heap.array_push(array, self.voice)?;
                    }
                    self.graph = Some(TypedCapture::new(heap, self.owner, array)?);
                }
            }
        }
        Ok(self.complete)
    }
    fn push_binding(
        &mut self,
        frame: usize,
        binding: CheckpointBinding,
    ) -> Result<(), CheckpointError> {
        let before = self.keys[frame].capacity();
        let key_bytes = binding.string_bytes();
        self.keys[frame].push(binding);
        let added =
            (self.keys[frame].capacity() - before) * size_of::<CheckpointBinding>() + key_bytes;
        let metadata = self.metadata.as_mut().unwrap();
        metadata.resize(metadata.bytes() + added as u64)?;
        Ok(())
    }
    /// Finish only after all shared values have been detached from the source heap.
    pub fn finish(mut self, source: &mut SessionVm) -> Result<CapturedCheckpoint, CheckpointError> {
        if !self.complete || self.failed {
            self.cancel(source)?;
            return Err(CheckpointError::Contract("continuation capture incomplete"));
        }
        let result = self.graph.take().unwrap().finish(source.vm.heap_mut());
        source
            .vm
            .heap_mut()
            .release_root(self.wrapper.take().unwrap())?;
        let (values, graph) = result?;
        let record = CheckpointRecord::new(
            self.root.clone(),
            std::mem::take(&mut self.descriptor),
            std::mem::take(&mut self.keys),
            values,
        )
        .map_err(CheckpointError::Contract)?
        .with_reading(self.reading.take().unwrap().finish()?);
        Ok(CapturedCheckpoint {
            record: Arc::new(record),
            _graph: graph,
            _metadata: self.metadata.take().unwrap(),
        })
    }
    /// Release the temporary graph and source roots before resuming the live session.
    pub fn cancel(mut self, source: &mut SessionVm) -> Result<(), CheckpointError> {
        if let Some(graph) = self.graph.take() {
            graph.cancel(source.vm.heap_mut())?;
        }
        if let Some(root) = self.wrapper.take() {
            source.vm.heap_mut().release_root(root)?;
        }
        Ok(())
    }
}
fn metadata_bytes(descriptor: &ContinuationDescriptor, keys: &Vec<Vec<CheckpointBinding>>) -> u64 {
    (size_of::<CapturedCheckpoint>()
        + size_of::<CheckpointRecord>()
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
        + keys.capacity() * size_of::<Vec<CheckpointBinding>>()
        + keys
            .iter()
            .map(|v| {
                v.capacity() * size_of::<CheckpointBinding>()
                    + v.iter().map(CheckpointBinding::string_bytes).sum::<usize>()
            })
            .sum::<usize>()) as u64
}
