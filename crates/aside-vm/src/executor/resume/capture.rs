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
    frames: Vec<FrameId>,
    plan: VerifiedResumePlan,
    frame: usize,
    binding: usize,
}
/// One bounded step through saved business bindings.
pub enum CaptureProgress<'a> {
    Binding(CapturedBinding<'a>),
    BudgetYield,
    Complete,
}
impl Vm {
    /// Match the actual waiting stack against registered stable points and returns.
    /// The immutable borrow keeps values and their aliasing fixed for graph capture.
    pub fn capture_resume<'a>(
        &'a self,
        table: &VerifiedResumeTable,
        task: TaskId,
    ) -> Result<ResumeCapture<'a>, ResumeError> {
        Ok(ResumeCapture {
            vm: self,
            cursor: self.capture_resume_cursor(table, task)?,
        })
    }
    /// Match the stack once and return a detached cursor. Every later step checks
    /// the original wait and frame identities before borrowing an author value.
    pub fn capture_resume_cursor(
        &self,
        table: &VerifiedResumeTable,
        task: TaskId,
    ) -> Result<ResumeCaptureCursor, ResumeError> {
        if self.program.snapshot_identity() != table.program.snapshot_identity() {
            return Err(ResumeError::new(
                "resume table belongs to another current program",
            ));
        }
        let state = self.task(task).map_err(error)?;
        if !state.unwinds.data.is_empty()
            || !state.caught.data.is_empty()
            || state.fault_build.is_some()
            || state.cancelling
            || state.terminal.is_some()
        {
            return Err(ResumeError::new("task is not at a stable ordinary wait"));
        }
        let Some(Frame::Native(native)) = state.frames.data.last() else {
            return Err(ResumeError::new(
                "resume capture requires a native waiting operation",
            ));
        };
        let NativePhase::Waiting(wait) = native.phase else {
            return Err(ResumeError::new(
                "native callbacks and intermediate native phases are not logical points",
            ));
        };
        if native.continuation_active {
            return Err(ResumeError::new(
                "native callback continuation is not a logical point",
            ));
        }
        let scripts = &state.frames.data[..state.frames.data.len() - 1];
        let mut descriptor = ContinuationDescriptor::default();
        for (index, frame) in scripts.iter().enumerate() {
            let Frame::Script(script) = frame else {
                return Err(ResumeError::new(
                    "native bridge inside a resumable business call chain",
                ));
            };
            if !matches!(script.phase, ScriptPhase::Body)
                || script.table_work.is_some()
                || script.derived_bridge.is_some()
            {
                return Err(
                    ResumeError::new("script is not at its ordinary call boundary")
                        .at(script.code.function, script.pc),
                );
            }
            let flow = table
                .table()
                .flows
                .iter()
                .find(|flow| flow.function == script.code.function)
                .ok_or_else(|| {
                    ResumeError::new("active script function has no stable flow identity")
                        .at(script.code.function, script.pc)
                })?;
            let child = if index + 1 < scripts.len() {
                match scripts[index + 1] {
                    Frame::Script(child) => child.ret,
                    Frame::Native(_) => {
                        return Err(ResumeError::new("native bridge inside resumable flow"));
                    }
                }
            } else {
                native.ret
            };
            let ReturnTo::Register { frame, reg, call } = child else {
                return Err(ResumeError::new(
                    "resume child does not return through an ordinary result register",
                ));
            };
            if frame != script.id {
                return Err(ResumeError::new(
                    "resume return ownership differs from the active stack",
                ));
            }
            let site = if index + 1 == scripts.len() {
                let point = flow
                    .points
                    .iter()
                    .find(|point| {
                        point.call == call && point.after_call == script.pc && point.result == reg
                    })
                    .ok_or_else(|| {
                        ResumeError::new("active native wait has no declared point")
                            .at(flow.function, call)
                    })?;
                ResumeSite::Point(point.key.clone())
            } else {
                let ret = flow
                    .returns
                    .iter()
                    .find(|ret| {
                        ret.call == call && ret.after_call == script.pc && ret.result == reg
                    })
                    .ok_or_else(|| {
                        ResumeError::new("active ordinary call has no declared return point")
                            .at(flow.function, call)
                    })?;
                ResumeSite::Return(ret.key.clone())
            };
            descriptor.frames.push(FrameDescriptor {
                flow: flow.key.clone(),
                site,
            });
        }
        let plan = table.verify_plan(&descriptor)?;
        Ok(ResumeCaptureCursor {
            task,
            wait,
            frames: scripts.iter().map(|frame| frame.id()).collect(),
            plan,
            frame: 0,
            binding: 0,
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
        if !matches!(state.frames.data.last(), Some(Frame::Native(native)) if matches!(native.phase, NativePhase::Waiting(wait) if wait == self.wait))
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
            let Some(Frame::Script(script)) = state.frames.data.get(self.frame) else {
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
