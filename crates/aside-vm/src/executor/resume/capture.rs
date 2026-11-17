use super::*;

/// One business binding read under the capture's immutable VM borrow.
pub struct CapturedBinding<'a> {
    pub frame: usize,
    pub key: &'a BindingKey,
    pub value: Value,
}
/// Frozen access to one actual waiting task. Native execution frames are not saved.
pub struct ResumeCapture<'a> {
    vm: &'a Vm,
    cursor: ResumeCaptureCursor,
}
/// A checked cursor without a VM borrow or retained author values. The host freezes
/// business execution while interleaving reads with rooted persistent-graph writes.
pub struct ResumeCaptureCursor {
    task: TaskId,
    wait: WaitId,
    boundary: FrameId,
    base: usize,
    frames: Vec<FrameId>,
    plan: VerifiedResumePlan,
    frame: usize,
    binding: usize,
    _charge: HeapReservation,
}
/// One bounded step through saved business bindings.
pub enum CaptureProgress<'a> {
    Binding(CapturedBinding<'a>),
    BudgetYield,
    Complete,
}
impl Vm {
    /// Match business frames above a current native callback boundary.
    /// The immutable borrow keeps values and their aliasing fixed for graph capture.
    pub fn capture_resume<'a>(
        &'a self,
        table: &VerifiedResumeTable,
        task: TaskId,
        boundary: FrameId,
    ) -> Result<ResumeCapture<'a>, ResumeError> {
        Ok(ResumeCapture {
            vm: self,
            cursor: self.capture_resume_cursor(table, task, boundary)?,
        })
    }
    /// Match the stack once and return a detached cursor. Every later step checks
    /// the original wait and frame identities before borrowing an author value.
    pub fn capture_resume_cursor(
        &self,
        table: &VerifiedResumeTable,
        task: TaskId,
        boundary: FrameId,
    ) -> Result<ResumeCaptureCursor, ResumeError> {
        let (proof, wait) = super::descriptor::waiting(self, table, task, boundary)?;
        let count = proof.frame_count();
        let bytes = count
            .checked_mul(
                size_of::<FrameId>()
                    + size_of::<crate::resume::PlannedFrame>()
                    + size_of::<FrameDescriptor>(),
            )
            .and_then(|v| v.checked_add(size_of::<ResumeCaptureCursor>()))
            .and_then(|v| v.checked_add(usize::try_from(proof.key_bytes()).ok()?))
            .ok_or_else(|| ResumeError::new("resume capture metadata size"))?;
        let mut charge = self
            .heap
            .budget_usage()
            .reserve(bytes as u64)
            .map_err(error)?;
        let mut frames = Vec::new();
        let mut planned = Vec::new();
        let mut descriptor = ContinuationDescriptor::default();
        frames.try_reserve_exact(count).map_err(error)?;
        planned.try_reserve_exact(count).map_err(error)?;
        descriptor.frames.try_reserve_exact(count).map_err(error)?;
        let actual = frames.capacity() * size_of::<FrameId>()
            + planned.capacity() * size_of::<crate::resume::PlannedFrame>()
            + descriptor.frames.capacity() * size_of::<FrameDescriptor>();
        let inline = count
            * (size_of::<FrameId>()
                + size_of::<crate::resume::PlannedFrame>()
                + size_of::<FrameDescriptor>());
        charge
            .resize(charge.bytes() + (actual - inline) as u64)
            .map_err(error)?;
        for (index, keys) in proof.frames().enumerate() {
            frames.push(proof.scripts[index].id());
            planned.push(proof.planned_frame(index));
            let site = match keys.site {
                ResumeSiteRef::Point(key) => ResumeSite::Point(key.clone()),
                ResumeSiteRef::Return(key) => ResumeSite::Return(key.clone()),
            };
            descriptor.frames.push(FrameDescriptor {
                flow: keys.flow.clone(),
                site,
            });
        }
        let key_capacity: usize = descriptor
            .frames
            .iter()
            .map(|frame| {
                frame.flow.0.capacity()
                    + match &frame.site {
                        ResumeSite::Point(key) => key.0.capacity(),
                        ResumeSite::Return(key) => key.0.capacity(),
                    }
            })
            .sum();
        charge
            .resize(charge.bytes() + (key_capacity as u64 - proof.key_bytes()))
            .map_err(error)?;
        Ok(ResumeCaptureCursor {
            task,
            wait,
            boundary,
            base: proof.base,
            frames,
            plan: VerifiedResumePlan::from_matching_stack(table.clone(), planned, descriptor),
            frame: 0,
            binding: 0,
            _charge: charge,
        })
    }
}
impl ResumeCapture<'_> {
    /// Stable descriptors can be encoded independently of code and runtime values.
    pub fn descriptor(&self) -> &ContinuationDescriptor {
        self.cursor.descriptor()
    }
    /// Release the immutable VM borrow while retaining the checked read position.
    pub fn into_cursor(self) -> ResumeCaptureCursor {
        self.cursor
    }
    /// Read at most one binding. Values remain valid while this capture is borrowed.
    pub fn next_binding(&mut self) -> Result<Option<CapturedBinding<'_>>, ResumeError> {
        self.cursor.next_binding(self.vm)
    }
}
impl ResumeCaptureCursor {
    /// Stable source identities, with no package-local PCs or author values.
    pub fn descriptor(&self) -> &ContinuationDescriptor {
        self.plan.descriptor()
    }
    /// Convenience traversal; use `step` to bound empty-activation traversal too.
    pub fn next_binding(&mut self, vm: &Vm) -> Result<Option<CapturedBinding<'_>>, ResumeError> {
        match self.step(vm, u64::MAX)? {
            CaptureProgress::Binding(binding) => Ok(Some(binding)),
            CaptureProgress::Complete => Ok(None),
            CaptureProgress::BudgetYield => unreachable!(),
        }
    }
    /// Read at most one binding, charging one unit for each visited activation.
    pub fn step(&mut self, vm: &Vm, mut work: u64) -> Result<CaptureProgress<'_>, ResumeError> {
        if vm.program.snapshot_identity() != self.plan.verified.program.snapshot_identity() {
            return Err(ResumeError::new(
                "capture cursor belongs to another program",
            ));
        }
        let state = vm.task(self.task).map_err(error)?;
        if state.cancelling
            || state.frames.data.len() != self.base + self.plan.frame_count() + 1
            || !matches!(state.frames.data.get(self.base - 1), Some(Frame::Native(native))
                if native.id == self.boundary && native.continuation_active)
            || !matches!(state.frames.data.last(), Some(Frame::Native(native)) if matches!(native.phase, NativePhase::Waiting(wait) if wait == self.wait))
        {
            return Err(ResumeError::new(
                "capture cursor's original wait has advanced",
            ));
        }
        while self.frame < self.plan.frame_count() {
            if work == 0 {
                return Ok(CaptureProgress::BudgetYield);
            }
            work -= 1;
            let (_, site) = self.plan.frame(self.frame);
            if self.binding == site.layout.bindings.len() {
                self.frame += 1;
                self.binding = 0;
                continue;
            }
            let binding = &site.layout.bindings[self.binding];
            let Some(Frame::Script(script)) = state.frames.data.get(self.base + self.frame) else {
                return Err(ResumeError::new("capture activation disappeared"));
            };
            if script.id != self.frames[self.frame] {
                return Err(ResumeError::new("capture activation identity changed"));
            }
            let def = &vm.program.package().functions[script.code.function.index()].bindings
                [binding.binding.index()];
            let value = vm.value(script.regs, def.storage.reg().0).map_err(error)?;
            let value = match def.storage {
                BindingStorage::Value(_) => value,
                BindingStorage::Cell(_) => vm.heap.cell_read(value).map_err(error)?,
            };
            self.binding += 1;
            return Ok(CaptureProgress::Binding(CapturedBinding {
                frame: self.frame,
                key: &binding.key,
                value,
            }));
        }
        Ok(CaptureProgress::Complete)
    }
}
