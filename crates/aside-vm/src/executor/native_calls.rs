//! Native callable binding, host tickets and continuations.
use super::*;

pub(super) struct CalleeRead<'a> {
    pub(super) function: FunctionId,
    pub(super) target: crate::heap::ObjectRead<'a>,
    pub(super) script_receiver: Option<Value>,
}

impl Vm {
    /// Register a trusted callable host object. Its first native parameter is
    /// the object itself; argument binding and effects share ordinary calls.
    pub fn bind_callable(&mut self, object: Value, function: FunctionId) -> Result<(), VmError> {
        self.validate_native_receiver(object, function)?;
        self.heap.set_host_call(object, function)?;
        Ok(())
    }

    pub(super) fn callable_receiver(&self, value: Value) -> Result<Option<Value>, VmError> {
        self.callable_receiver_from(value, self.heap.read_object(value)?)
    }
    pub(super) fn callable_receiver_from(
        &self,
        value: Value,
        read: crate::heap::ObjectRead<'_>,
    ) -> Result<Option<Value>, VmError> {
        match read.kind() {
            ObjectKind::BoundNative => Ok(Some(read.field(3)?)),
            ObjectKind::Host => {
                read.host_call()?;
                Ok(Some(value))
            }
            _ => Ok(None),
        }
    }

    pub(super) fn shared_host_method(
        &self,
        object: Value,
        key: Value,
        name: Option<SymbolId>,
    ) -> Result<Option<FunctionId>, HeapError> {
        if let Some(name) = name {
            return self.heap.shared_host_method(object, name);
        }
        if !self.heap.accepts_builtin(BuiltinType::String, key)? {
            return Ok(None);
        }
        let Some(name) = self
            .type_checks
            .symbol(self.program.package(), &self.heap, key)?
        else {
            return Ok(None);
        };
        self.heap.shared_host_method(object, name)
    }

    pub(super) fn host_member_symbol(&self, frame: &ScriptFrame) -> Option<SymbolId> {
        frame.member_symbol
    }

    pub(super) fn host_binding_cost(&self, function: FunctionId) -> (u64, u64) {
        let p = self.program.package();
        let signature = p.functions[function.index()].signature;
        let receiver = p.signatures[signature.index()].parameters[0].ty;
        (
            4 + self.type_checks.extra(receiver),
            Values::allocation_bytes(4),
        )
    }

    pub(super) fn host_binding_value(
        &mut self,
        host: Value,
        function: FunctionId,
        effects: Effects,
    ) -> Result<Value, FaultKind> {
        if effects.0 & Effects::ALLOCATE == 0 {
            Err(FaultKind::Effect)
        } else {
            self.bind_native(host, function).map_err(vm_fault)
        }
    }

    /// Bind the receiver of an admitted native callable. The immutable value
    /// traces its receiver and shares ordinary native argument preparation.
    /// Live bindings may be reused through the bounded weak cache shared with
    /// script methods. Collected or
    /// GC-unconfirmed bindings are recreated instead of revived.
    #[cfg_attr(not(feature = "profiling"), inline(always))]
    pub fn bind_native(&mut self, receiver: Value, function: FunctionId) -> Result<Value, VmError> {
        #[cfg(feature = "profiling")]
        let span = self
            .step_costs
            .start_part(step_costs::Part::MethodNativeError);
        #[cfg(feature = "profiling")]
        let hits = self.native_binding_hits;
        let result = self.bind_native_inner(receiver, function);
        #[cfg(feature = "profiling")]
        self.step_costs.finish_part(
            span,
            Some(if result.is_err() {
                step_costs::Part::MethodNativeError
            } else if self.native_binding_hits != hits {
                step_costs::Part::MethodNativeHit
            } else {
                step_costs::Part::MethodNativeMiss
            }),
        );
        result
    }
    fn bind_native_inner(
        &mut self,
        receiver: Value,
        function: FunctionId,
    ) -> Result<Value, VmError> {
        self.validate_native_receiver(receiver, function)?;
        let receiver_key = receiver.object_parts().map(|(_, key)| key);
        if let Some(key) = receiver_key
            && let Some(callable) = self
                .method_bindings
                .get(method_bindings::MethodKey::native(key, function))
        {
            let value = Value::object(self.heap.id(), callable);
            if self.heap.read_object(value).is_ok() {
                self.native_binding_hits += 1;
                return Ok(value);
            }
        }
        self.native_binding_misses += 1;
        let p = self.program.package();
        let f = &p.functions[function.index()];

        let value = self.heap.alloc_record(
            ObjectKind::BoundNative,
            &[
                Value::int(function.0 as i64),
                Value::int(p.content_version as i64),
                Value::int(f.module.0 as i64),
                receiver,
            ],
        )?;
        if let Some(key) = receiver_key {
            self.method_bindings.remember(
                method_bindings::MethodKey::native(key, function),
                value.object_parts().unwrap().1,
            );
        }
        Ok(value)
    }

