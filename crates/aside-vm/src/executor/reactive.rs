//! Derived computations share ordinary script semantics and native commit continuations.
use super::*;
use crate::heap::DerivedSlot;

pub(super) struct ReactiveRead {
    output: Value,
    compact: Option<FunctionId>,
    native: NativeId,
    count: u32,
    location: Location,
}

impl Vm {
    pub(super) fn reactive_read_plan(
        &mut self,
        property: crate::PropertyId,
        cost: &mut (u64, u64),
    ) -> Result<Option<ReactiveRead>, VmError> {
        let (output, work) = self.heap.reactive_read_source(property)?;
        cost.0 += work;
        let Some(output) = output else {
            return Ok(None);
        };
        let arguments = self
            .heap
            .host_payload(output, DerivedSlot::Arguments as u32)?;
        let count = self.heap.sequence_len(arguments)?;
        let function = FunctionId(
            self.heap
                .host_payload(output, DerivedSlot::Function as u32)?
                .as_int()
                .unwrap() as u32,
        );
        let FunctionKind::Native(native) = self.program.package().functions[function.index()].kind
        else {
            unreachable!("observer entry is native")
        };
        let origin = FunctionId(
            self.heap
                .host_payload(output, DerivedSlot::Origin as u32)?
                .as_int()
                .unwrap() as u32,
        );
        let pc = Pc(self
            .heap
            .host_payload(output, DerivedSlot::CallPc as u32)?
            .as_int()
            .unwrap() as u32);
        let compact = if self.compact_output(output)? {
            Some(self.closure_function(self.heap.derived_closure(output)?)?)
        } else {
            None
        };
        let push = compact.map_or_else(
            || self.native_push_cost(native, count),
            |function| self.script_push_cost(function, 0),
        );
        cost.0 += push.0 + 3 * u64::from(count) + self.heap.derived_claim_work() + 12;
        cost.1 += push.1;
        Ok(Some(ReactiveRead {
            output,
            compact,
            native,
            count,
            location: Location {
                code: CodeRef {
                    function: origin,
                    version: self.program.package().content_version,
                },
                pc,
                source: self.program.source_at(origin, pc),
            },
        }))
    }

