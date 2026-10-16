//! Derived computations use ordinary native continuations and script frames.
use super::*;
use crate::heap::DerivedSlot;

impl Vm {
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

    pub fn call_reactive(
        &mut self,
        ticket: HostTicket,
        output: Value,
        closure: Value,
        pure: bool,
        retained: &[Value],
    ) -> Result<(), VmError> {
        self.select_task(ticket.task)?;
        let native = self.ticket(ticket)?;
        if native.derived_output.is_none() {
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
        let Frame::Native(frame) = self.active.frames.data.last_mut().unwrap() else {
            unreachable!()
        };
        frame.context.derived |= pure;
        frame.context.no_wait = true;
        frame.derived_output = Some(output);
        Ok(())
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
