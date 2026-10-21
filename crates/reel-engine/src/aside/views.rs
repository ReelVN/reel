//! A stable stage container with dependency-driven component construction.
use super::*;

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Create a stable container whose child is built by a non-waiting reactive function.
        /// Return a node or null. Observable dependency changes rerun the builder. Returning
        /// a different child retires the previous subtree and its owned work; returning the
        /// same child preserves it. The builder may construct nodes and start non-waiting
        /// activities; the enclosing container keeps its identity.
        "reel::view";
    }
}

impl SessionVm {
    pub(super) fn dispatch_view(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        match ticket.input {
            NativeInput::Entry => {
                let args = self.vm.host_arguments(ticket)?.to_vec();
                let (output, node) = if let Some(output) = self.vm.reactive_output(ticket)? {
                    let node = self.vm.heap().derived_state(output)?;
                    if self.stage.id(self.vm.heap(), node).is_err() {
                        self.vm.heap_mut().disconnect_derived(output)?;
                        self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?;
                        self.issued = None;
                        return Ok(());
                    }
                    (output, node)
                } else {
                    let mut layout = args[1..].to_vec();
                    layout.push(Value::NULL);
                    let origin = self.stage_origin(ticket);
                    let node = self.stage.dispatch(
                        &mut self.vm,
                        self.root_owner,
                        "reel::stack",
                        &layout,
                        self.read_limit,
                        origin,
                    )?;
                    let output = self.vm.heap_mut().alloc_derived(args[0])?;
                    self.vm.heap_mut().set_derived_state(output, node)?;
                    self.vm.heap_mut().set_host_payload(node, 3, output)?;
                    (output, node)
                };
                self.vm
                    .call_reactive(ticket, output, args[0], false, &[output, node])?;
            }
            NativeInput::CallbackReturned => {
                let retained = self.vm.host_continuation(ticket)?;
                let (output, node) = (retained[0], retained[1]);
                let child = self.vm.host_callback_value(ticket)?;
                // Keep the last mounted subtree until both computation and mount admission succeed.
                let result = self
                    .vm
                    .heap_mut()
                    .commit_derived(output, node)
                    .map_err(AdapterError::from)
                    .and_then(|()| {
                        self.stage
                            .replace_view(self.vm.heap_mut(), node, child, self.read_limit)
                    });
                self.vm.heap_mut().end_derived(output)?;
                match result {
                    Ok(()) => self.vm.respond(ticket, HostResponse::Ready(node))?,
                    Err(error) => return Err(error),
                }
            }
            NativeInput::CallbackFault => {
                let output = self.vm.host_continuation(ticket)?[0];
                self.vm.heap_mut().end_derived(output)?;
                self.vm.respond(
                    ticket,
                    HostResponse::ForwardFault(self.vm.host_callback_fault(ticket)?.unwrap().id),
                )?;
            }
            _ => return Err(AdapterError::Input("view continuation")),
        }
        self.issued = None;
        Ok(())
    }
}