    pub(super) fn push_reactive_read(
        &mut self,
        s: &ScriptFrame,
        read: ReactiveRead,
    ) -> Result<(), VmError> {
        if let Some(function) = read.compact {
            let closure = self.heap.derived_closure(read.output)?;
            self.push_script(
                function,
                Some(closure),
                ArgSource::None,
                0,
                s.owner,
                inherit(s.context, Context::Derived),
                s.effects,
                ReturnTo::ReactiveRead {
                    frame: s.id,
                    call: s.pc,
                },
            )?;
            let signature = self.program.package().natives[read.native.index()].signature;
            let effects = Effects(
                s.effects.0
                    & self.program.package().signatures[signature.index()]
                        .effects
                        .0,
            );
            self.enter_compact_derived(read.output, s.module, effects)?;
            self.heap.claim_derived(read.output)?;
            return Ok(());
        }
        let site = NativeSite {
            owner: s.owner,
            module: s.module,
            context: s.context,
            effects: s.effects,
            location: read.location,
            ret: ReturnTo::ReactiveRead {
                frame: s.id,
                call: s.pc,
            },
        };
        // These arguments already belong to the observer. Copy them directly
        // into its admitted argument window instead of reserving callback slots.
        self.push_native(read.native, ArgSource::None, read.count, site)?;
        let index = self.active.frames.data.len() - 1;
        let Frame::Native(mut frame) = self.active.frames.data[index] else {
            unreachable!()
        };
        self.put(frame.roots, 2, read.output)?;
        let arguments = self
            .heap
            .host_payload(read.output, DerivedSlot::Arguments as u32)?;
        for i in 0..read.count {
            self.put(frame.args, i * 2, Value::int(-1))?;
            self.put(frame.args, i * 2 + 1, self.heap.sequence_get(arguments, i)?)?;
        }
        frame.derived_output = Some(read.output);
        frame.continuation_len = 1;
        frame.phase = NativePhase::Bind(ArgumentBinding::new(0));
        self.active.frames.data[index] = Frame::Native(frame);
        self.heap.claim_derived(read.output)?;
        self.derived_starts += 1;
        Ok(())
    }
    /// Implement the standard read-only derived-value contract. Admitted parameterless
    /// expressions retain their script instructions and may defer the native bridge
    /// until this same commit continuation. `standard_derived_commit` registration
    /// permits the VM to absorb an existing compact observer's commit ticket;
    /// custom observers use `call_reactive` and retain host interception.
    pub fn respond_derive(&mut self, ticket: HostTicket) -> Result<(), VmError> {
        self.select_task(ticket.task)?;
        let native = self.ticket(ticket)?;
        match ticket.input {
            NativeInput::Entry => {
                let closure = self.host_arguments(ticket)?[0];
                let function = self.closure_function(closure)?;
                let output = if let Some(output) = native.derived_output {
                    output
                } else {
                    self.heap.alloc_derived(closure)?
                };
                let signature = &self.program.package().signatures[native.signature.index()];
                let compact = signature.parameters.len() == 1
                    && signature.rest.is_none()
                    && native.bindings.is_none()
                    && self.program.compact_derived(function);
                if compact {
                    self.heap.set_host_payload(
                        output,
                        DerivedSlot::Compact as u32,
                        Value::bool(true),
                    )?;
                }
                self.call_reactive(
                    ticket,
                    output,
                    closure,
                    true,
                    &[output, Value::bool(native.derived_output.is_some())],
                )?;
                if native.derived_output.is_none() {
                    self.derived_outputs += 1;
                    self.compact_derived_outputs += u64::from(compact);
                }
            }
            NativeInput::CallbackReturned => {
                let output = self.host_continuation(ticket)?[0];
                let value = self.host_callback_value(ticket)?;
                if let Err(error) = self.heap.commit_derived(output, value) {
                    self.heap.end_derived(output)?;
                    self.respond_language_error(ticket, heap_fault(error), Value::NULL)?;
                } else {
                    let reference = if self.host_continuation(ticket)?[1].as_bool() == Some(true) {
                        Value::NULL
                    } else {
                        self.heap.alloc_property_ref(
                            crate::PropertyId {
                                host: output,
                                field: 0,
                            },
                            false,
                        )?
                    };
                    self.respond(ticket, HostResponse::Ready(reference))?;
                }
            }
            NativeInput::CallbackFault => {
                let output = self.host_continuation(ticket)?[0];
                self.heap.end_derived(output)?;
                self.respond(
                    ticket,
                    HostResponse::ForwardFault(native.callback_fault.unwrap()),
                )?;
            }
            _ => return Err(VmError::InvalidTicket),
        }
        Ok(())
    }

    pub fn reactive_output(&self, ticket: HostTicket) -> Result<Option<Value>, VmError> {
        Ok(self.ticket(ticket)?.derived_output)
    }

    /// A scheduled observer owns its output before its first native admission,
    /// including a budget yield between next_derived and begin_derived.
    pub fn reactive_task_output(&self, task: TaskId) -> Result<Option<Value>, VmError> {
        Ok(match self.task(task)?.frames.data.first() {
            Some(Frame::Native(frame)) => frame.derived_output,
            Some(Frame::Script(frame)) if frame.derived_bridge.is_some() => {
                Some(self.value(frame.env, 4)?)
            }
            _ => None,
        })
    }

    pub fn call_reactive(
        &mut self,
        ticket: HostTicket,
        output: Value,
        closure: Value,
        pure: bool,
        retained: &[Value],
    ) -> Result<(), VmError> {
        self.prepare_reactive(ticket, output)?;
        if let Err(error) = self.respond(
            ticket,
            HostResponse::CallScript {
                closure,
                arguments: &[],
                continuation: Continuation {
                    version: 1,
                    stage: 0,
                    values: retained,
                    faults: FaultRetention::Preserve,
                },
            },
        ) {
            self.heap.end_derived(output)?;
            return Err(error);
        }
        self.enter_reactive_context(output, pure);
        Ok(())
    }

    /// Start a host-maintained observer in the same native frame. The caller
    /// retains `output` in its continuation, and commits or ends the capture
    /// exactly as for `call_reactive`; no script callback is required.
    pub fn begin_reactive(
        &mut self,
        ticket: HostTicket,
        output: Value,
        pure: bool,
        continuation: Continuation<'_>,
    ) -> Result<(), VmError> {
        self.prepare_reactive(ticket, output)?;
        if let Err(error) = self.respond(ticket, HostResponse::Continue(continuation)) {
            self.heap.end_derived(output)?;
            return Err(error);
        }
        self.enter_reactive_context(output, pure);
        Ok(())
    }

