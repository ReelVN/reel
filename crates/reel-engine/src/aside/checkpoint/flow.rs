//! Current Root State and the single native business callback boundary.
use crate::aside::{AdapterError, NativeBudget, NativeDispatch, Session, SessionVm};
use aside_vm::{
    Heap, RootKind, RootToken, Value,
    executor::{
        Continuation, FaultRetention, FrameId, HostResponse, HostTicket, NativeInput, TaskId,
    },
    resume::RootKey,
    typed_data::{Registry, RegistryInitCursor},
};
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(in crate::aside) struct FlowRuntime {
    pub(in crate::aside) registry: Option<Registry>,
    pub(in crate::aside) registry_init: Option<RegistryInitCursor>,
    pub(in crate::aside) active: Option<ActiveFlow>,
}
pub(in crate::aside) struct ActiveFlow {
    pub(in crate::aside) task: TaskId,
    pub(in crate::aside) boundary: FrameId,
    pub(in crate::aside) root_state: RootToken,
    pub(in crate::aside) context: RootToken,
}
pub(in crate::aside) struct FlowEntry {
    pub(in crate::aside) entry: Value,
    pub(in crate::aside) state: Value,
    pub(in crate::aside) context: Value,
    pub(in crate::aside) work: u32,
}
pub(in crate::aside) enum FlowPreparation {
    Ready(FlowEntry),
    Pending(NativeDispatch),
}
/// Current native callback state shared by new execution and verified restoration.
pub(in crate::aside) fn flow_continuation(values: &[Value]) -> Continuation<'_> {
    Continuation {
        version: 1,
        stage: 0,
        values,
        faults: FaultRetention::Preserve,
    }
}
impl FlowRuntime {
    pub(in crate::aside) fn clear_active(&mut self, heap: &mut Heap) -> Result<(), AdapterError> {
        if let Some(active) = self.active.take() {
            heap.release_root(active.root_state)?;
            heap.release_root(active.context)?;
        }
        Ok(())
    }
    pub(in crate::aside) fn clear(&mut self, heap: &mut Heap) -> Result<(), AdapterError> {
        self.clear_active(heap)?;
        if let Some(cursor) = self.registry_init.take() {
            cursor.cancel(heap)?;
        }
        if let Some(registry) = self.registry.take() {
            registry.release(heap)?;
        }
        Ok(())
    }
}
impl SessionVm {
    /// Current main task and native callback frame that delimit saved business frames.
    pub fn flow_boundary(&self) -> Option<(TaskId, FrameId)> {
        self.flow
            .active
            .as_ref()
            .map(|flow| (flow.task, flow.boundary))
    }
    /// Stable Root identity associated with the active business callback.
    pub fn flow_root_key(&self) -> Option<&RootKey> {
        self.flow.active.as_ref()?;
        self.resume.table().root.as_ref().map(|root| &root.key)
    }
    /// Independently rooted State, including when no business local still reads it.
    pub fn flow_root_state(&self) -> Result<Option<Value>, AdapterError> {
        self.flow
            .active
            .as_ref()
            .map(|flow| {
                self.vm
                    .heap()
                    .root_value(flow.root_state)
                    .map_err(Into::into)
            })
            .transpose()
    }
    /// Completed current-class registry, shared by checkpoint capture and restoration.
    pub fn flow_registry(&self) -> Option<&Registry> {
        self.flow.registry.as_ref()
    }

    /// Advance automatic State registration after current module initialization.
    /// An incomplete result leaves its class roots owned by this session.
    pub fn prepare_flow_registry(
        &mut self,
        budget: NativeBudget,
    ) -> Result<NativeDispatch, AdapterError> {
        if self.revoked {
            return Err(AdapterError::Input("owner revoked"));
        }
        if self.flow.registry.is_some() {
            return Ok(NativeDispatch::Ready { work: 0 });
        }
        let work = budget.with_minimum(1);
        if work == 0 {
            return Ok(NativeDispatch::BudgetYield {
                work: 0,
                required_work: 1,
            });
        }
        let mut spent = 0;
        if self.flow.registry_init.is_none() {
            self.flow.registry_init = Some(RegistryInitCursor::new(
                &mut self.vm,
                &self.resume,
                self.root_owner,
            )?);
            spent = 1;
        }
        while spent < work {
            spent += 1;
            match self
                .flow
                .registry_init
                .as_mut()
                .unwrap()
                .step(&mut self.vm, 1)
            {
                Ok(true) => {
                    let cursor = self.flow.registry_init.take().unwrap();
                    self.flow.registry = Some(cursor.finish(self.vm.heap_mut())?);
                    return Ok(NativeDispatch::Ready { work: spent });
                }
                Ok(false) => {}
                Err(error) => {
                    self.flow
                        .registry_init
                        .take()
                        .unwrap()
                        .cancel(self.vm.heap_mut())?;
                    return Err(error.into());
                }
            }
        }
        Ok(NativeDispatch::Decoding { work: spent })
    }