    /// Bind a registered shared method of a live host receiver. Own fields are
    /// excluded: this host API selects the trusted nominal type's method.
    /// A missing method returns `None`; invalid or retired receivers fail through
    /// ordinary heap validation. Name lookup reuses the loaded symbol index.
    pub fn bind_host_method(
        &mut self,
        receiver: Value,
        name: &str,
    ) -> Result<Option<Value>, VmError> {
        self.heap.host_fields(receiver)?;
        let Some(symbol) = self.type_checks.symbol_str(self.program.package(), name) else {
            return Ok(None);
        };
        self.heap
            .shared_host_method(receiver, symbol)?
            .map(|function| self.bind_native(receiver, function))
            .transpose()
    }

    pub(super) fn validate_native_receiver(
        &self,
        receiver: Value,
        function: FunctionId,
    ) -> Result<(), VmError> {
        let p = self.program.package();
        let f = p
            .functions
            .get(function.index())
            .ok_or(VmError::Contract("unknown native callable"))?;
        if !matches!(f.kind, FunctionKind::Native(_)) {
            return Err(VmError::Contract(
                "receiver binding requires a native callable",
            ));
        }
        let parameter = p.signatures[f.signature.index()]
            .parameters
            .first()
            .filter(|p| p.default.is_none())
            .ok_or(VmError::Contract(
                "native receiver requires a first parameter",
            ))?;
        if !self
            .type_checks
            .accepts(p, &self.heap, parameter.ty, receiver)?
        {
            return Err(HeapError::TypeMismatch.into());
        }
        Ok(())
    }

    /// Resolve a function value that carries no receiver or captured environment.
    /// The identity belongs to this verified program; no heap value crosses into
    /// a replacement VM. Hosts validate the entry's signature and context.
    pub fn unbound_function(&self, value: Value) -> Result<FunctionId, VmError> {
        if self.heap.read_object(value)?.kind() != ObjectKind::Closure {
            return Err(VmError::Contract("entry must be an unbound function"));
        }
        let function = self.closure_function(value)?;
        if !self.program.package().functions[function.index()]
            .captures
            .is_empty()
        {
            return Err(VmError::Contract("entry must not capture local state"));
        }
        Ok(function)
    }

