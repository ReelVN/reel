//! Dependency-driven one-shot condition results.
use super::*;

aside_host::native_documentation! {
    /// Observe a read-only boolean predicate until it first becomes true.
    ///
    /// Observable reads subscribe to dependency changes. False keeps observing;
    /// true resolves the repeatable Awaitable and disconnects its dependencies.
    /// The result exposes cancel() and observable pending. Owner retirement
    /// cancels it; cancelling a consumer removes only that consumer's wait.
    /// Non-observable captures are evaluated once. Predicate faults preserve
    /// their original script location, and pending conditions survive snapshots.
    ///
    /// ```aside
    /// fn main() {
    ///     let state = model({ready: true});
    ///     await when(|| state.ready);
    /// }
    /// ```
    pub(super) const DOCUMENTATION;
}

impl SessionVm {
    pub(super) fn dispatch_when(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        match ticket.input {
            NativeInput::Entry => {
                let predicate = self.vm.host_arguments(ticket)?[0];
                let (output, result) = if let Some(output) = self.vm.reactive_output(ticket)? {
                    let result = self.vm.heap().derived_state(output)?;
                    if !results::pending(self.vm.heap_mut(), result)? {
                        self.vm.heap_mut().disconnect_derived(output)?;
                        self.vm.respond(ticket, HostResponse::Ready(result))?;
                        self.issued = None;
                        return Ok(());
                    }
                    (output, result)
                } else {
                    if ticket.context.init || ticket.context.no_wait || ticket.context.derived {
                        return Err(AdapterError::Vm(VmError::Effect));
                    }
                    let requested = self.vm.host_arguments(ticket)?[1];
                    let owner = if requested.tag() == aside_vm::ValueTag::Null {
                        self.default_activity_owner(ticket.task)?
                    } else {
                        requested
                    };
                    scopes::validate_owner(self.vm.heap(), &self.stage, owner)?;
                    let result =
                        self.results
                            .create_condition(&mut self.vm, self.root_owner, owner)?;
                    let evaluator = self
                        .vm
                        .program()
                        .package()
                        .functions
                        .iter()
                        .enumerate()
                        .find_map(|(index, function)| {
                            if let aside_vm::bytecode::FunctionKind::Native(native) = function.kind
                                && self.vm.program().package().symbols[self
                                    .vm
                                    .program()
                                    .package()
                                    .natives[native.index()]
                                .symbol
                                .index()]
                                    == "reel::when::evaluate"
                            {
                                Some(FunctionId(index as u32))
                            } else {
                                None
                            }
                        })
                        .ok_or(AdapterError::Input("condition evaluator missing"))?;
                    let checked = self.vm.bind_native(predicate, evaluator)?;
                    let output = self.vm.heap_mut().alloc_derived(checked)?;
                    self.vm.heap_mut().set_derived_state(output, result)?;
                    results::attach(self.vm.heap_mut(), result, output)?;
                    (output, result)
                };
                let checked = self.vm.heap().derived_closure(output)?;
                self.vm
                    .call_reactive(ticket, output, checked, true, &[output, result])?;
            }
            NativeInput::CallbackReturned => {
                let saved = self.vm.host_continuation(ticket)?;
                let (output, result) = (saved[0], saved[1]);
                let value = self.vm.host_callback_value(ticket)?;
                let ready = value.as_bool().expect("condition evaluator returns bool");
                self.vm.heap_mut().commit_derived(output, value)?;
                self.vm.heap_mut().end_derived(output)?;
                if ready {
                    self.results.settle_condition(
                        self.vm.heap_mut(),
                        result,
                        Value::bool(true),
                        false,
                    )?;
                }
                self.vm.respond(ticket, HostResponse::Ready(result))?;
            }
            NativeInput::CallbackFault => {
                let saved = self.vm.host_continuation(ticket)?;
                let (output, result) = (saved[0], saved[1]);
                let fault = self.vm.host_callback_fault(ticket)?.unwrap();
                self.vm.heap_mut().end_derived(output)?;
                self.results.settle_condition(
                    self.vm.heap_mut(),
                    result,
                    fault.as_value(),
                    true,
                )?;
                self.vm.respond(ticket, HostResponse::Ready(result))?;
            }
            _ => return Err(AdapterError::Input("when continuation")),
        }
        self.issued = None;
        Ok(())
    }

    pub(super) fn evaluate_condition(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        match ticket.input {
            NativeInput::Entry => {
                let predicate = self.vm.host_arguments(ticket)?[0];
                self.vm.respond(
                    ticket,
                    HostResponse::CallScript {
                        closure: predicate,
                        arguments: &[],
                        continuation: aside_vm::executor::Continuation {
                            version: 1,
                            stage: 0,
                            values: &[],
                            faults: aside_vm::executor::FaultRetention::Preserve,
                        },
                    },
                )?;
            }
            NativeInput::CallbackReturned => {
                let value = self.vm.host_callback_value(ticket)?;
                if value.as_bool().is_none() {
                    let details = self
                        .vm
                        .heap_mut()
                        .alloc_string("when predicate must return bool")?;
                    self.vm
                        .respond_language_error(ticket, FaultKind::Type, details)?;
                } else {
                    self.vm.respond(ticket, HostResponse::Ready(value))?;
                }
            }
            NativeInput::CallbackFault => {
                let fault = self.vm.host_callback_fault(ticket)?.unwrap();
                self.vm
                    .respond(ticket, HostResponse::ForwardFault(fault.id))?;
            }
            _ => return Err(AdapterError::Input("condition evaluator continuation")),
        }
        self.issued = None;
        Ok(())
    }
}