    fn prepare_reactive(&mut self, ticket: HostTicket, output: Value) -> Result<(), VmError> {
        self.select_task(ticket.task)?;
        let native = self.ticket(ticket)?;
        if self
            .heap
            .host_payload(output, DerivedSlot::Function as u32)?
            .same_bits(Value::NULL)
        {
            let arguments = self.heap.alloc_array(&[])?;
            let count = self.host_arguments(ticket)?.len();
            for i in 0..count {
                self.heap
                    .array_push(arguments, self.host_arguments(ticket)?[i])?;
            }
            self.heap
                .set_host_payload(output, DerivedSlot::Arguments as u32, arguments)?;
            let function = self
                .program
                .package()
                .functions
                .iter()
                .position(|f| f.kind == FunctionKind::Native(native.native))
                .ok_or(VmError::InvalidTicket)?;
            self.heap.set_host_payload(
                output,
                DerivedSlot::Function as u32,
                Value::int(function as i64),
            )?;
            self.heap.set_host_payload(
                output,
                DerivedSlot::Origin as u32,
                Value::int(native.location.code.function.0 as i64),
            )?;
            self.heap.set_host_payload(
                output,
                DerivedSlot::CallPc as u32,
                Value::int(native.location.pc.0 as i64),
            )?;
        }
        self.heap.begin_derived(output)?;
        Ok(())
    }

    fn enter_reactive_context(&mut self, output: Value, pure: bool) {
        let Frame::Native(frame) = self.active.frames.data.last_mut().unwrap() else {
            unreachable!()
        };
        frame.context.derived |= pure;
        frame.context.no_wait = true;
        frame.derived_output = Some(output);
    }

    /// Schedule an existing observer, retaining its output before execution begins.
    /// Standard compact expressions enter their commit continuation after the
    /// budgeted script phase. The native registration controls host interception;
    /// both paths retain ordinary GC and snapshot ownership.
    pub fn start_derived(&mut self, output: Value, owner: RootOwner) -> Result<TaskId, VmError> {
        if self.compact_output(output)? {
            let function = FunctionId(
                self.heap
                    .host_payload(output, DerivedSlot::Function as u32)?
                    .as_int()
                    .unwrap() as u32,
            );
            let module =
                ModuleInstanceId(self.program.package().functions[function.index()].module.0);
            let task = self.start_closure(
                self.heap.derived_closure(output)?,
                &[],
                owner,
                Context::Derived,
            )?;
            if let Err(error) = self.enter_compact_derived(
                output,
                module,
                self.program.package().signatures[self.program.package().functions
                    [function.index()]
                .signature
                .index()]
                .effects,
            ) {
                self.forget_active_task();
                self.active.task_owner = None;
                return Err(error);
            }
            return Ok(task);
        }
        let arguments = self
            .heap
            .host_payload(output, DerivedSlot::Arguments as u32)?;
        let mut args = crate::root_ranges::ChargedVec::new(
            &self.heap.budget_usage(),
            self.heap.sequence_len(arguments)? as usize,
        )?;
        for i in 0..self.heap.sequence_len(arguments)? {
            args.data.push(self.heap.sequence_get(arguments, i)?);
        }
        let function = FunctionId(
            self.heap
                .host_payload(output, DerivedSlot::Function as u32)?
                .as_int()
                .unwrap() as u32,
        );
        let origin = FunctionId(
            self.heap
                .host_payload(output, DerivedSlot::Origin as u32)?
                .as_int()
                .unwrap() as u32,
        );
        let pc = Pc(self
            .heap
            .host_payload(output, DerivedSlot::CallPc as u32)?
            .as_int()
            .unwrap() as u32);
        let task = self.start_impl(function, None, &args.data, owner, Context::Ordinary)?;
        let Frame::Native(mut frame) = *self.active.frames.data.last().unwrap() else {
            unreachable!()
        };
        frame.derived_output = Some(output);
        frame.location = Location {
            code: CodeRef {
                function: origin,
                version: self.program.package().content_version,
            },
            pc,
            source: self.program.source_at(origin, pc),
        };
        self.put(frame.roots, 2, output)?;
        frame.continuation_len = 1;
        *self.active.frames.data.last_mut().unwrap() = Frame::Native(frame);
        self.derived_starts += 1;
        Ok(task)
    }
}
