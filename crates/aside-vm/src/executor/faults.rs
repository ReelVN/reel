//! FaultBuild, native retention and catch lifetime state machines.
use super::*;

impl Vm {
    pub(super) fn native_pending_slot(&self) -> u32 {
        2 + self.limits.max_continuation_values + self.limits.max_arguments
    }
    pub(super) fn native_fault_base(&self) -> u32 {
        self.native_pending_slot() + 1
    }
    pub(super) fn initialized_prefix(
        &self,
        range: RootRangeToken,
        start: u32,
        len: u32,
    ) -> Result<&[Value], VmError> {
        Ok(self
            .slots
            .borrow_initialized_window(range, if len == 0 { 0 } else { start }, len)?)
    }
    pub(super) fn check_new_fault(&self, new: NewFault) -> Result<(), VmError> {
        if new.code.registry_version != 1 {
            return Err(VmError::InvalidFault);
        };
        if let Some(id) = new.cause {
            self.heap.fault_info(id)?;
        }
        Ok(())
    }
    pub(super) fn native_fault_slot_holds(
        &self,
        n: NativeFrame,
        id: FaultId,
    ) -> Result<bool, VmError> {
        for i in 0..n.fault_count {
            if self
                .heap
                .fault_id(self.value(n.roots, self.native_fault_base() + i)?)?
                == id
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) fn native_holds(&self, n: NativeFrame, id: FaultId) -> Result<bool, VmError> {
        self.heap.fault_info(id)?;
        if self.native_fault_slot_holds(n, id)? {
            return Ok(true);
        }
        for i in 0..n.continuation_len {
            if self.value(n.roots, 2 + i)?.same_bits(id.as_value()) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) fn push_native_fault(
        &mut self,
        n: &mut NativeFrame,
        id: FaultId,
    ) -> Result<(), VmError> {
        if self.native_fault_slot_holds(*n, id)? {
            return Ok(());
        }
        if n.fault_count == self.limits.max_native_faults {
            return Err(VmError::Limit);
        };
        let len = self.native_fault_base() + n.fault_count + 1;
        self.slots.grow(&mut self.heap, n.roots, len)?;
        self.put(
            n.roots,
            self.native_fault_base() + n.fault_count,
            id.as_value(),
        )?;
        n.fault_count += 1;
        Ok(())
    }
    pub(super) fn retain_native_faults(
        &mut self,
        n: &mut NativeFrame,
        policy: FaultRetention<'_>,
    ) -> Result<(), VmError> {
        let FaultRetention::Replace(ids) = policy else {
            return Ok(());
        };
        self.validate_native_faults(ids)?;
        if !ids.is_empty() {
            let len = self.native_fault_base() + ids.len() as u32;
            self.slots.grow(&mut self.heap, n.roots, len)?;
        }
        self.write_native_faults(n, policy)
    }
    pub(super) fn validate_native_faults(&mut self, ids: &[FaultId]) -> Result<(), VmError> {
        if ids.len() > self.limits.max_native_faults as usize {
            return Err(VmError::Limit);
        };
        for &id in ids {
            self.heap.fault_info(id)?;
            self.heap.root_insertion_barrier(id.as_value())?;
        }
        Ok(())
    }
    pub(super) fn write_native_faults(
        &mut self,
        n: &mut NativeFrame,
        policy: FaultRetention<'_>,
    ) -> Result<(), VmError> {
        let FaultRetention::Replace(ids) = policy else {
            return Ok(());
        };
        for (i, &id) in ids.iter().enumerate() {
            self.put(n.roots, self.native_fault_base() + i as u32, id.as_value())?;
        }
        for i in ids.len() as u32..n.fault_count {
            self.clear(n.roots, self.native_fault_base() + i)?;
        }
        n.fault_count = ids.len() as u32;
        if n.callback_fault.is_some_and(|id| !ids.contains(&id)) {
            n.callback_fault = None;
        }
        Ok(())
    }
    pub fn replace_native_faults(
        &mut self,
        task: TaskId,
        frame: FrameId,
        ids: &[FaultId],
    ) -> Result<(), VmError> {
        self.select_task(task)?;
        let index = self
            .active
            .frames
            .data
            .iter()
            .position(|f| f.id() == frame)
            .ok_or(VmError::InvalidFault)?;
        let Frame::Native(mut n) = self.active.frames.data[index] else {
            return Err(VmError::InvalidFault);
        };
        self.retain_native_faults(&mut n, FaultRetention::Replace(ids))?;
        self.active.frames.data[index] = Frame::Native(n);
        Ok(())
    }
    pub fn error_info(&self, task: TaskId) -> Result<Option<FaultInfo>, VmError> {
        let t = self.task(task)?;
        let restricted = t.frames.data.last().is_some_and(|f| match f {
            Frame::Script(s) => s.context.derived,
            Frame::Native(n) => n.context.derived,
        });
        if restricted {
            return Err(VmError::Effect);
        };
        t.caught
            .data
            .iter()
            .max_by_key(|c| c.activation)
            .and_then(|c| c.info.fault)
            .map(|id| self.heap.fault_info(id).map_err(VmError::from))
            .transpose()
    }
    pub fn cancel_cooperatively(&mut self, task: TaskId) -> Result<(), VmError> {
        self.select_task(task)?;
        if self.active.terminal.is_some() || self.active.cancelling {
            return Err(VmError::Idle);
        }
        self.active.cooperative_cancel = true;
        Ok(())
    }
    pub(super) fn seed_cancellation(&mut self) -> Result<(), VmError> {
        let top = self.active.frames.data.len() - 1;
        match self.active.frames.data[top] {
            Frame::Script(mut s) => {
                let preparing = matches!(
                    s.phase,
                    ScriptPhase::Copy { .. }
                        | ScriptPhase::Rest
                        | ScriptPhase::Bind(_)
                        | ScriptPhase::Defaults { .. }
                        | ScriptPhase::DefaultCall { .. }
                        | ScriptPhase::DefaultConstant { .. }
                        | ScriptPhase::Parameter { .. }
                        | ScriptPhase::Install { .. }
                );
                self.active.abort_equality |= matches!(
                    s.phase,
                    ScriptPhase::Equality { .. }
                        | ScriptPhase::Search(_)
                        | ScriptPhase::EqualityCleanup { .. }
                        | ScriptPhase::TableValidate
                        | ScriptPhase::KeyPath { .. }
                );
                let explicit = match s.phase {
                    ScriptPhase::Exit {
                        reason: ExitReason::Throw(seed),
                        ..
                    }
                    | ScriptPhase::ExitApply {
                        reason: ExitReason::Throw(seed),
                        ..
                    }
                    | ScriptPhase::Release {
                        reason: ExitReason::Throw(seed),
                    } => seed.fault,
                    _ => None,
                };
                s.table_work = None;
                self.put(s.env, 1, Value::NULL)?;
                if let Some(id) = explicit {
                    self.put(s.env, 2, id.as_value())?;
                } else {
                    self.clear(s.env, 2)?;
                }
                let reason = ExitReason::Throw(FaultSeed {
                    kind: FaultKind::Cancelled,
                    location: if preparing {
                        self.binding_location(s)
                    } else {
                        self.loc(s, s.pc)
                    },
                    fault: None,
                    explicit,
                    author_value: false,
                    awaited: None,
                });
                s.phase = if preparing {
                    ScriptPhase::Release { reason }
                } else {
                    ScriptPhase::Exit {
                        origin: s.pc,
                        reason,
                        skip: None,
                    }
                };
                self.active.frames.data[top] = Frame::Script(s);
            }
            Frame::Native(mut n) => {
                let explicit = n.pending_fault.and_then(|s| s.fault);
                self.put(n.roots, 0, Value::NULL)?;
                if let Some(id) = explicit {
                    self.put(n.roots, self.native_pending_slot(), id.as_value())?;
                } else {
                    self.clear_native_pending(n.roots)?;
                }
                n.pending_fault = Some(FaultSeed {
                    kind: FaultKind::Cancelled,
                    location: n.location,
                    fault: None,
                    explicit,
                    author_value: false,
                    awaited: None,
                });
                n.phase = NativePhase::Commit(true);
                self.active.frames.data[top] = Frame::Native(n);
            }
        }
        Ok(())
    }
    pub(super) fn unbuilt_fault(&self) -> Option<(FrameId, FaultSeed)> {
        match self.active.frames.data.last()? {
            Frame::Script(s) => match &s.phase {
                ScriptPhase::Exit {
                    reason: ExitReason::Throw(seed),
                    ..
                }
                | ScriptPhase::Release {
                    reason: ExitReason::Throw(seed),
                } if seed.fault.is_none() => Some((s.id, *seed)),
                _ => None,
            },
            Frame::Native(n) => {
                if matches!(
                    n.phase,
                    NativePhase::Commit(true)
                        | NativePhase::Release(true)
                        | NativePhase::CallbackFaultBuild
                        | NativePhase::WaitingFaultBuild
                ) {
                    n.pending_fault
                        .filter(|s| s.fault.is_none())
                        .map(|s| (n.id, s))
                } else {
                    None
                }
            }
        }
    }
    pub(super) fn build_roots(
        &self,
        frame: FrameId,
    ) -> Result<(RootRangeToken, u32, u32), VmError> {
        match self.active.frames.data[self.index(frame)?] {
            Frame::Script(s) => Ok((s.env, 1, 2)),
            Frame::Native(n) => Ok((n.roots, 0, self.native_pending_slot())),
        }
    }
    pub(super) fn block(&mut self, resource: BlockedResource, current: u64, required: u64) {
        self.active.blocked = Some(ResourceBlocked {
            task: self.active.task_id.unwrap(),
            resource,
            current,
            required,
        });
    }
    pub(super) fn block_heap(&mut self, error: HeapError) -> Result<(), VmError> {
        match error {
            HeapError::ObjectCapacity => {
                let s = self.heap.stats();
                let current = (s.live_objects + s.pending_objects + s.retired_objects) as u64;
                self.block(BlockedResource::HeapObjects, current, current + 1)
            }
            HeapError::ResourceLimit {
                requested, used, ..
            } => self.block(BlockedResource::HeapBytes, used, used + requested),
            HeapError::RootRangeCapacity => {
                let current = self.heap.stats().root_ranges as u64;
                self.block(BlockedResource::RootRanges, current, current + 1)
            }
            HeapError::WindowCapacity => {
                let current = self.slots.stats().active_values as u64;
                self.block(BlockedResource::WindowValues, current, current + 2)
            }
            other => return Err(VmError::Heap(other)),
        }
        Ok(())
    }
    pub(super) fn stack_entry(&self, index: usize) -> StackEntry {
        let version = self.program.package().content_version;
        match self.active.frames.data[index] {
            Frame::Script(s) => {
                let pc = if let Some(next) = self.active.frames.data.get(index + 1) {
                    let ret = match next {
                        Frame::Script(s) => s.ret,
                        Frame::Native(n) => n.ret,
                    };
                    match ret {
                        ReturnTo::Register { frame, call, .. }
                        | ReturnTo::Default { frame, call, .. }
                        | ReturnTo::ReactiveRead { frame, call }
                            if frame == s.id =>
                        {
                            call
                        }
                        _ => s.pc,
                    }
                } else {
                    s.pc
                };
                StackEntry {
                    frame: s.id,
                    location: self.loc(s, pc),
                    module: s.module,
                    module_version: version,
                    source_version: version,
                    kind: StackFrameKind::Script,
                }
            }
            Frame::Native(n) => StackEntry {
                frame: n.id,
                location: n.location,
                module: n.module,
                module_version: version,
                source_version: version,
                kind: StackFrameKind::Native {
                    native: n.native,
                    abi_version: self.program.package().natives[n.native.index()].abi_version,
                    continuation: ContinuationState {
                        version: n.continuation_version,
                        stage: n.continuation_stage,
                    },
                },
            },
        }
    }
    pub(super) fn fault_build_step(&mut self, m: &mut Meter) -> Result<bool, VmError> {
        let mut build = self.active.fault_build.unwrap();
        let alloc = match build.phase {
            BuildPhase::ErrorValue
                if !build.seed.author_value && build.seed.kind != FaultKind::Thrown =>
            {
                crate::fault::FAULT_NODE_BYTES
            }
            BuildPhase::Envelope => crate::fault::FAULT_NODE_BYTES,
            BuildPhase::Stack(n) | BuildPhase::CompactScriptStack(n)
                if (n as usize) < self.active.frames.data.len() =>
            {
                crate::fault::FAULT_NODE_BYTES
            }
            BuildPhase::Cause if build.seed.explicit.or(build.implicit).is_some() => {
                crate::fault::FAULT_NODE_BYTES
            }
            BuildPhase::Awaited if build.seed.awaited.is_some() => crate::fault::FAULT_NODE_BYTES,
            _ => 0,
        };
        let work = match build.phase {
            BuildPhase::Stack(n) | BuildPhase::CompactScriptStack(n)
                if (n as usize) < self.active.frames.data.len() =>
            {
                let (function, extra) = match self.active.frames.data[n as usize] {
                    Frame::Script(s)
                        if s.derived_bridge.is_some()
                            && matches!(build.phase, BuildPhase::Stack(_)) =>
                    {
                        let output = self.value(s.env, 3)?;
                        let function = FunctionId(
                            self.heap
                                .host_payload(output, crate::heap::DerivedSlot::Origin as u32)?
                                .as_int()
                                .unwrap() as u32,
                        );
                        (function, 6)
                    }
                    Frame::Script(s) => (s.code.function, 0),
                    Frame::Native(n) => (n.location.code.function, 0),
                };
                extra
                    + crate::bytecode::SourceMapSequence::lookup_work(
                        &self.program.package().functions[function.index()].source_map,
                    )
            }
            _ => 1,
        };
        let (roots, value_slot, fault_slot) = self.build_roots(build.frame)?;
        let growth = if matches!(build.phase, BuildPhase::Complete) {
            let Some(plan) = self.native_root_admission(roots, fault_slot + 1)? else {
                return Ok(true);
            };
            Some(plan)
        } else {
            None
        };
        if !m.take(
            0,
            work + growth.as_ref().map_or(0, |p| p.work),
            alloc + growth.as_ref().map_or(0, |p| p.allocation_bytes),
        ) {
            return Ok(false);
        };
        if growth.is_some()
            && let Err(error) = self.slots.grow(&mut self.heap, roots, fault_slot + 1)
        {
            self.block_heap(error)?;
            return Ok(true);
        }
        match build.phase {
            BuildPhase::ErrorValue => {
                if !build.seed.author_value && build.seed.kind != FaultKind::Thrown {
                    let code = FaultCode::core(build.seed.kind);
                    let details = self.value(roots, value_slot)?;
                    match self.heap.alloc_fault_node(
                        ObjectKind::CoreError,
                        FaultData::Error(code),
                        [details, Value::NULL, Value::NULL],
                        self.program.clone(),
                    ) {
                        Ok(value) => self.put(roots, value_slot, value)?,
                        Err(e) => {
                            self.block_heap(e)?;
                            return Ok(true);
                        }
                    }
                }
                build.phase = BuildPhase::Envelope;
            }
            BuildPhase::Envelope => {
                let value = self.value(roots, value_slot)?;
                match self.heap.alloc_fault_node(
                    ObjectKind::FaultEnvelope,
                    FaultData::Envelope {
                        code: FaultCode::core(build.seed.kind),
                        origin: build.seed.location,
                        complete: false,
                    },
                    [value, Value::NULL, Value::NULL],
                    self.program.clone(),
                ) {
                    Ok(envelope) => self.put(self.active.delivery.unwrap(), 0, envelope)?,
                    Err(e) => {
                        self.block_heap(e)?;
                        return Ok(true);
                    }
                }
                build.phase = BuildPhase::Stack(0);
            }
            BuildPhase::Stack(cursor) | BuildPhase::CompactScriptStack(cursor) => {
                if cursor as usize == self.active.frames.data.len() {
                    build.phase = BuildPhase::Cause;
                } else {
                    let envelope = self.value(self.active.delivery.unwrap(), 0)?;
                    let tail = self.heap.unfinished_stack(envelope)?;
                    let compact = if matches!(build.phase, BuildPhase::Stack(_)) {
                        match self.active.frames.data[cursor as usize] {
                            Frame::Script(s) if s.derived_bridge.is_some() => Some(s),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    let entry = if let Some(script) = compact {
                        self.compact_stack_entry(script)?
                    } else {
                        self.stack_entry(cursor as usize)
                    };
                    match self.heap.alloc_fault_node(
                        ObjectKind::FaultStack,
                        FaultData::Stack(entry),
                        [tail, Value::NULL, Value::NULL],
                        self.program.clone(),
                    ) {
                        Ok(block) => self.heap.fault_edge(envelope, 1, block)?,
                        Err(e) => {
                            self.block_heap(e)?;
                            return Ok(true);
                        }
                    }
                    build.phase = if compact.is_some() {
                        BuildPhase::CompactScriptStack(cursor)
                    } else {
                        BuildPhase::Stack(cursor + 1)
                    };
                }
            }
            BuildPhase::Cause => {
                if let Some(cause) = build.seed.explicit.or(build.implicit) {
                    let value = self.value(self.active.delivery.unwrap(), 0)?;
                    let id = FaultId(self.heap.handle(value)?);
                    let kind = if build.seed.explicit.is_some() {
                        CauseKind::ExplicitCause
                    } else {
                        CauseKind::CaughtFault
                    };
                    if let Err(e) = self.heap.append_fault_cause(
                        id,
                        CauseInfo {
                            event: build.event,
                            kind,
                            location: build.seed.location,
                            fault: Some(cause),
                            value: Value::NULL,
                        },
                        self.program.clone(),
                    ) {
                        self.block_heap(e)?;
                        return Ok(true);
                    }
                }
                build.phase = if build.seed.awaited.is_some() {
                    BuildPhase::Awaited
                } else {
                    BuildPhase::Complete
                };
            }
            BuildPhase::Awaited => {
                if let Some((source, activation)) = build.seed.awaited {
                    let envelope = self.value(self.active.delivery.unwrap(), 0)?;
                    let id = FaultId(self.heap.handle(envelope)?);
                    if let Err(error) = self.heap.append_fault_cause(
                        id,
                        CauseInfo {
                            event: activation,
                            kind: CauseKind::AwaitedFault { activation },
                            location: build.seed.location,
                            fault: Some(source),
                            value: Value::NULL,
                        },
                        self.program.clone(),
                    ) {
                        self.block_heap(error)?;
                        return Ok(true);
                    }
                }
                build.phase = BuildPhase::Complete;
            }
            BuildPhase::Complete => {
                let envelope = self.value(self.active.delivery.unwrap(), 0)?;
                let id = self.heap.finish_fault(envelope)?;
                self.put(roots, fault_slot, envelope)?;
                build.seed.fault = Some(id);
                let top = self.index(build.frame)?;
                match self.active.frames.data[top] {
                    Frame::Script(mut s) => {
                        s.phase = match s.phase {
                            ScriptPhase::Exit { origin, skip, .. } => ScriptPhase::Exit {
                                origin,
                                reason: ExitReason::Throw(build.seed),
                                skip,
                            },
                            ScriptPhase::Release { .. } => ScriptPhase::Release {
                                reason: ExitReason::Throw(build.seed),
                            },
                            _ => return Err(VmError::Contract("FaultBuild script stage")),
                        };
                        self.active.frames.data[top] = Frame::Script(s);
                    }
                    Frame::Native(mut n) => {
                        n.pending_fault = Some(build.seed);
                        self.active.frames.data[top] = Frame::Native(n);
                    }
                }
                self.clear(self.active.delivery.unwrap(), 0)?;
                self.active.fault_build = None;
                self.active.blocked = None;
                return Ok(true);
            }
        }
        self.active.fault_build = Some(build);
        self.active.blocked = None;
        Ok(true)
    }
    pub(super) fn release_exited_catch(
        &mut self,
        s: ScriptFrame,
        target: Option<Pc>,
    ) -> Result<bool, VmError> {
        if let Some(index) = self.active.caught.data.iter().position(|c| {
            c.frame == s.id
                && target.is_none_or(|pc| {
                    !self.program.package().functions[s.code.function.index()].regions
                        [c.region.index()]
                    .catch
                    .as_ref()
                    .unwrap()
                    .range
                    .contains(pc)
                })
        }) {
            let c = self.active.caught.data.remove(index);
            self.clear(s.env, 3 + c.region.0 * 2)?;
            self.clear(s.env, 4 + c.region.0 * 2)?;
            return Ok(true);
        }
        Ok(false)
    }
}