    pub(super) fn closure_function(&self, closure: Value) -> Result<FunctionId, VmError> {
        Ok(self.read_callee(closure)?.function)
    }
    pub(super) fn read_callee(&self, closure: Value) -> Result<CalleeRead<'_>, VmError> {
        let target = self.heap.read_object(closure)?;
        self.read_callee_from(closure, target)
    }
    pub(super) fn read_callee_from<'a>(
        &'a self,
        closure: Value,
        target: crate::heap::ObjectRead<'a>,
    ) -> Result<CalleeRead<'a>, VmError> {
        let (closure, script_receiver) = self.script_closure_from(closure, target)?;
        let read = if target.kind() == ObjectKind::BoundScript {
            self.heap.read_object(closure)?
        } else {
            target
        };
        let kind = read.kind();
        if kind == ObjectKind::Host {
            return Ok(CalleeRead {
                function: read.host_call()?,
                target,
                script_receiver,
            });
        }
        let bound = kind == ObjectKind::BoundNative;
        if kind != ObjectKind::Closure && !bound {
            return Err(VmError::Contract("callee is not a closure"));
        }
        let fi = read
            .field(0)?
            .as_int()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or(VmError::Contract("invalid closure code"))?;
        let version = read
            .field(1)?
            .as_int()
            .ok_or(VmError::Contract("invalid closure version"))? as u64;
        let f = self
            .program
            .package()
            .functions
            .get(fi as usize)
            .ok_or(VmError::Contract("foreign closure code"))?;
        if version != self.program.package().content_version
            || read.field(2)?.as_int() != Some(f.module.0 as i64)
            || read.len() != f.captures.len() as u32 + 3 + u32::from(bound)
            || (bound && !matches!(f.kind, FunctionKind::Native(_)))
        {
            return Err(VmError::Contract("foreign closure environment/version"));
        }
        Ok(CalleeRead {
            function: FunctionId(fi),
            target,
            script_receiver,
        })
    }
    pub(super) fn argument_contract(&self, source: ArgSource, index: u32) -> Option<TypeId> {
        let ArgSource::CheckedCaller { contract, .. } = source else {
            return None;
        };
        let TypeDef::Function { parameters, .. } = &self.program.package().types[contract.index()]
        else {
            unreachable!()
        };
        parameters.get(index as usize).copied()
    }

    pub(super) fn argument_work(&self, source: ArgSource, index: u32) -> u64 {
        1 + self
            .argument_contract(source, index)
            .map_or(0, |ty| 1 + self.type_checks.extra(ty))
    }

    pub(super) fn checked_argument(
        &self,
        source: ArgSource,
        index: u32,
    ) -> Result<(Value, Value), VmError> {
        let (name, value) = self.argument(source, index)?;
        if let Some(ty) = self.argument_contract(source, index)
            && (name.as_int() != Some(-1)
                || !self
                    .type_checks
                    .accepts(self.program.package(), &self.heap, ty, value)?)
        {
            return Err(HeapError::TypeMismatch.into());
        }
        Ok((name, value))
    }

    pub(super) fn argument(
        &self,
        source: ArgSource,
        index: u32,
    ) -> Result<(Value, Value), VmError> {
        match source {
            ArgSource::None => Err(VmError::Contract("external arguments already copied")),
            ArgSource::Await { frame, value } => {
                let Frame::Script(s) = self.active.frames.data[self.index(frame)?] else {
                    return Err(VmError::Contract("await argument source"));
                };
                Ok((Value::int(-1), self.slots.read_vm(at(s.regs, value.0))?))
            }
            ArgSource::Caller { frame, descriptor }
            | ArgSource::CheckedCaller {
                frame, descriptor, ..
            } => {
                let Frame::Script(s) = self.active.frames.data[self.index(frame)?] else {
                    return Err(VmError::Contract("argument source"));
                };
                let Descriptor::Call(desc) = &self.program.package().functions
                    [s.code.function.index()]
                .descriptors[descriptor.index()] else {
                    unreachable!()
                };
                match desc.arguments {
                    Arguments::Window(w) => {
                        // B01 verifies that names occupy the ordered window
                        // tail; copying each argument needs no name-list scan.
                        let positional = w.len as usize - desc.names.len();
                        let name = (index as usize)
                            .checked_sub(positional)
                            .map_or(-1, |i| i64::from(desc.names[i].name.0));
                        Ok((
                            Value::int(name),
                            self.slots.read_vm(at(s.regs, w.start.0 + index))?,
                        ))
                    }
                    Arguments::Packed(r) => {
                        let packed = self.slots.read_vm(at(s.regs, r.0))?;
                        Ok((
                            self.heap.get_field(packed, index * 2)?,
                            self.heap.get_field(packed, index * 2 + 1)?,
                        ))
                    }
                }
            }
            ArgSource::Native { frame, .. } => {
                let Frame::Native(n) = self.active.frames.data[self.index(frame)?] else {
                    unreachable!()
                };
                Ok((
                    Value::int(-1),
                    self.value(n.roots, 2 + self.limits.max_continuation_values + index)?,
                ))
            }
            ArgSource::Sort { frame } => self.sort_argument(frame, index),
            ArgSource::Prefix { frame, .. } => {
                let range = match self.active.frames.data[self.index(frame)?] {
                    Frame::Script(s) => s.bound,
                    Frame::Native(n) => n.bindings.unwrap_or(n.bound),
                };
                Ok((Value::int(-1), self.value(range, index)?))
            }
        }
    }
    pub(super) fn push_native(
        &mut self,
        native: NativeId,
        source: ArgSource,
        count: u32,
        site: NativeSite,
    ) -> Result<(), VmError> {
        self.push_native_bound(native, source, count, site, None)
    }
    #[cfg_attr(not(feature = "profiling"), inline(always))]
    pub(super) fn push_native_bound(
        &mut self,
        native: NativeId,
        source: ArgSource,
        count: u32,
        site: NativeSite,
        receiver: Option<Value>,
    ) -> Result<(), VmError> {
        part_cost!(
            self.step_costs,
            CallPushNative,
            self.push_native_bound_inner(native, source, count, site, receiver)
        )
    }
    fn push_native_bound_inner(
        &mut self,
        native: NativeId,
        source: ArgSource,
        count: u32,
        site: NativeSite,
        receiver: Option<Value>,
    ) -> Result<(), VmError> {
        let signature = self.program.package().natives[native.index()].signature;
        let declared = self.program.package().signatures[signature.index()].effects;
        let pure = declared.0 & (Effects::HOST | Effects::WRITE | Effects::WAIT) == 0;
        // Calling a pure native is not itself an external side effect. Derived
        // callbacks retain their allocation mask and cannot admit an impure native.
        if (site.context.derived && !pure)
            || (site.effects.0 & Effects::HOST == 0 && (!pure || declared.0 & !site.effects.0 != 0))
        {
            return Err(VmError::Effect);
        }
        if self.active.frames.data.len() == self.limits.max_frames as usize
            || count > self.limits.max_arguments
        {
            return Err(VmError::Limit);
        }
        let sig = &self.program.package().signatures[signature.index()];
        let n = sig.parameters.len() as u32 + u32::from(sig.rest.is_some());
        let NativeParameters {
            has_defaults,
            needs_cells,
            ..
        } = self.native_parameters.data[native.index()];
        let argument_plan = if receiver.is_none() {
            if let ArgSource::Caller { frame, descriptor } = source {
                let Frame::Script(caller) = self.active.frames.data[self.index(frame)?] else {
                    unreachable!()
                };
                self.program
                    .native_call_plan(caller.code.function, descriptor)
                    .map(NativeArgumentPlan::Fixed)
            } else {
                None
            }
        } else if !needs_cells
            && sig.rest.is_none()
            && sig.parameters.len() == count as usize + 1
            && self.positional_native_source(source, count)
        {
            Some(NativeArgumentPlan::PositionalReceiver)
        } else {
            None
        };
        let argument_slots = if argument_plan.is_some() {
            0
        } else {
            count * 2
        };
        let values = 3 + u64::from(argument_slots) + u64::from(n) * (1 + u64::from(needs_cells));
        self.slots
            .admit_windows(&mut self.heap, 3 + usize::from(needs_cells), values)?;
        let id = FrameId(self.serial()?);
        let args = self.open(site.owner, RootKind::Arguments, argument_slots)?;
        let bound = match self.open(site.owner, RootKind::Arguments, n) {
            Ok(r) => r,
            Err(e) => {
                self.slots.release(&mut self.heap, args)?;
                return Err(e);
            }
        };
        if let Some(value) = receiver
            && let Err(error) = self.put(bound, 0, value)
        {
            self.slots.release(&mut self.heap, bound)?;
            self.slots.release(&mut self.heap, args)?;
            return Err(error);
        }
        let bindings = if needs_cells {
            match self.open(site.owner, RootKind::Arguments, n) {
                Ok(r) => Some(r),
                Err(e) => {
                    self.slots.release(&mut self.heap, bound)?;
                    self.slots.release(&mut self.heap, args)?;
                    return Err(e);
                }
            }
        } else {
            None
        };
        let roots = match self.open(site.owner, RootKind::NativeContinuation, 3) {
            Ok(r) => r,
            Err(e) => {
                if let Some(r) = bindings {
                    self.slots.release(&mut self.heap, r)?;
                }
                self.slots.release(&mut self.heap, bound)?;
                self.slots.release(&mut self.heap, args)?;
                return Err(e);
            }
        };
        self.active.frames.data.push(Frame::Native(NativeFrame {
            inline_argument: false,
            argument_plan,
            id,
            await_activation: matches!(source, ArgSource::Await { .. }).then_some(id.0),
            native,
            signature,
            args,
            bound,
            bindings,
            module: site.module,
            has_defaults,
            roots,
            argument_count: count,
            receiver: receiver.is_some(),
            continuation_len: 0,
            continuation_active: false,
            continuation_version: 0,
            continuation_stage: 0,
            effects: Effects(
                site.effects.0
                    & self.program.package().signatures[signature.index()]
                        .effects
                        .0,
            ),
            context: site.context,
            owner: site.owner,
            location: site.location,
            ret: site.ret,
            callback_fault: None,
            derived_output: None,
            fault_count: 0,
            pending_fault: None,
            phase: if argument_plan.is_some() {
                NativePhase::PlannedCheck { source }
            } else {
                NativePhase::Copy { source, index: 0 }
            },
        }));
        execution_count!(self, NativeFrames, 1);
        Ok(())
    }
    pub(super) fn positional_native_source(&self, source: ArgSource, count: u32) -> bool {
        #[cfg(test)]
        if !self.program.positional_native_plans() {
            return false;
        }
        let ArgSource::Caller { frame, descriptor } = source else {
            return false;
        };
        let Ok(index) = self.index(frame) else {
            return false;
        };
        let Frame::Script(caller) = self.active.frames.data[index] else {
            return false;
        };
        let Descriptor::Call(call) = &self.program.package().functions
            [caller.code.function.index()]
        .descriptors[descriptor.index()] else {
            return false;
        };
        call.names.is_empty()
            && matches!(call.arguments, Arguments::Window(window) if window.len == count)
    }
    pub fn host_arguments(&self, ticket: HostTicket) -> Result<&[Value], VmError> {
        let n = self.ticket(ticket)?;
        Ok(if n.inline_argument {
            self.slots.borrow_initialized_window(n.roots, 1, 1)?
        } else {
            self.slots.borrow_initialized(n.bound)?
        })
    }
    /// Borrow only supplied arguments; omission remains distinct from explicit null.
    pub fn host_provided_arguments(
        &self,
        ticket: HostTicket,
    ) -> Result<ProvidedArguments<'_>, VmError> {
        let n = self.ticket(ticket)?;
        Ok(ProvidedArguments {
            values: if n.inline_argument {
                self.slots.borrow_initialized_window(n.roots, 1, 1)?
            } else {
                self.slots
                    .borrow_initialized(if n.argument_plan.is_some() {
                        n.bound
                    } else {
                        n.args
                    })?
            },
            positional_start: matches!(
                n.argument_plan,
                Some(NativeArgumentPlan::PositionalReceiver)
            )
            .then_some(1),
            positions: if n.inline_argument {
                Some(&[0])
            } else {
                n.argument_plan.and_then(|plan| match plan {
                    NativeArgumentPlan::Fixed(plan) => self.program.native_call_slots(plan),
                    NativeArgumentPlan::PositionalReceiver => None,
                })
            },
        })
    }
    /// Split the existing argument roots from mutable heap access for a native
    /// operation. The callback cannot resume the VM or outlive the native frame.
    pub fn with_host_provided_arguments<T>(
        &mut self,
        ticket: HostTicket,
        body: impl FnOnce(&mut Heap, ProvidedArguments<'_>) -> T,
    ) -> Result<T, VmError> {
        let n = self.ticket(ticket)?;
        let provided = ProvidedArguments {
            values: if n.inline_argument {
                self.slots.borrow_initialized_window(n.roots, 1, 1)?
            } else {
                self.slots
                    .borrow_initialized(if n.argument_plan.is_some() {
                        n.bound
                    } else {
                        n.args
                    })?
            },
            positional_start: matches!(
                n.argument_plan,
                Some(NativeArgumentPlan::PositionalReceiver)
            )
            .then_some(1),
            positions: if n.inline_argument {
                Some(&[0])
            } else {
                n.argument_plan.and_then(|plan| match plan {
                    NativeArgumentPlan::Fixed(plan) => self.program.native_call_slots(plan),
                    NativeArgumentPlan::PositionalReceiver => None,
                })
            },
        };
        Ok(body(&mut self.heap, provided))
    }
    pub fn host_continuation(&self, ticket: HostTicket) -> Result<&[Value], VmError> {
        let n = self.ticket(ticket)?;
        // Open already published physical NULLs with clear initialized bits.
        // Only values actually written by the adapter belong to this view.
        self.initialized_prefix(n.roots, 2, n.continuation_len)
    }
    /// Original value of the current callback or continued-wait resume input.
    pub fn host_resume_value(&self, ticket: HostTicket) -> Result<Value, VmError> {
        let n = self.ticket(ticket)?;
        if !matches!(
            ticket.input,
            NativeInput::CallbackReturned
                | NativeInput::CallbackFault
                | NativeInput::WaitingReturned
                | NativeInput::WaitingFault
        ) {
            return Err(VmError::InvalidTicket);
        }
        self.value(n.roots, 0)
    }
    /// Compatibility alias for the unified resume-input getter.
    pub fn host_callback_value(&self, ticket: HostTicket) -> Result<Value, VmError> {
        self.host_resume_value(ticket)
    }
    pub fn host_callback_fault(&self, ticket: HostTicket) -> Result<Option<FaultInfo>, VmError> {
        let n = self.ticket(ticket)?;
        n.callback_fault
            .map(|id| self.heap.fault_info(id).map_err(VmError::from))
            .transpose()
    }
    pub fn host_faults(&self, ticket: HostTicket) -> Result<&[Value], VmError> {
        let n = self.ticket(ticket)?;
        self.initialized_prefix(n.roots, self.native_fault_base(), n.fault_count)
    }
    pub fn host_callback_cause(&self, ticket: HostTicket) -> Result<Option<Value>, VmError> {
        let Some(info) = self.host_callback_fault(ticket)? else {
            return Ok(None);
        };
        Ok(self
            .heap
            .fault_causes(info.id)?
            .next(&self.heap)?
            .map(|c| c.value))
    }
    pub(super) fn make_ticket(
        &self,
        n: &NativeFrame,
        task: TaskId,
        serial: u64,
        input: NativeInput,
    ) -> HostTicket {
        HostTicket {
            heap: self.heap.id(),
            epoch: self.heap.epoch(),
            serial,
            frame: n.id,
            task,
            native: n.native,
            signature: n.signature,
            abi_version: self.program.package().natives[n.native.index()].abi_version,
            owner: n.owner,
            location: n.location,
            input,
            context: n.context,
            effects: n.effects,
            await_activation: n.await_activation,
            continuation: if input == NativeInput::Entry {
                None
            } else {
                Some(ContinuationState {
                    version: n.continuation_version,
                    stage: n.continuation_stage,
                })
            },
        }
    }
    pub(super) fn ticket(&self, ticket: HostTicket) -> Result<NativeFrame, VmError> {
        let task = self.task(ticket.task).map_err(|_| VmError::InvalidTicket)?;
        self.ticket_frame(task, ticket)
    }
    pub(super) fn ticket_frame(
        &self,
        task: &TaskState,
        ticket: HostTicket,
    ) -> Result<NativeFrame, VmError> {
        if ticket.heap != self.heap.id() || ticket.epoch != self.heap.epoch() || task.cancelling {
            return Err(VmError::InvalidTicket);
        }
        match task.frames.data.last() {
            Some(Frame::Native(n)) if matches!(n.phase,NativePhase::Issued {serial,input} if self.make_ticket(n,ticket.task,serial,input)==ticket) => {
                Ok(*n)
            }
            _ => Err(VmError::InvalidTicket),
        }
    }
    /// Accept a response at an exclusive safe point without running a callback.
    /// Payload counts retain their configured limits. Root growth can relocate
    /// the full execution pool (bounded by its configured capacity); it admits
    /// replacement buffers including old/new overlap before publication. This
    /// host entry has no slice work budget. Borrowed inputs and their actual root
    /// window are admitted before ticket consumption. Failed growth preserves
    /// the issued ticket and previous continuation for retry or cancellation.
    pub fn respond(
        &mut self,
        ticket: HostTicket,
        response: HostResponse<'_>,
    ) -> Result<(), VmError> {
        self.select_task(ticket.task)
            .map_err(|_| VmError::InvalidTicket)?;
        let mut n = self.ticket(ticket)?;
        self.prepare_native_response(&mut n, ticket, &response)?;
        match response {
            HostResponse::Ready(v) => {
                self.put(n.roots, 0, v)?;
                n.pending_fault = None;
                n.phase = NativePhase::Commit(false);
            }
            HostResponse::Error(new) => {
                self.put(n.roots, 0, new.value)?;
                if let Some(id) = new.cause {
                    self.put(n.roots, self.native_pending_slot(), id.as_value())?;
                } else {
                    self.clear_native_pending(n.roots)?;
                }
                n.pending_fault = Some(FaultSeed {
                    kind: new.code.kind,
                    location: n.location,
                    fault: None,
                    explicit: new.cause,
                    author_value: true,
                    awaited: None,
                });
                n.phase = NativePhase::Commit(true);
            }
            HostResponse::ForwardFault(id) => {
                let value = self.heap.fault_value(id)?;
                self.put(n.roots, 0, value)?;
                self.put(n.roots, self.native_pending_slot(), id.as_value())?;
                n.pending_fault = Some(self.forwarded_fault(n, id, true)?);
                n.phase = NativePhase::Commit(true);
            }
            HostResponse::Pending(wait) => {
                // NoWait constrains actual waiting, not the native's signature mask.
                if no_wait(n.context) || n.effects.0 & Effects::WAIT == 0 {
                    self.native_fault(&mut n, FaultKind::Effect)?;
                } else {
                    n.phase = NativePhase::Waiting(wait);
                }
            }
            HostResponse::Continue(c) => {
                self.save_continuation(&mut n, c)?;
                n.phase = NativePhase::Continue;
            }
            HostResponse::CallScript {
                closure,
                arguments,
                continuation,
            } => {
                self.save_continuation(&mut n, continuation)?;
                self.put(n.roots, 1, closure)?;
                for (i, &v) in arguments.iter().enumerate() {
                    self.put(
                        n.roots,
                        2 + self.limits.max_continuation_values + i as u32,
                        v,
                    )?;
                }
                n.phase = NativePhase::Callback {
                    count: arguments.len() as u32,
                };
            }
        }
        *self.active.frames.data.last_mut().unwrap() = Frame::Native(n);
        Ok(())
    }
    pub fn respond_language_error(
        &mut self,
        ticket: HostTicket,
        kind: FaultKind,
        details: Value,
    ) -> Result<(), VmError> {
        self.select_task(ticket.task)
            .map_err(|_| VmError::InvalidTicket)?;
        let mut n = self.ticket(ticket)?;
        self.native_fault(&mut n, kind)?;
        self.put(n.roots, 0, details)?;
        *self.active.frames.data.last_mut().unwrap() = Frame::Native(n);
        Ok(())
    }
    pub(super) fn save_continuation(
        &mut self,
        n: &mut NativeFrame,
        c: Continuation<'_>,
    ) -> Result<(), VmError> {
        for (i, &v) in c.values.iter().enumerate() {
            self.put(n.roots, 2 + i as u32, v)?;
        }
        for i in c.values.len() as u32..n.continuation_len {
            self.put(n.roots, 2 + i, Value::NULL)?;
        }
        self.write_native_faults(n, c.faults)?;
        n.continuation_active = true;
        n.continuation_len = c.values.len() as u32;
        n.continuation_version = c.version;
        n.continuation_stage = c.stage;
        Ok(())
    }
}
