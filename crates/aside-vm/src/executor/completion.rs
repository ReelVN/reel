//! Equality, stack exit and native continuation execution.
use super::*;

impl Vm {
    pub(super) fn equality_step(
        &mut self,
        s: &mut ScriptFrame,
        dest: Reg,
        negated: bool,
    ) -> Result<(), VmError> {
        if self.table_changed(*s)? {
            s.phase = ScriptPhase::EqualityCleanup {
                dest,
                negated,
                result: Err(FaultKind::ConcurrentModification),
            };
            return Ok(());
        }
        if let Some(result) = self.equality_compare_step()? {
            s.phase = ScriptPhase::EqualityCleanup {
                dest,
                negated,
                result,
            };
        }
        Ok(())
    }

    pub(super) fn equality_compare_step(
        &mut self,
    ) -> Result<Option<Result<bool, FaultKind>>, VmError> {
        let depth = self.active.equality_cursors.data.len() as u32 - 1;
        let offset = self.active.equality_cursors.data[depth as usize];
        let left = self.value(self.active.equality_roots.unwrap(), depth * 2)?;
        let right = self.value(self.active.equality_roots.unwrap(), depth * 2 + 1)?;
        let result = if left.tag() == ValueTag::Object && right.tag() == ValueTag::Object {
            let l = self.heap.info(left)?;
            let r = self.heap.info(right)?;
            if l.kind != r.kind {
                Some(false)
            } else {
                match l.kind {
                    ObjectKind::String | ObjectKind::Bytes => {
                        if l.len != r.len {
                            Some(false)
                        } else if offset == l.len {
                            Some(true)
                        } else {
                            let mut a = [0];
                            let mut b = [0];
                            self.heap.read_bytes(left, offset, &mut a)?;
                            self.heap.read_bytes(right, offset, &mut b)?;
                            self.active.equality_cursors.data[depth as usize] += 1;
                            if a != b { Some(false) } else { None }
                        }
                    }
                    ObjectKind::Tuple => {
                        if l.len != r.len {
                            Some(false)
                        } else if offset == l.len {
                            Some(true)
                        } else {
                            if self.active.equality_cursors.data.len()
                                == self.limits.max_unwinds as usize
                            {
                                return Ok(Some(Err(FaultKind::Resource)));
                            }
                            let a = self.heap.get_field(left, offset)?;
                            let b = self.heap.get_field(right, offset)?;
                            self.active.equality_cursors.data[depth as usize] += 1;
                            self.put(self.active.equality_roots.unwrap(), (depth + 1) * 2, a)?;
                            self.put(self.active.equality_roots.unwrap(), (depth + 1) * 2 + 1, b)?;
                            self.active.equality_cursors.data.push(0);
                            None
                        }
                    }
                    _ => Some(left.same_bits(right)),
                }
            }
        } else {
            Some(
                binary(Opcode::Eq, left, right)
                    .map_err(|_| VmError::Contract("equality scalar"))?
                    .as_bool()
                    .unwrap(),
            )
        };
        if let Some(equal) = result {
            if !equal {
                return Ok(Some(Ok(false)));
            } else {
                self.clear(self.active.equality_roots.unwrap(), depth * 2)?;
                self.clear(self.active.equality_roots.unwrap(), depth * 2 + 1)?;
                self.active.equality_cursors.data.pop();
                if self.active.equality_cursors.data.is_empty() {
                    return Ok(Some(Ok(true)));
                }
            }
        }
        Ok(None)
    }
    pub(super) fn select_exit_handler(
        &self,
        s: ScriptFrame,
        origin: Pc,
        reason: ExitReason,
        skip: Option<RegionId>,
    ) -> Result<Option<HandlerTarget>, VmError> {
        #[cfg(test)]
        self.work_probe
            .exit_handler_queries
            .set(self.work_probe.exit_handler_queries.get() + 1);
        Ok(match reason {
            ExitReason::Throw(_) => {
                self.program
                    .exception_handler(s.code.function, origin, skip)?
            }
            ExitReason::Return | ExitReason::GeneratorClose { .. } => self
                .program
                .cleanup_handler(s.code.function, origin, CleanupDestination::Return, skip)?,
            ExitReason::Leave(target) => self.program.cleanup_handler(
                s.code.function,
                origin,
                CleanupDestination::Leave(target),
                skip,
            )?,
        })
    }
    pub(super) fn superseded_unwind(
        &self,
        s: ScriptFrame,
        origin: Pc,
        reason: ExitReason,
        handler: Option<HandlerTarget>,
    ) -> Option<Unwind> {
        let u = self
            .active
            .unwinds
            .data
            .last()
            .copied()
            .filter(|u| u.frame == s.id)?;
        let range = self.program.package().functions[s.code.function.index()].regions
            [u.region.index()]
        .finally
        .unwrap();
        (range.contains(origin)
            && !handler.is_some_and(|h| range.contains(h.pc))
            && !matches!(reason, ExitReason::Leave(target) if range.contains(target)))
        .then_some(u)
    }
    pub(super) fn exit_step(
        &mut self,
        s: &mut ScriptFrame,
        origin: Pc,
        reason: ExitReason,
        handler: Option<HandlerTarget>,
        superseded: Option<Unwind>,
    ) -> Result<bool, VmError> {
        // A new error/return inside an active finally supersedes that exit. Nested
        // ordinary Leave stays within the outer cleanup and preserves its record.
        if let Some(u) = superseded {
            if let ExitReason::Throw(info) = reason {
                let id = info
                    .fault
                    .ok_or(VmError::Contract("incomplete unwind fault"))?;
                let (kind, old_fault, old_value) = match u.reason {
                    ExitReason::Throw(old) => (
                        CauseKind::ReplacedExit(ReplacedExit::Throw(
                            old.fault
                                .ok_or(VmError::Contract("incomplete pending fault"))?,
                        )),
                        old.fault,
                        Value::NULL,
                    ),
                    ExitReason::Return => (
                        CauseKind::ReplacedExit(ReplacedExit::Return),
                        None,
                        self.value(u.value, 0)?,
                    ),
                    ExitReason::GeneratorClose {
                        logical_id,
                        activation,
                    } => (
                        CauseKind::ReplacedExit(ReplacedExit::Close {
                            kind: CloseKind::Generator,
                            logical_id,
                            activation,
                        }),
                        None,
                        Value::NULL,
                    ),
                    ExitReason::Leave(target) => (
                        CauseKind::ReplacedExit(ReplacedExit::Leave {
                            code: s.code,
                            target,
                        }),
                        None,
                        Value::NULL,
                    ),
                };
                let event = if let Some(e) = u.replacement_event {
                    e
                } else {
                    let e = self.serial()?;
                    self.active
                        .unwinds
                        .data
                        .last_mut()
                        .unwrap()
                        .replacement_event = Some(e);
                    e
                };
                if let Err(e) = self.heap.append_fault_cause(
                    id,
                    CauseInfo {
                        event,
                        kind,
                        location: self.loc(*s, origin),
                        fault: old_fault,
                        value: old_value,
                    },
                    self.program.clone(),
                ) {
                    self.block_heap(e)?;
                    return Ok(true);
                }
            }
            self.slots.release(&mut self.heap, u.value)?;
            self.active.unwinds.data.pop();
            return Ok(true);
        }
        let target = handler.map(|h| h.pc).or(match reason {
            ExitReason::Leave(target) => Some(target),
            _ => None,
        });
        if self.release_exited_catch(*s, target)? {
            return Ok(true);
        }
        if let Some(handler) = handler {
            let value = self.value(s.env, 1)?;
            match handler.kind {
                HandlerKind::Catch => {
                    let ExitReason::Throw(info) = reason else {
                        return Err(VmError::Contract("catch on normal exit"));
                    };
                    let base = 4 + handler.region.0 * 2;
                    self.put(s.env, base, value)?;
                    if info.fault.is_some() {
                        let cause = self.value(s.env, 2)?;
                        self.put(s.env, base + 1, cause)?;
                    } else {
                        self.clear(s.env, base + 1)?;
                    }
                    if let Some(index) = self
                        .active
                        .caught
                        .data
                        .iter()
                        .position(|c| c.frame == s.id && c.region == handler.region)
                    {
                        self.active.caught.data[index].info = info;
                    } else {
                        if self.active.caught.data.len() == self.limits.max_unwinds as usize {
                            self.block(
                                BlockedResource::CatchRecords,
                                self.active.caught.data.len() as u64,
                                self.active.caught.data.len() as u64 + 1,
                            );
                            return Ok(true);
                        }
                        let activation = self.serial()?;
                        self.active.caught.data.push(CaughtFault {
                            frame: s.id,
                            region: handler.region,
                            info,
                            activation,
                        });
                    }
                    self.put(s.regs, handler.exception.unwrap().0, value)?;
                    self.clear(s.env, 1)?;
                    self.clear(s.env, 2)?;
                    s.pc = handler.pc;
                    s.phase = ScriptPhase::Body;
                }
                HandlerKind::Finally => {
                    if self.active.unwinds.data.len() == self.limits.max_unwinds as usize {
                        self.block(
                            BlockedResource::UnwindRecords,
                            self.active.unwinds.data.len() as u64,
                            self.active.unwinds.data.len() as u64 + 1,
                        );
                        return Ok(true);
                    }
                    let root = match self
                        .slots
                        .admit_windows(&mut self.heap, 1, 2)
                        .map_err(VmError::Heap)
                        .and_then(|()| self.open(s.owner, RootKind::Exception, 2))
                    {
                        Ok(r) => r,
                        Err(e) => {
                            match e {
                                VmError::Heap(e) => self.block_heap(e)?,
                                other => return Err(other),
                            };
                            return Ok(true);
                        }
                    };
                    self.put(root, 0, value)?;
                    if matches!(reason,ExitReason::Throw(info) if info.fault.is_some()) {
                        let cause = self.value(s.env, 2)?;
                        self.put(root, 1, cause)?;
                    }
                    self.active.unwinds.data.push(Unwind {
                        frame: s.id,
                        origin,
                        reason,
                        region: handler.region,
                        value: root,
                        replacement_event: None,
                    });
                    self.clear(s.env, 1)?;
                    self.clear(s.env, 2)?;
                    s.pc = handler.pc;
                    s.phase = ScriptPhase::Body;
                }
            }
        } else {
            match reason {
                ExitReason::Leave(target) => {
                    s.pc = target;
                    s.phase = ScriptPhase::Body;
                }
                _ => s.phase = ScriptPhase::Release { reason },
            }
        }
        Ok(true)
    }
    pub(super) fn commit(
        &mut self,
        ret: ReturnTo,
        value: Value,
        cause: Option<Value>,
        error: Option<FaultSeed>,
    ) -> Result<bool, VmError> {
        if let (ReturnTo::Native { frame }, Some(seed)) = (ret, error)
            && let Some(id) = seed.fault
        {
            let Frame::Native(n) = self.active.frames.data[self.index(frame)?] else {
                return Err(VmError::Contract("callback target"));
            };
            if !self.native_fault_slot_holds(n, id)?
                && n.fault_count == self.limits.max_native_faults
            {
                self.block(
                    BlockedResource::NativeFaultSlots { frame },
                    n.fault_count as u64,
                    n.fault_count as u64 + 1,
                );
                return Ok(false);
            }
        }
        match ret {
            ReturnTo::Generator { .. } => {
                return self.generator_commit_error(ret, value, cause, error);
            }
            ReturnTo::Delivery => {
                self.put(self.active.delivery.unwrap(), 0, value)?;
                if let Some(c) = cause {
                    self.put(self.active.delivery.unwrap(), 1, c)?;
                } else {
                    self.clear(self.active.delivery.unwrap(), 1)?;
                }
                let serial = self.serial()?;
                self.active.terminal = Some(Terminal {
                    id: DeliveryId {
                        task: self.active.task_id.unwrap(),
                        heap: self.heap.id(),
                        epoch: self.heap.epoch(),
                        serial,
                    },
                    fault: error,
                });
            }
            ReturnTo::Register { frame, call, .. }
            | ReturnTo::Default { frame, call, .. }
            | ReturnTo::Class { frame, call }
            | ReturnTo::Sort { frame, call }
            | ReturnTo::ReactiveRead { frame, call } => {
                let i = self.index(frame)?;
                let Frame::Script(mut parent) = self.active.frames.data[i] else {
                    return Err(VmError::Contract("return target"));
                };
                if let Some(info) = error {
                    self.abort_class(parent)?;
                    self.put(parent.env, 1, value)?;
                    if let Some(c) = cause {
                        self.put(parent.env, 2, c)?;
                    } else {
                        self.clear(parent.env, 2)?;
                    }
                    parent.phase = match ret {
                        ReturnTo::Default { .. } => ScriptPhase::Release {
                            reason: ExitReason::Throw(info),
                        },
                        _ => ScriptPhase::Exit {
                            origin: call,
                            reason: ExitReason::Throw(info),
                            skip: None,
                        },
                    };
                } else {
                    match ret {
                        ReturnTo::Register { reg, .. } => self.put(parent.regs, reg.0, value)?,
                        ReturnTo::Class { .. } => self.put(parent.env, 2, value)?,
                        ReturnTo::Sort { .. } => self.put(parent.env, 3, value)?,
                        ReturnTo::ReactiveRead { .. } => {}
                        ReturnTo::Default { parameter, .. } => {
                            self.put(parent.bound, parameter, value)?;
                            parent.phase = ScriptPhase::Defaults { parameter };
                        }
                        _ => unreachable!(),
                    }
                }
                self.active.frames.data[i] = Frame::Script(parent);
            }
            ReturnTo::Native { frame } => {
                let i = self.index(frame)?;
                let Frame::Native(mut n) = self.active.frames.data[i] else {
                    return Err(VmError::Contract("native callback return target"));
                };
                self.put(n.roots, 0, value)?;
                let cause_slot =
                    2 + self.limits.max_continuation_values + self.limits.max_arguments;
                self.put(n.roots, cause_slot, cause.unwrap_or(Value::NULL))?;
                n.callback_fault = error.and_then(|s| s.fault);
                if let Some(id) = n.callback_fault {
                    self.push_native_fault(&mut n, id)?;
                }
                self.clear(n.roots, self.native_pending_slot())?;
                n.phase = NativePhase::Resume(if error.is_some() {
                    NativeInput::CallbackFault
                } else {
                    NativeInput::CallbackReturned
                });
                self.active.frames.data[i] = Frame::Native(n);
            }
            ReturnTo::NativeDefault { frame, parameter } => {
                let i = self.index(frame)?;
                let Frame::Native(mut n) = self.active.frames.data[i] else {
                    return Err(VmError::Contract("native default target"));
                };
                if let Some(seed) = error {
                    self.put(n.roots, 0, value)?;
                    self.put(
                        n.roots,
                        self.native_pending_slot(),
                        cause.unwrap_or(Value::NULL),
                    )?;
                    n.pending_fault = Some(seed);
                    n.phase = NativePhase::Commit(true);
                } else {
                    self.put(n.bound, parameter, value)?;
                    n.phase = NativePhase::Defaults { parameter };
                }
                self.active.frames.data[i] = Frame::Native(n);
            }
        }
        Ok(true)
    }
    pub(super) fn native_step(
        &mut self,
        mut n: NativeFrame,
        m: &mut Meter,
    ) -> Result<Option<RunOutcome>, VmError> {
        let top = self.active.frames.data.len() - 1;
        match n.phase {
            NativePhase::Copy { source, index } => {
                if !m.take(0, self.argument_work(source, index), 0) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                if index < n.argument_count {
                    match self.checked_argument(source, index) {
                        Ok((name, value)) => {
                            self.put(n.args, index * 2, name)?;
                            self.put(n.args, index * 2 + 1, value)?;
                            n.phase = NativePhase::Copy {
                                source,
                                index: index + 1,
                            };
                        }
                        Err(e) => {
                            let kind = language_fault(e)?;
                            self.native_fault(&mut n, kind)?;
                        }
                    }
                } else {
                    // FrameSlots::open initialized the complete physical range
                    // and its bitmap under native_push_cost. Re-publishing each
                    // unused NULL only satisfied the former whole-window borrow.
                    n.phase = if self.program.package().signatures[n.signature.index()]
                        .rest
                        .is_some()
                    {
                        NativePhase::Rest
                    } else {
                        NativePhase::Bind(ArgumentBinding::new(u32::from(n.receiver)))
                    };
                }
            }
            NativePhase::Bind(mut binding) => {
                if binding.index < n.argument_count {
                    let result = self.bind_argument_step(
                        n.signature,
                        n.bound,
                        n.args,
                        &mut binding,
                        m,
                        true,
                    )?;
                    n.phase = NativePhase::Bind(binding);
                    match result {
                        ArgumentStep::Blocked => {
                            self.active.frames.data[top] = Frame::Native(n);
                            return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                        }
                        ArgumentStep::Ready(Err(kind)) => self.native_fault(&mut n, kind)?,
                        _ => {}
                    }
                } else {
                    let sig = &self.program.package().signatures[n.signature.index()];
                    // The no-default path checks the whole initialized window.
                    if !m.take(0, sig.parameters.len() as u64 + 3, 0) {
                        return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                    }
                    if n.has_defaults || sig.rest.is_some() {
                        n.phase = NativePhase::Defaults { parameter: 0 };
                    } else if self.slots.borrow_initialized(n.bound).is_err() {
                        self.native_fault(&mut n, FaultKind::MissingArgument)?;
                    } else {
                        n.phase = NativePhase::Resume(NativeInput::Entry);
                    }
                }
            }
            NativePhase::Rest
            | NativePhase::Defaults { .. }
            | NativePhase::Parameter { .. }
            | NativePhase::DefaultCall { .. }
            | NativePhase::DefaultConstant { .. }
            | NativePhase::Project { .. } => {
                if !self.native_parameter_step(&mut n, m)? {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
            }
            NativePhase::Resume(input) | NativePhase::Issue(input) => {
                if !m.take(0, 1, 0) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                if matches!(n.phase, NativePhase::Resume(_)) && n.bindings.is_some() {
                    n.phase = NativePhase::Project { index: 0, input };
                    self.active.frames.data[top] = Frame::Native(n);
                    return Ok(None);
                }
                let serial = self.serial()?;
                let ticket = self.make_ticket(n, self.active.task_id.unwrap(), serial, input);
                if input == NativeInput::Entry {
                    self.native_entries += 1;
                } else {
                    self.native_resumes += 1;
                }
                n.phase = NativePhase::Issued { serial, input };
                self.active.frames.data[top] = Frame::Native(n);
                return Ok(Some(RunOutcome::HostCall(ticket)));
            }
            NativePhase::Issued { serial, input } => {
                return Ok(Some(RunOutcome::HostCall(self.make_ticket(
                    n,
                    self.active.task_id.unwrap(),
                    serial,
                    input,
                ))));
            }
            NativePhase::Waiting(wait) => return Ok(Some(RunOutcome::Waiting(wait))),
            NativePhase::Continue => {
                if !m.take(0, 1, 0) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                n.phase = NativePhase::Resume(NativeInput::Continue {
                    version: n.continuation_version,
                    stage: n.continuation_stage,
                });
            }
            NativePhase::Callback { count } => {
                let closure = self.value(n.roots, 1)?;
                let function = self.closure_function(closure)?;
                let (work, bytes) = self.script_push_cost(function, count);
                if !m.take(0, work, bytes) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                self.active.frames.data[top] = Frame::Native(n);
                let prepared = self.push_script(
                    function,
                    Some(closure),
                    ArgSource::Native { frame: n.id },
                    count,
                    n.owner,
                    n.context,
                    n.effects,
                    ReturnTo::Native { frame: n.id },
                );
                match prepared {
                    Ok(()) => return Ok(None),
                    Err(e) => {
                        let kind = language_fault(e)?;
                        self.put(n.roots, 0, Value::NULL)?;
                        n.pending_fault = Some(FaultSeed {
                            fault: None,
                            explicit: None,
                            author_value: false,
                            awaited: None,
                            kind,
                            location: n.location,
                        });
                        n.phase = NativePhase::CallbackFaultBuild;
                    }
                }
            }
            NativePhase::CallbackFaultBuild | NativePhase::WaitingFaultBuild => {
                if !m.take(
                    0,
                    2 * (n.fault_count as u64 + n.continuation_len as u64) + 8,
                    0,
                ) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                let id = n
                    .pending_fault
                    .and_then(|f| f.fault)
                    .ok_or(VmError::Contract("unbuilt callback fault"))?;
                if !self.native_fault_slot_holds(n, id)?
                    && n.fault_count == self.limits.max_native_faults
                {
                    self.block(
                        BlockedResource::NativeFaultSlots { frame: n.id },
                        n.fault_count as u64,
                        n.fault_count as u64 + 1,
                    );
                    return Ok(None);
                }
                self.push_native_fault(&mut n, id)?;
                n.callback_fault = Some(id);
                n.pending_fault = None;
                self.clear(n.roots, self.native_pending_slot())?;
                n.phase =
                    NativePhase::Resume(if matches!(n.phase, NativePhase::WaitingFaultBuild) {
                        NativeInput::WaitingFault
                    } else {
                        NativeInput::CallbackFault
                    });
            }
            NativePhase::Commit(error) => {
                let extra = if error {
                    0
                } else {
                    self.type_checks
                        .extra(self.program.package().signatures[n.signature.index()].return_type)
                };
                if !m.take(0, 1 + extra, 0) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                let value = self.value(n.roots, 0)?;
                if !error
                    && self
                        .check_type(
                            self.program.package().signatures[n.signature.index()].return_type,
                            value,
                        )
                        .is_err()
                {
                    self.put(n.roots, 0, Value::NULL)?;
                    self.clear(n.roots, self.native_pending_slot())?;
                    n.pending_fault = Some(FaultSeed {
                        fault: None,
                        explicit: None,
                        author_value: false,
                        awaited: None,
                        kind: FaultKind::Type,
                        location: n.location,
                    });
                    n.phase = NativePhase::Release(true);
                } else {
                    n.phase = NativePhase::Release(error);
                }
            }
            NativePhase::Release(error) => {
                if !m.take(
                    0,
                    self.release_cost(n.root_count()) + self.commit_work(n.ret),
                    0,
                ) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                let value = self.value(n.roots, 0)?;
                let info = if error {
                    Some(n.pending_fault.unwrap_or(FaultSeed {
                        fault: None,
                        explicit: None,
                        author_value: false,
                        awaited: None,
                        kind: FaultKind::Thrown,
                        location: n.location,
                    }))
                } else {
                    None
                };
                let cause = info.and_then(|seed| seed.fault).map(FaultId::as_value);
                if !self.commit(n.ret, value, cause, info)? {
                    self.active.frames.data[top] = Frame::Native(n);
                    return Ok(None);
                }
                self.release_frame(Frame::Native(n))?;
                self.active.frames.data.remove(top);
                return Ok(None);
            }
        }
        self.active.frames.data[top] = Frame::Native(n);
        Ok(None)
    }
}
