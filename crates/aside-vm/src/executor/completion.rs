//! Equality, stack exit and native continuation execution.
use super::*;

impl Vm {
    pub(super) fn commit_work(&self, ret: ReturnTo) -> u64 {
        match ret {
            ReturnTo::Native { frame } => {
                match self.index(frame).ok().map(|i| self.active.frames.data[i]) {
                    Some(Frame::Native(n)) => {
                        2 * (n.fault_count as u64 + n.continuation_len as u64) + 8
                    }
                    _ => 8,
                }
            }
            _ => 2,
        }
    }
    #[cfg_attr(not(feature = "profiling"), inline(always))]
    pub(super) fn release_frame(&mut self, f: Frame) -> Result<(), VmError> {
        part_cost!(self.step_costs, RootRelease, self.release_frame_inner(f))
    }
    fn release_frame_inner(&mut self, f: Frame) -> Result<(), VmError> {
        match f {
            Frame::Script(s) => {
                if s.generator {
                    self.generator_abandon(s)?;
                }
                if s.derived_bridge.is_some() {
                    self.heap.end_derived(self.value(s.env, 4)?)?;
                }
                if s.derived_bridge.is_none() || matches!(s.phase, ScriptPhase::Class(_)) {
                    self.abort_class(&s)?;
                }
                for r in [s.regs, s.env, s.args, s.bound] {
                    self.slots.release(&mut self.heap, r)?;
                }
            }
            Frame::Native(n) => {
                if let Some(output) = n.derived_output {
                    self.heap.end_derived(output)?;
                }
                if self
                    .active
                    .model_copy
                    .as_ref()
                    .is_some_and(|copy| copy.frame == n.id)
                {
                    self.active.model_copy = None;
                }
                if let Some(r) = n.bindings {
                    self.slots.release(&mut self.heap, r)?;
                }
                for r in [n.args, n.bound, n.roots] {
                    self.slots.release(&mut self.heap, r)?;
                }
            }
        }
        Ok(())
    }

    #[cfg_attr(not(feature = "profiling"), inline(always))]
    pub(super) fn equality_phase_step(
        &mut self,
        s: &mut ScriptFrame,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        #[cfg(feature = "profiling")]
        let span = self.step_costs.start_part(
            if s.table_work
                .as_ref()
                .is_some_and(|work| work.named_read(s.member_symbol))
            {
                step_costs::Part::NamedEq
            } else {
                step_costs::Part::StructuralEq
            },
        );
        let result = self.equality_phase_inner(s, meter);
        #[cfg(feature = "profiling")]
        self.step_costs.finish_part(span, None);
        result
    }
    fn equality_phase_inner(
        &mut self,
        s: &mut ScriptFrame,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        if !meter.take(0, 1, 0) {
            return Ok(false);
        }
        #[cfg(feature = "execution-counts")]
        if s.table_work
            .as_ref()
            .is_some_and(|work| work.named_read(s.member_symbol))
        {
            execution_count!(self, NamedTableEqualitySteps, 1);
        }
        match s.phase {
            ScriptPhase::Equality { dest, negated } => {
                self.equality_step(s, dest, negated, meter)?
            }
            ScriptPhase::EqualityCleanup {
                dest,
                negated,
                result,
            } => {
                if !self.active.equality_cursors.data.is_empty() {
                    let depth = self.active.equality_cursors.data.len() as u32 - 1;
                    self.clear(self.active.equality_roots.unwrap(), depth * 2)?;
                    self.clear(self.active.equality_roots.unwrap(), depth * 2 + 1)?;
                    self.active.equality_cursors.data.pop();
                } else {
                    if s.table_work.is_some() {
                        self.table_comparison_done(s, result)?;
                    } else {
                        match result {
                            Ok(equal) => {
                                self.put(s.regs, dest.0, Value::bool(equal != negated))?;
                                s.pc = s.instruction_end;
                                s.phase = ScriptPhase::Body;
                            }
                            Err(kind) => {
                                self.put(s.env, 1, Value::NULL)?;
                                self.clear(s.env, 2)?;
                                s.phase = ScriptPhase::Exit {
                                    origin: s.pc,
                                    reason: ExitReason::Throw(FaultSeed {
                                        fault: None,
                                        explicit: None,
                                        author_value: false,
                                        awaited: None,

                                        kind,
                                        location: self.loc(s.code, s.pc),
                                    }),
                                    skip: None,
                                };
                            }
                        }
                    }
                }
            }
            _ => unreachable!("equality phase dispatch"),
        }
        Ok(true)
    }

