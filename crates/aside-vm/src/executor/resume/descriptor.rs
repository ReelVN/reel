//! Issued positions and waiting captures share one proof of actual business frames.
use super::*;
use crate::resume::{FlowKey, PlannedFrame, PlannedSite, PointKey, ResumeLimits, ReturnKey};

/// Constant-time admission bound for stack proof and one subsequent traversal.
#[derive(Clone, Copy, Debug)]
pub struct ResumeDescriptorBound {
    /// Maximum number of activations inspected by this proof.
    pub frames: usize,
    /// Work admitted before proving the stack and traversing its stable path.
    pub work: u64,
    /// Upper bound on the borrowed stable key bytes; no keys are copied.
    pub key_bytes: u64,
}
/// One borrowed stable site, independent of its current numeric code position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeSiteRef<'a> {
    Point(&'a PointKey),
    Return(&'a ReturnKey),
}
impl ResumeSiteRef<'_> {
    /// Borrow the site's stable source key without its point/return tag.
    pub fn key(&self) -> &str {
        match self {
            Self::Point(key) => &key.0,
            Self::Return(key) => &key.0,
        }
    }
}
/// A proved business activation's source identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResumeFrameRef<'a> {
    /// Stable identity of the ordinary business function.
    pub flow: &'a FlowKey,
    /// Its current point or suspended ordinary return site.
    pub site: ResumeSiteRef<'a>,
}
/// Read-only proof of the actual native call, borrowing current stack and metadata.
/// Its immutable VM borrow prevents the ticket or any proved return edge advancing.
#[derive(Clone, Copy)]
pub struct ResumeDescriptor<'a> {
    table: &'a VerifiedResumeTable,
    pub(super) scripts: &'a [Frame],
    native: NativeFrame,
    pub(super) base: usize,
    bound: ResumeDescriptorBound,
    key_bytes: u64,
}
/// Allocation-free iteration over an already proved business path.
pub struct ResumeFrames<'a> {
    descriptor: ResumeDescriptor<'a>,
    next: usize,
}
fn bound(
    table: &VerifiedResumeTable,
    state: &TaskState,
) -> Result<ResumeDescriptorBound, ResumeError> {
    let frames = state.frames.data.len();
    let (lookup, key) = table.current_bounds();
    let per_frame = lookup
        .checked_mul(4)
        .and_then(|v| v.checked_add(12))
        .ok_or_else(|| ResumeError::new("resume descriptor work overflow"))?;
    let work = (frames as u64)
        .checked_mul(per_frame)
        .and_then(|v| v.checked_add(8))
        .and_then(|v| v.checked_add(state.unwinds.data.len() as u64))
        .and_then(|v| v.checked_add(state.caught.data.len() as u64))
        .ok_or_else(|| ResumeError::new("resume descriptor work overflow"))?;
    let key_bytes = (frames as u64)
        .checked_mul(key)
        .ok_or_else(|| ResumeError::new("resume descriptor key-byte overflow"))?;
    Ok(ResumeDescriptorBound {
        frames,
        work,
        key_bytes,
    })
}
pub(super) fn same_program(vm: &Vm, table: &VerifiedResumeTable) -> Result<(), ResumeError> {
    if vm.program.snapshot_identity() != table.program.snapshot_identity() {
        return Err(ResumeError::new(
            "resume table belongs to another current program",
        ));
    }
    Ok(())
}
fn planned(
    table: &VerifiedResumeTable,
    scripts: &[Frame],
    native: NativeFrame,
    index: usize,
) -> Result<PlannedFrame, ResumeError> {
    let Frame::Script(script) = scripts[index] else {
        return Err(ResumeError::new(
            "native bridge inside a resumable business call chain",
        ));
    };
    if script.code.version != table.program.package().content_version
        || !matches!(script.phase, ScriptPhase::Body)
        || script.table_work.is_some()
        || script.derived_bridge.is_some()
    {
        return Err(
            ResumeError::new("script is not at its ordinary current-code call boundary")
                .at(script.code.function, script.pc),
        );
    }
    let child = scripts.get(index + 1);
    let ret = match child {
        Some(Frame::Script(child)) => child.ret,
        Some(Frame::Native(_)) => {
            return Err(ResumeError::new("native bridge inside resumable flow"));
        }
        None => native.ret,
    };
    let ReturnTo::Register { frame, reg, call } = ret else {
        return Err(ResumeError::new(
            "resume child does not return through an ordinary result register",
        ));
    };
    if frame != script.id {
        return Err(ResumeError::new(
            "resume return ownership differs from the active stack",
        ));
    }
    let (current, callee) = table
        .current_site(script.code.function, call)
        .ok_or_else(|| {
            ResumeError::new("active business call has no declared point")
                .at(script.code.function, call)
        })?;
    let flow = &table.table().flows[current.flow];
    let valid = match current.site {
        PlannedSite::Point(index) if child.is_none() => {
            let point = &flow.points[index];
            point.after_call == script.pc
                && point.result == reg
                && callee.is_some_and(|function| {
                    table.program.package().functions[function.index()].kind
                        == FunctionKind::Native(native.native)
                })
        }
        PlannedSite::Return(index) if child.is_some() => {
            let ret = &flow.returns[index];
            matches!(child, Some(Frame::Script(child)) if Some(child.code.function) == callee)
                && ret.after_call == script.pc
                && ret.result == reg
        }
        _ => false,
    };
    if !valid {
        return Err(ResumeError::new(
            "active call differs from its declared point or return contract",
        )
        .at(flow.function, call));
    }
    Ok(current)
}
fn match_stack<'a>(
    table: &'a VerifiedResumeTable,
    state: &'a TaskState,
    boundary: FrameId,
    native: NativeFrame,
    bound: ResumeDescriptorBound,
) -> Result<ResumeDescriptor<'a>, ResumeError> {
    let base = state
        .frames
        .data
        .iter()
        .position(|frame| frame.id() == boundary)
        .and_then(|index| match state.frames.data[index] {
            Frame::Native(native)
                if native.continuation_active
                    && matches!(
                        native.phase,
                        NativePhase::Callback { .. } | NativePhase::Continue
                    ) =>
            {
                Some(index + 1)
            }
            _ => None,
        })
        .ok_or_else(|| ResumeError::new("resume requires a current native callback boundary"))?;
    if state.unwinds.data.iter().any(|u| u.frame.0 > boundary.0)
        || state.caught.data.iter().any(|c| c.frame.0 > boundary.0)
        || state.fault_build.is_some()
        || state.cancelling
        || state.terminal.is_some()
    {
        return Err(ResumeError::new(
            "task is not at a stable ordinary call boundary",
        ));
    }
    let scripts = state
        .frames
        .data
        .get(base..state.frames.data.len() - 1)
        .ok_or_else(|| ResumeError::new("resume callback has no business activation"))?;
    let Some(Frame::Script(root)) = scripts.first() else {
        return Err(ResumeError::new("resume callback has no business root"));
    };
    if !matches!(root.ret, ReturnTo::Native { frame } if frame == boundary) {
        return Err(ResumeError::new(
            "business root does not return to the selected native boundary",
        ));
    }
    let mut descriptor = ResumeDescriptor {
        table,
        scripts,
        native,
        base,
        bound,
        key_bytes: 0,
    };
    for index in 0..scripts.len() {
        let frame = planned(table, scripts, native, index)?;
        let keys = descriptor.keys(frame);
        descriptor.key_bytes = descriptor
            .key_bytes
            .checked_add(keys.flow.0.len() as u64)
            .and_then(|v| v.checked_add(keys.site.key().len() as u64))
            .ok_or_else(|| ResumeError::new("resume descriptor key-byte overflow"))?;
    }
    Ok(descriptor)
}
impl Vm {
    /// Bound a logical waiting-stack query without copying frames or keys.
    pub fn resume_wait_bound(
        &self,
        table: &VerifiedResumeTable,
        task: TaskId,
    ) -> Result<ResumeDescriptorBound, ResumeError> {
        same_program(self, table)?;
        bound(table, self.task(task).map_err(error)?)
    }