    /// Prepare one live ticket without consuming it. Ready values are still held
    /// by that ticket; invoke or restore its callback before advancing the task.
    pub(in crate::aside) fn prepare_flow_entry(
        &mut self,
        session: &Session,
        ticket: HostTicket,
        budget: NativeBudget,
    ) -> Result<FlowPreparation, AdapterError> {
        self.check_story_wait(session, ticket)?;
        let program = self.vm.program().package();
        if program.symbols[program.natives[ticket.native.index()].symbol.index()]
            != "reel::run_flow"
        {
            return Err(AdapterError::Input(
                "flow preparation requires the current run_flow ticket",
            ));
        }
        if ticket.input != NativeInput::Entry {
            return Err(AdapterError::Input("run_flow requires its entry ticket"));
        }
        if let Some(active) = &self.flow.active {
            return if (active.task, active.boundary) == (ticket.task, ticket.frame) {
                let args = self.vm.host_arguments(ticket)?;
                Ok(FlowPreparation::Ready(FlowEntry {
                    entry: args[0],
                    state: args[1],
                    context: args[2],
                    work: 0,
                }))
            } else {
                Err(AdapterError::Input("a business flow is already active"))
            };
        }
        let root = self
            .resume
            .table()
            .root
            .as_ref()
            .ok_or(AdapterError::Input(
                "run_flow requires a declared Root and State",
            ))?;
        let required = u32::try_from(
            self.resume.table().flows.len()
                + self.resume.table().states.len()
                + root.state.module.len()
                + root.state.name.len()
                + 5,
        )
        .unwrap_or(u32::MAX);
        let available = budget.with_minimum(required.max(1));
        let spent = match self.prepare_flow_registry(NativeBudget::Exact(available))? {
            NativeDispatch::Ready { work } => {
                let remaining = available.saturating_sub(work);
                if remaining < required {
                    return Ok(FlowPreparation::Pending(NativeDispatch::BudgetYield {
                        work,
                        required_work: required,
                    }));
                }
                work
            }
            pending => return Ok(FlowPreparation::Pending(pending)),
        };
        let args = self.vm.host_arguments(ticket)?;
        let (entry, state, context) = (args[0], args[1], args[2]);
        if !matches!(
            context.tag(),
            aside_vm::ValueTag::Null | aside_vm::ValueTag::Object
        ) {
            return Err(AdapterError::Input(
                "run_flow context must be an object or null",
            ));
        }
        let function = self.vm.unbound_function(entry)?;
        if !self
            .resume
            .table()
            .flows
            .iter()
            .any(|flow| flow.function == function)
        {
            return Err(AdapterError::Input(
                "run_flow entry must be a declared current flow",
            ));
        }
        let registry = self.flow.registry.as_ref().unwrap();
        let class = registry.class_for_instance(self.vm.heap(), state)?;
        let expected = &self.resume.table().root.as_ref().unwrap().state;
        if &registry.schema(class)?.key != expected {
            return Err(AdapterError::Input(
                "run_flow State differs from its declared Root State",
            ));
        }
        let root_state = self
            .vm
            .heap_mut()
            .add_root(self.root_owner, RootKind::Host, state)?;
        let context = match self
            .vm
            .heap_mut()
            .add_root(self.root_owner, RootKind::Host, context)
        {
            Ok(root) => root,
            Err(error) => {
                self.vm.heap_mut().release_root(root_state)?;
                return Err(error.into());
            }
        };
        self.flow.active = Some(ActiveFlow {
            task: ticket.task,
            boundary: ticket.frame,
            root_state,
            context,
        });
        Ok(FlowPreparation::Ready(FlowEntry {
            entry,
            state,
            context: self.vm.heap().root_value(context)?,
            work: spent + required,
        }))
    }