    pub(super) fn equality_step(
        &mut self,
        s: &mut ScriptFrame,
        dest: Reg,
        negated: bool,
        meter: &mut Meter,
    ) -> Result<(), VmError> {
        if self.table_changed(s)? {
            s.phase = ScriptPhase::EqualityCleanup {
                dest,
                negated,
                result: Err(FaultKind::ConcurrentModification),
            };
            return Ok(());
        }
        let meter = Some(meter);
        #[cfg(test)]
        let meter = meter.filter(|_| !self.work_probe.single_equality_step);
        if let Some(result) = self.equality_compare(meter)? {
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
        self.equality_compare(None)
    }

    fn equality_compare(
        &mut self,
        meter: Option<&mut Meter>,
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
                        } else if let Some(meter) = meter {
                            self.equality_byte_batch(depth, left, right, offset, meter)?
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

    fn equality_byte_batch(
        &mut self,
        depth: u32,
        left: Value,
        right: Value,
        offset: u32,
        meter: &mut Meter,
    ) -> Result<Option<bool>, VmError> {
        // The caller admitted the first logical byte. Fuse only the following
        // short steps that fit both work and the existing clock batch. Borrowed
        // immutable segments stay within this allocation/callback-free step.
        let mut extra = 31.min(meter.budget.work - meter.usage.work);
        if meter.budget.deadline_ns.is_some() {
            extra = extra.min(u64::from(meter.clock_steps));
        }
        let a = self.heap.byte_segment(left, offset)?;
        let b = self.heap.byte_segment(right, offset)?;
        let count = (extra as usize + 1).min(a.len()).min(b.len());
        let different = a[..count].iter().zip(&b[..count]).position(|(a, b)| a != b);
        let compared = different.map_or(count, |index| index + 1);
        for _ in 1..compared {
            assert!(meter.next_short_step());
            assert!(meter.take(0, 1, 0));
        }
        self.active.equality_cursors.data[depth as usize] += compared as u32;
        // Even a complete equal chunk retains the original separate end step,
        // preserving its work charge, cursor and root-release boundaries.
        Ok(different.map(|_| false))
    }
    pub(super) fn select_exit_handler(
        &self,
        s: &ScriptFrame,
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
        s: &ScriptFrame,
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
                        location: self.loc(s.code, origin),
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
        if !self.grow_commit_roots(ret, error)? {
            return Ok(false);
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
            | ReturnTo::BuiltinCallback { frame, call }
            | ReturnTo::ReactiveRead { frame, call } => {
                let i = self.index(frame)?;
                let Frame::Script(mut parent) = self.active.frames.data[i] else {
                    return Err(VmError::Contract("return target"));
                };
                if let Some(info) = error {
                    self.abort_class(&parent)?;
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
                        ReturnTo::BuiltinCallback { .. } => self.put(parent.env, 3, value)?,
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
                if let Some(cause) = cause {
                    self.put(n.roots, cause_slot, cause)?;
                }
                n.callback_fault = error.and_then(|s| s.fault);
                if let Some(id) = n.callback_fault {
                    self.push_native_fault(&mut n, id)?;
                }
                self.clear_native_pending(n.roots)?;
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
    #[cfg_attr(not(feature = "profiling"), inline(always))]
    pub(super) fn native_step(
        &mut self,
        n: NativeFrame,
        m: &mut Meter,
    ) -> Result<Option<RunOutcome>, VmError> {
        #[cfg(feature = "profiling")]
        let span = self.step_costs.start_part(match n.phase {
            NativePhase::Resume(_) | NativePhase::Issue(_) => step_costs::Part::NativeResume,
            NativePhase::Commit(_) => step_costs::Part::NativeCommit,
            NativePhase::Release(_) => step_costs::Part::NativeRelease,
            _ => step_costs::Part::NativePrepare,
        });
        let result = self.native_step_inner(n, m);
        #[cfg(feature = "profiling")]
        self.step_costs.finish_part(span, None);
        result
    }
    fn native_step_inner(
        &mut self,
        mut n: NativeFrame,
        m: &mut Meter,
    ) -> Result<Option<RunOutcome>, VmError> {
        #[cfg(feature = "execution-counts")]
        self.execution_counts.native_step(&n.phase);
        let top = self.active.frames.data.len() - 1;
        match n.phase {
            NativePhase::PlannedCheck { source } => {
                let direct = matches!(
                    n.argument_plan,
                    Some(NativeArgumentPlan::PositionalReceiver)
                ) && n.argument_count <= 1;
                let direct_work = if direct {
                    1 + u64::from(n.argument_count)
                        * (2 + self.type_checks.argument_extra(n.signature))
                } else {
                    0
                };
                if !m.take(0, 1 + u64::from(n.argument_count) + direct_work, 0) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                let ArgSource::Caller { frame, descriptor } = source else {
                    unreachable!()
                };
                let Frame::Script(caller) = self.active.frames.data[self.index(frame)?] else {
                    unreachable!()
                };
                let Descriptor::Call(call) = &self.program.package().functions
                    [caller.code.function.index()]
                .descriptors[descriptor.index()] else {
                    unreachable!()
                };
                let Arguments::Window(window) = call.arguments else {
                    unreachable!()
                };
                match self
                    .slots
                    .borrow_initialized_window(caller.regs, window.start.0, window.len)
                {
                    Ok(_) if direct => {
                        let result = if n.argument_count == 0 {
                            Ok(())
                        } else {
                            let ty = self.program.package().signatures[n.signature.index()]
                                .parameters[1]
                                .ty;
                            self.argument(source, 0)
                                .map_err(vm_fault)
                                .and_then(|(_, value)| self.check_type(ty, value).map(|()| value))
                                .and_then(|value| self.put(n.bound, 1, value).map_err(vm_fault))
                        };
                        match result {
                            Ok(()) => {
                                n.phase = if n.has_defaults {
                                    NativePhase::Defaults { parameter: 0 }
                                } else {
                                    NativePhase::Resume(NativeInput::Entry)
                                }
                            }
                            Err(kind) => self.native_fault(&mut n, kind)?,
                        }
                    }
                    Ok(_) => n.phase = NativePhase::PlannedCopy { source, index: 0 },
                    Err(error) => self.native_fault(&mut n, heap_fault(error))?,
                }
            }
            NativePhase::PlannedCopy { source, index } => {
                if index < n.argument_count {
                    if !m.take(0, 2 + self.type_checks.argument_extra(n.signature), 0) {
                        return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                    }
                    let position = match n.argument_plan.unwrap() {
                        NativeArgumentPlan::Fixed(plan) => {
                            self.program.native_call_slots(plan).unwrap()[index as usize]
                        }
                        NativeArgumentPlan::PositionalReceiver => index + 1,
                    };
                    let ty = self.program.package().signatures[n.signature.index()].parameters
                        [position as usize]
                        .ty;
                    let result = self
                        .argument(source, index)
                        .map_err(vm_fault)
                        .and_then(|(_, value)| self.check_type(ty, value).map(|()| value));
                    match result {
                        Ok(value) => {
                            self.put(n.bound, position, value)?;
                            n.phase = NativePhase::PlannedCopy {
                                source,
                                index: index + 1,
                            };
                        }
                        Err(kind) => self.native_fault(&mut n, kind)?,
                    }
                } else {
                    if !m.take(0, 1, 0) {
                        return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                    }
                    n.phase = if n.has_defaults {
                        NativePhase::Defaults { parameter: 0 }
                    } else {
                        NativePhase::Resume(NativeInput::Entry)
                    };
                }
            }
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
            NativePhase::Defaults { .. } | NativePhase::Parameter { .. } => loop {
                let batched = if n.has_defaults
                    && n.bindings.is_none()
                    && let NativePhase::Defaults { parameter } = n.phase
                {
                    self.native_scalar_defaults(&mut n, m, parameter)?
                } else {
                    0
                };
                if batched == 0 && !self.native_parameter_step(&mut n, m)? {
                    self.active.frames.data[top] = Frame::Native(n);
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                #[cfg(feature = "execution-counts")]
                if batched > 1 {
                    self.execution_counts
                        .native_steps(&n.phase, u64::from(batched - 1));
                }
                // Leave the batch before a clock check or a different phase
                // that can suspend, run script code, or deliver a fault.
                if !matches!(
                    n.phase,
                    NativePhase::Defaults { .. } | NativePhase::Parameter { .. }
                ) || !m.next_short_step()
                {
                    break;
                }
                #[cfg(feature = "execution-counts")]
                self.execution_counts.native_step(&n.phase);
            },
            NativePhase::Rest
            | NativePhase::DefaultCall { .. }
            | NativePhase::DefaultConstant { .. }
            | NativePhase::Project { .. } => {
                if !self.native_parameter_step(&mut n, m)? {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
            }
            NativePhase::Resume(input) | NativePhase::Issue(input) => {
                let projecting = matches!(n.phase, NativePhase::Resume(_)) && n.bindings.is_some();
                let derived_commit = input == NativeInput::CallbackReturned
                    && n.inline_argument
                    && n.derived_output.is_some()
                    && self.native_parameters.data[n.native.index()].standard_derived_commit;
                let leaf = if input == NativeInput::Entry && !projecting {
                    self.native_parameters.data[n.native.index()].read_only_leaf
                } else {
                    None
                };
                let work = if derived_commit {
                    2
                } else {
                    leaf.map_or(1, |leaf| u64::from(leaf.work))
                };
                if !m.take(0, work, 0) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                if projecting {
                    n.phase = NativePhase::Project { index: 0, input };
                    self.active.frames.data[top] = Frame::Native(n);
                    return Ok(None);
                }
                if derived_commit {
                    self.commit_standard_derived(&mut n)?;
                    self.active.frames.data[top] = Frame::Native(n);
                    m.clock_steps = 0;
                    return Ok(None);
                }
                if let Some(leaf) = leaf {
                    let arguments = if n.inline_argument {
                        self.slots.borrow_initialized_window(n.roots, 1, 1)?
                    } else {
                        self.slots.borrow_initialized(n.bound)?
                    };
                    if let Some(value) = part_cost!(
                        self.step_costs,
                        NativeLeafInvoke,
                        (leaf.invoke)(&self.heap, arguments)
                    ) {
                        self.put(n.roots, 0, value)?;
                        n.pending_fault = None;
                        n.phase = NativePhase::Commit(false);
                        self.active.frames.data[top] = Frame::Native(n);
                        // Preserve the supplied-clock boundary after native work.
                        m.clock_steps = 0;
                        return Ok(None);
                    }
                }
                let serial = self.serial()?;
                let ticket = self.make_ticket(&n, self.active.task_id.unwrap(), serial, input);
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
                    &n,
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
                let id = n
                    .pending_fault
                    .and_then(|f| f.fault)
                    .ok_or(VmError::Contract("unbuilt callback fault"))?;
                let retained = self.native_fault_slot_holds(n, id)?;
                let required = self.native_fault_base()
                    + (n.fault_count + u32::from(!retained)).min(self.limits.max_native_faults);
                let Some(growth) = self.native_root_admission(n.roots, required)? else {
                    return Ok(None);
                };
                if !m.take(
                    0,
                    2 * (n.fault_count as u64 + n.continuation_len as u64) + 8 + growth.work,
                    growth.allocation_bytes,
                ) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                if !retained && n.fault_count == self.limits.max_native_faults {
                    self.block(
                        BlockedResource::NativeFaultSlots { frame: n.id },
                        n.fault_count as u64,
                        n.fault_count as u64 + 1,
                    );
                    return Ok(None);
                }
                if let Err(error) = self.slots.grow(&mut self.heap, n.roots, required) {
                    self.block_heap(error)?;
                    return Ok(None);
                }
                self.push_native_fault(&mut n, id)?;
                n.callback_fault = Some(id);
                n.pending_fault = None;
                self.clear_native_pending(n.roots)?;
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
                    self.clear_native_pending(n.roots)?;
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
                let info = error.then(|| {
                    n.pending_fault.unwrap_or(FaultSeed {
                        fault: None,
                        explicit: None,
                        author_value: false,
                        awaited: None,
                        kind: FaultKind::Thrown,
                        location: n.location,
                    })
                });
                let Some(growth) = self.commit_root_admission(n.ret, info)? else {
                    return Ok(None);
                };
                if !m.take(
                    0,
                    self.release_cost(n.root_count()) + self.commit_work(n.ret) + growth.0,
                    growth.1,
                ) {
                    return Ok(Some(RunOutcome::BudgetYield(m.usage)));
                }
                let value = self.value(n.roots, 0)?;
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