    /// Check the current waiting operation and its stable business call chain.
    pub fn can_capture_resume(
        &self,
        table: &VerifiedResumeTable,
        task: TaskId,
        boundary: FrameId,
    ) -> bool {
        waiting(self, table, task, boundary).is_ok()
    }

    /// Admit only a real issued Entry ticket. This query scans no frames or keys.
    /// The work bound covers proof and one descriptor traversal; byte processing
    /// such as hashing or matching must additionally admit `key_bytes`.
    pub fn resume_descriptor_bound(
        &self,
        table: &VerifiedResumeTable,
        ticket: HostTicket,
    ) -> Result<ResumeDescriptorBound, ResumeError> {
        same_program(self, table)?;
        let state = self.task(ticket.task).map_err(error)?;
        let native = self.ticket_frame(state, ticket).map_err(error)?;
        if ticket.input != NativeInput::Entry || native.continuation_active {
            return Err(ResumeError::new(
                "native callback continuation is not a logical point",
            ));
        }
        bound(table, state)
    }
    /// Prove this real issued Entry is above the selected live business callback.
    /// No author value, frame, ticket or owned metadata is changed or allocated.
    pub fn resume_descriptor<'a>(
        &'a self,
        table: &'a VerifiedResumeTable,
        ticket: HostTicket,
        boundary: FrameId,
        limits: ResumeLimits,
    ) -> Result<ResumeDescriptor<'a>, ResumeError> {
        let cost = self.resume_descriptor_bound(table, ticket)?;
        if limits.work < cost.work {
            return Err(ResumeError::new("resume descriptor work limit"));
        }
        let state = self.task(ticket.task).map_err(error)?;
        let native = self.ticket_frame(state, ticket).map_err(error)?;
        match_stack(table, state, boundary, native, cost)
    }
}
pub(super) fn waiting<'a>(
    vm: &'a Vm,
    table: &'a VerifiedResumeTable,
    task: TaskId,
    boundary: FrameId,
) -> Result<(ResumeDescriptor<'a>, WaitId), ResumeError> {
    same_program(vm, table)?;
    let state = vm.task(task).map_err(error)?;
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
    Ok((
        match_stack(table, state, boundary, *native, bound(table, state)?)?,
        wait,
    ))
}
impl<'a> ResumeDescriptor<'a> {
    /// Number of ordinary business activations above the selected boundary.
    pub fn frame_count(&self) -> usize {
        self.scripts.len()
    }
    /// Exact stable key bytes visited by one complete path traversal.
    pub fn key_bytes(&self) -> u64 {
        self.key_bytes
    }
    /// Admitted work for proving and traversing this immutable business path.
    pub fn work_bound(&self) -> u64 {
        self.bound.work
    }
    /// Borrow stable keys from outermost return site to the leaf point.
    pub fn frames(&self) -> ResumeFrames<'a> {
        ResumeFrames {
            descriptor: *self,
            next: 0,
        }
    }
    /// Compare a saved path without allocating or resolving an executable plan.
    pub fn matches(&self, expected: &ContinuationDescriptor) -> bool {
        self.frame_count() == expected.frames.len()
            && self.frames().zip(&expected.frames).all(|(current, old)| {
                current.flow == &old.flow
                    && match (current.site, &old.site) {
                        (ResumeSiteRef::Point(a), ResumeSite::Point(b)) => a == b,
                        (ResumeSiteRef::Return(a), ResumeSite::Return(b)) => a == b,
                        _ => false,
                    }
            })
    }
    pub(super) fn planned_frame(&self, index: usize) -> PlannedFrame {
        planned(self.table, self.scripts, self.native, index)
            .expect("immutable stack already proved")
    }
    fn keys(&self, current: PlannedFrame) -> ResumeFrameRef<'a> {
        let flow = &self.table.table().flows[current.flow];
        ResumeFrameRef {
            flow: &flow.key,
            site: match current.site {
                PlannedSite::Point(index) => ResumeSiteRef::Point(&flow.points[index].key),
                PlannedSite::Return(index) => ResumeSiteRef::Return(&flow.returns[index].key),
            },
        }
    }
}
impl<'a> Iterator for ResumeFrames<'a> {
    type Item = ResumeFrameRef<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.descriptor.frame_count() {
            return None;
        }
        let current = self.descriptor.planned_frame(self.next);
        self.next += 1;
        Some(self.descriptor.keys(current))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.descriptor.frame_count() - self.next;
        (len, Some(len))
    }
}
impl ExactSizeIterator for ResumeFrames<'_> {}