    pub(in crate::aside) fn dispatch_run_flow(
        &mut self,
        session: &Session,
        ticket: HostTicket,
        budget: NativeBudget,
    ) -> Result<NativeDispatch, AdapterError> {
        if budget.with_minimum(1) == 0 {
            return Ok(NativeDispatch::BudgetYield {
                work: 0,
                required_work: 1,
            });
        }
        let mut work = 1;
        if ticket.input == NativeInput::Entry {
            let entry = match self.prepare_flow_entry(session, ticket, budget)? {
                FlowPreparation::Ready(entry) => entry,
                FlowPreparation::Pending(pending) => return Ok(pending),
            };
            work = entry.work.max(1);
            let values = [entry.state, entry.context];
            let arguments = if entry.context.tag() == aside_vm::ValueTag::Null {
                &values[..1]
            } else {
                &values[..]
            };
            if let Err(error) = self.vm.respond(
                ticket,
                HostResponse::CallScript {
                    closure: entry.entry,
                    arguments,
                    continuation: flow_continuation(&values),
                },
            ) {
                self.flow.clear_active(self.vm.heap_mut())?;
                return Err(error.into());
            }
        } else {
            if !self
                .flow
                .active
                .as_ref()
                .is_some_and(|active| (active.task, active.boundary) == (ticket.task, ticket.frame))
            {
                return Err(AdapterError::Input(
                    "run_flow callback has no active Root State",
                ));
            }
            let state = ticket
                .continuation
                .ok_or(AdapterError::Input("run_flow continuation is missing"))?;
            if state.version != 1 || state.stage != 0 {
                return Err(AdapterError::Input("unexpected run_flow continuation"));
            }
            match ticket.input {
                NativeInput::CallbackReturned => {
                    let value = self.vm.host_resume_value(ticket)?;
                    self.vm.respond(ticket, HostResponse::Ready(value))?;
                }
                NativeInput::CallbackFault => {
                    let fault = self
                        .vm
                        .host_callback_fault(ticket)?
                        .ok_or(AdapterError::Input("run_flow callback fault is missing"))?;
                    self.vm
                        .respond(ticket, HostResponse::ForwardFault(fault.id))?;
                }
                NativeInput::CallbackCancelled => self.vm.respond_language_error(
                    ticket,
                    aside_vm::executor::FaultKind::Cancelled,
                    Value::NULL,
                )?,
                _ => return Err(AdapterError::Input("unexpected run_flow callback input")),
            }
            self.flow.clear_active(self.vm.heap_mut())?;
        }
        self.issued = None;
        Ok(NativeDispatch::Ready { work })
    }
    pub(in crate::aside) fn retire_flow_region(
        &mut self,
        task: TaskId,
        frame: Option<FrameId>,
    ) -> Result<(), AdapterError> {
        if self.flow.active.as_ref().is_some_and(|active| {
            active.task == task && frame.is_none_or(|frame| active.boundary.0 > frame.0)
        }) {
            self.flow.clear_active(self.vm.heap_mut())?;
        }
        Ok(())
    }
}

pub(in crate::aside) fn contracts() -> Vec<aside_host::NativeContract> {
    use aside_vm::bytecode::{BuiltinType, Effects};
    vec![aside_host::NativeContract {
        standard_derived_commit: false,
        read_only_leaf: None,
        rest: None,
        symbol: "reel::run_flow".into(),
        abi_version: 1,
        parameters: vec![
            aside_host::NativeParameter {
                name: "entry".into(),
                ty: BuiltinType::Closure,
                default: None,
            },
            aside_host::NativeParameter {
                name: "state".into(),
                ty: BuiltinType::Any,
                default: None,
            },
            aside_host::NativeParameter {
                name: "context".into(),
                ty: BuiltinType::Any,
                default: Some(aside_vm::bytecode::Const::Null),
            },
        ],
        result: BuiltinType::Any,
        effects: Effects::ALL,
        required_capabilities: Vec::new(),
    }]
}
pub(in crate::aside) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    api.refine(
        "reel::run_flow",
        TypeHint::Any,
        &[
            ("entry", TypeHint::Closure),
            ("state", TypeHint::Any),
            ("context", TypeHint::Any),
        ],
    );
}
aside_host::native_documentation! {
    pub(in crate::aside) fn documentation {
        /// Run one declared business flow with the current Root State in the main task.
        /// The entry is an ordinary function without captures. Its callback inherits
        /// the current Reading, Scope, owner and effects, and returns its normal result.
        /// Declare the State class with `std::resume::state` and the bootstrap with
        /// `std::resume::root`; the instance's registered type must match that Root.
        /// Root State stays independently rooted until the boundary returns or retires.
        /// Pass `context` to call entry(State, context) with current Root services.
        /// Its direct live aliases reconnect to the current context on loading;
        /// its fields and callbacks remain owned by the current Root definition.
        /// A table context explicitly names current services, such as
        /// `{scene: scene, poem: poem, selection: completion}`. Live aliases of
        /// its direct object members reconnect by the same string key on loading.
        /// Build the current operations and their terminal state from State first.
        "reel::run_flow";
    }
}
