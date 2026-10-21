//! Derived computations use ordinary native continuations and script frames.
use super::*;
use crate::heap::DerivedSlot;

pub(super) struct ReactiveRead {
    output: Value,
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
        let push = self.native_push_cost(native, count);
        cost.0 += push.0 + u64::from(count) + self.heap.derived_claim_work() + 2;
        cost.1 += push.1;
        Ok(Some(ReactiveRead {
            output,
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
        s: ScriptFrame,
        read: ReactiveRead,
    ) -> Result<(), VmError> {
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
        // Reuse the native frame's existing callback-argument root region as its
        // copy source. No temporary argument graph or separate task is needed.
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
            self.put(
                frame.roots,
                2 + self.limits.max_continuation_values + i,
                self.heap.sequence_get(arguments, i)?,
            )?;
        }
        frame.derived_output = Some(read.output);
        frame.continuation_len = 1;
        frame.phase = NativePhase::Copy {
            source: ArgSource::Native { frame: frame.id },
            index: 0,
        };
        self.active.frames.data[index] = Frame::Native(frame);
        self.heap.claim_derived(read.output)?;
        Ok(())
    }
    pub fn respond_derive(&mut self, ticket: HostTicket) -> Result<(), VmError> {
        self.select_task(ticket.task)?;
        let native = self.ticket(ticket)?;
        match ticket.input {
            NativeInput::Entry => {
                let closure = self.host_arguments(ticket)?[0];
                self.closure_function(closure)?;
                let output = if let Some(output) = native.derived_output {
                    output
                } else {
                    self.heap.alloc_derived(closure)?
                };
                self.call_reactive(
                    ticket,
                    output,
                    closure,
                    true,
                    &[output, Value::bool(native.derived_output.is_some())],
                )?;
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

    pub fn start_derived(&mut self, output: Value, owner: RootOwner) -> Result<TaskId, VmError> {
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
        Ok(task)
    }
}
