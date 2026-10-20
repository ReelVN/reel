//! Native callback lifetimes share the ordinary host-owner and task protocols.
use super::*;
use aside_vm::executor::{Continuation, FaultRetention, FrameId};
use aside_vm::{Heap, HeapReservation, ValueTag};
use std::collections::VecDeque;

const TAG: i64 = -1108;
pub(super) fn is_scope(heap: &Heap, value: Value) -> bool {
    heap.host_payload(value, 0)
        .is_ok_and(|tag| tag.as_int() == Some(TAG))
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Scope {
    pub task: TaskId,
    pub frame: FrameId,
    root: RootToken,
}

#[derive(Default)]
pub(super) struct Scopes {
    pub methods: Option<[FunctionId; 6]>,
    pub live: VecDeque<Scope>,
    charge: Option<HeapReservation>,
}

pub(super) fn validate_owner(
    heap: &Heap,
    stage: &stage::StageBinding,
    owner: Value,
) -> Result<(), AdapterError> {
    if owner.tag() != ValueTag::Null && heap.host_payload(owner, 0)?.as_int() != Some(TAG) {
        stage.id(heap, owner)?;
    }
    Ok(())
}

pub(super) fn owner_live(heap: &Heap, owner: Value) -> bool {
    owner.tag() == ValueTag::Null || heap.host_fields(owner).is_ok()
}

impl Scopes {
    pub fn restore(
        &mut self,
        mut scope: Scope,
        map: &aside_vm::snapshot::SnapshotMap,
        heap: &Heap,
    ) -> Result<(), AdapterError> {
        scope.task = map.task(scope.task)?;
        scope.root = map.root(scope.root)?;
        results::reserve_queue(heap, &mut self.live, &mut self.charge)?;
        self.live.push_back(scope);
        Ok(())
    }
}

impl SessionVm {
    pub(super) fn current_scope(&self, task: TaskId) -> Result<Option<Value>, AdapterError> {
        self.scopes
            .live
            .iter()
            .rev()
            .find(|s| s.task == task)
            .map(|s| {
                self.vm
                    .heap()
                    .root_value(s.root)
                    .map_err(AdapterError::from)
            })
            .transpose()
    }

    pub(super) fn scopes_pending(&self) -> bool {
        self.scopes.live.iter().any(|scope| {
            let heap = self.vm.heap();
            let object = heap.root_value(scope.root).expect("live scope root");
            let owner = heap.host_payload(object, 1).expect("scope owner");
            !owner_live(heap, owner)
        })
    }

    fn close_scope(&mut self, index: usize) -> Result<(), AdapterError> {
        let root = self.scopes.live[index].root;
        let heap = self.vm.heap_mut();
        let value = heap.root_value(root)?;
        let overrides = heap.host_payload(value, 3)?;
        if overrides.tag() != ValueTag::Null {
            while heap.sequence_len(overrides)? != 0 {
                let token = heap.sequence_get(overrides, heap.sequence_len(overrides)? - 1)?;
                self.stage.remove_layer(heap, token)?;
                heap.array_pop(overrides)?;
            }
        }
        let nodes = heap.host_payload(value, 2)?;
        if nodes.tag() != ValueTag::Null {
            while heap.sequence_len(nodes)? != 0 {
                let node = heap.sequence_get(nodes, heap.sequence_len(nodes)? - 1)?;
                self.stage.remove_node(heap, node)?;
                heap.array_pop(nodes)?;
            }
        }
        heap.retire_host(value)?;
        heap.release_root(root)?;
        self.scopes.live.remove(index);
        self.results.changed();
        self.particles.owners_changed();
        self.activities.after_script(self.vm.heap())?;
        Ok(())
    }

    pub(super) fn cancel_task_scopes(&mut self, task: TaskId) -> Result<(), AdapterError> {
        for index in (0..self.scopes.live.len()).rev() {
            if self.scopes.live[index].task == task {
                self.close_scope(index)?;
            }
        }
        Ok(())
    }

    pub(super) fn cancel_inner_scopes(
        &mut self,
        task: TaskId,
        frame: FrameId,
    ) -> Result<(), AdapterError> {
        for index in (0..self.scopes.live.len()).rev() {
            let scope = &self.scopes.live[index];
            if scope.task == task && scope.frame.0 > frame.0 {
                self.close_scope(index)?;
            }
        }
        Ok(())
    }

    /// Scope and transition revocation use the same native cancellation barrier.
    pub(super) fn cancel_native_region(
        &mut self,
        session: &mut Session,
        task: TaskId,
        frame: FrameId,
    ) -> Result<(), AdapterError> {
        self.vm.cancel_host_callback(task, frame)?;
        self.cancel_task_transitions(task, Some(frame))?;
        self.activities.cancel_native(self.vm.heap_mut(), task)?;
        if self.issued.is_some_and(|ticket| ticket.task == task) {
            self.issued = None;
            self.saves.capture = None;
            if let Some(reader) = self.choice_reader.take() {
                reader.release(self.vm.heap_mut())?;
            }
            if let Some(reader) = self.dialogue_reader.take() {
                reader.release(self.vm.heap_mut())?;
            }
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.task == task)
        {
            session.cancel_interaction(crate::CancellationReason::OwnerRevoked);
            session.take_completion();
            self.claimed = None;
            let pending = self.pending.take().unwrap();
            self.release_keys(pending.keys)?;
            self.main_wait = None;
        }
        Ok(())
    }

    pub(super) fn maintain_scopes(&mut self, session: &mut Session) -> Result<(), AdapterError> {
        let mut index = 0;
        while index < self.scopes.live.len() {
            let scope = &self.scopes.live[index];
            let object = self.vm.heap().root_value(scope.root)?;
            let owner = self.vm.heap().host_payload(object, 1)?;
            if owner_live(self.vm.heap(), owner) {
                index += 1;
                continue;
            }
            let task = scope.task;
            self.cancel_native_region(session, task, scope.frame)?;
            self.close_scope(index)?;
        }
        Ok(())
    }

    pub(super) fn dispatch_scope(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        if ticket.input == NativeInput::Entry {
            if ticket.context.no_wait || ticket.context.init || ticket.context.derived {
                return Err(VmError::Effect.into());
            }
            let args = self.vm.host_arguments(ticket)?;
            let body = args[0];
            let takes_scope = self.vm.callable_has_parameters(body)?;
            let owner = if args[1].tag() == ValueTag::Null {
                self.default_activity_owner(ticket.task)?
            } else {
                args[1]
            };
            validate_owner(self.vm.heap(), &self.stage, owner)?;
            if self.scopes.methods.is_none() {
                let package = self.vm.program().package();
                let find = |symbol: &str| {
                    package
                        .functions
                        .iter()
                        .enumerate()
                        .find_map(|(i, f)| {
                            if let aside_vm::bytecode::FunctionKind::Native(n) = f.kind
                                && package.symbols[package.natives[n.index()].symbol.index()]
                                    == symbol
                            {
                                Some(FunctionId(i as u32))
                            } else {
                                None
                            }
                        })
                        .ok_or(AdapterError::Input("scope adapter missing"))
                };
                self.scopes.methods = Some([
                    find("reel::scope::run")?,
                    find("reel::scope::spawn")?,
                    find("reel::scope::add")?,
                    find("reel::scope::override")?,
                    find("reel::scope::offset")?,
                    find("reel::override::remove")?,
                ]);
            }
            results::reserve_queue(
                self.vm.heap(),
                &mut self.scopes.live,
                &mut self.scopes.charge,
            )?;
            let heap = self.vm.heap_mut();
            let fields = heap.alloc_table()?;
            let object = heap.alloc_host(
                fields,
                &[Value::int(TAG), owner, Value::NULL, Value::NULL],
                Some(results::readonly),
            )?;
            for (i, (name, function)) in ["run", "spawn", "add", "override", "offset"]
                .into_iter()
                .zip(self.scopes.methods.unwrap())
                .enumerate()
            {
                let method = self.vm.bind_native(object, function)?;
                let heap = self.vm.heap_mut();
                let key = heap.alloc_string(name)?;
                heap.table_store(fields, i as u32, key, method)?;
            }
            let root = self
                .vm
                .heap_mut()
                .add_root(self.root_owner, RootKind::Host, object)?;
            let result = self.vm.respond(
                ticket,
                HostResponse::CallScript {
                    closure: body,
                    arguments: if takes_scope {
                        std::slice::from_ref(&object)
                    } else {
                        &[]
                    },
                    continuation: Continuation {
                        version: 4,
                        stage: 0,
                        values: &[object],
                        faults: FaultRetention::Preserve,
                    },
                },
            );
            if let Err(error) = result {
                self.vm.heap_mut().release_root(root)?;
                return Err(error.into());
            }
            self.scopes.live.push_back(Scope {
                task: ticket.task,
                frame: ticket.frame,
                root,
            });
        } else {
            let response = match ticket.input {
                NativeInput::CallbackReturned => {
                    HostResponse::Ready(self.vm.host_callback_value(ticket)?)
                }
                NativeInput::CallbackFault => {
                    HostResponse::ForwardFault(self.vm.host_callback_fault(ticket)?.unwrap().id)
                }
                NativeInput::CallbackCancelled => HostResponse::Error(aside_vm::NewFault {
                    value: self.vm.heap_mut().alloc_string("ScopeCancelled")?,
                    code: aside_vm::FaultCode::core(FaultKind::Cancelled),
                    cause: None,
                }),
                _ => return Err(AdapterError::Input("unexpected scope continuation")),
            };
            if let Some(index) = self
                .scopes
                .live
                .iter()
                .position(|s| s.task == ticket.task && s.frame == ticket.frame)
            {
                self.close_scope(index)?;
            }
            self.vm.respond(ticket, response)?;
        }
        self.issued = None;
        Ok(())
    }

    pub(super) fn dispatch_scope_override(
        &mut self,
        ticket: HostTicket,
    ) -> Result<(), AdapterError> {
        let args = self.vm.host_arguments(ticket)?;
        let [scope, node, properties] = [args[0], args[1], args[2]];
        self.vm.heap().host_fields(scope)?;
        if !is_scope(self.vm.heap(), scope) {
            return Err(AdapterError::Input("override requires a scope"));
        }
        let token = self.stage.create_override(
            &mut self.vm,
            [scope, node, properties],
            self.scopes.methods.unwrap()[5],
            self.read_limit,
        )?;
        self.vm.respond(ticket, HostResponse::Ready(token))?;
        self.issued = None;
        Ok(())
    }

    pub(super) fn dispatch_scope_offset(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        let args = self.vm.host_arguments(ticket)?;
        let arguments = [args[0], args[1], args[2], args[3]];
        self.vm.heap().host_fields(arguments[0])?;
        if !is_scope(self.vm.heap(), arguments[0]) {
            return Err(AdapterError::Input("offset requires a scope"));
        }
        let token =
            self.stage
                .create_offset(&mut self.vm, arguments, self.scopes.methods.unwrap()[5])?;
        self.vm.respond(ticket, HostResponse::Ready(token))?;
        self.issued = None;
        Ok(())
    }

    pub(super) fn dispatch_override_remove(
        &mut self,
        ticket: HostTicket,
    ) -> Result<(), AdapterError> {
        let token = self.vm.host_arguments(ticket)?[0];
        self.stage.remove_layer(self.vm.heap_mut(), token)?;
        self.activities.after_script(self.vm.heap())?;
        self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?;
        self.issued = None;
        Ok(())
    }

    pub(super) fn dispatch_scope_add(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        let args = self.vm.host_arguments(ticket)?;
        let [scope, parent, node, index] = [args[0], args[1], args[2], args[3]];
        let index = if index.tag() == ValueTag::Null {
            None
        } else {
            Some(
                index
                    .as_int()
                    .ok_or(AdapterError::Input("child index must be an integer"))?,
            )
        };
        let heap = self.vm.heap_mut();
        if heap.host_payload(scope, 0)?.as_int() != Some(TAG) {
            return Err(AdapterError::Input("scope.add requires a scope"));
        }
        let mut nodes = heap.host_payload(scope, 2)?;
        if nodes.tag() == ValueTag::Null {
            nodes = heap.alloc_array(&[])?;
            heap.set_host_payload(scope, 2, nodes)?;
        }
        // Charge and trace registration before mounting; a failed mount keeps
        // the caller's node and the scope's ownership unchanged.
        heap.array_push(nodes, node)?;
        if let Err(error) = self.stage.add_node(heap, parent, node, index) {
            heap.array_pop(nodes)?;
            return Err(error);
        }
        self.vm.respond(ticket, HostResponse::Ready(node))?;
        self.issued = None;
        Ok(())
    }
}
