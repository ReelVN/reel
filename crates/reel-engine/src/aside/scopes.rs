//! Native callback lifetimes share the ordinary host-owner and task protocols.
//! Nominal methods are shared; each handle retains only its lifetime state.
use super::*;

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{Member, NamedType, named, nullable};
    let scope = named("reel::types::Scope");
    let node = named("reel::types::Node");
    api.refine(
        "reel::scope",
        TypeHint::Any,
        &[
            ("body", TypeHint::Closure),
            ("owner", nullable(node.clone())),
        ],
    );
    api.refine(
        "reel::reading",
        TypeHint::Any,
        &[
            ("body", TypeHint::Closure),
            ("owner", nullable(node.clone())),
        ],
    );
    for (method, result, parameters) in [
        (
            "run",
            named("reel::types::Activity"),
            vec![
                ("action", named("reel::types::Action")),
                ("skip", super::activity::skip_type()),
            ],
        ),
        (
            "spawn",
            named("reel::types::Task"),
            vec![("work", TypeHint::Closure)],
        ),
        (
            "add",
            node.clone(),
            vec![("parent", node.clone()), ("node", node.clone())],
        ),
        (
            "override",
            named("reel::types::Override"),
            vec![("node", node.clone()), ("properties", TypeHint::Table)],
        ),
        (
            "offset",
            named("reel::types::Override"),
            vec![("node", node)],
        ),
        ("cancel", TypeHint::Bool, Vec::new()),
        ("wait", TypeHint::Null, Vec::new()),
        ("finish", TypeHint::Bool, vec![("value", TypeHint::Any)]),
    ] {
        let mut parameters = parameters;
        parameters.push(("scope", scope.clone()));
        api.refine(&format!("reel::scope::{method}"), result, &parameters);
    }
    api.native_mut("reel::scope::add").returns_parameter("node");
    api.refine(
        "reel::override::remove",
        TypeHint::Null,
        &[("token", named("reel::types::Override"))],
    );
    api.types.push(NamedType::new(
        "reel::types::Scope",
        "A callback lifetime that owns tasks, activities, nodes and property overrides.",
        [
            "run", "spawn", "add", "override", "offset", "cancel", "finish", "wait",
        ]
        .into_iter()
        .map(|method| {
            Member::method(
                api.native(&format!("reel::scope::{method}"))
                    .clone()
                    .bound(method),
            )
        })
        .collect(),
    ));
    api.types.push(NamedType::new(
        "reel::types::Override",
        "A temporary property layer owned by a scope.",
        vec![Member::method(
            api.native("reel::override::remove").clone().bound("remove"),
        )],
    ));
}

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Run `body(scope)` in the current task and return its result. Work created
        /// inside inherits the scope. Leaving the body retires its unfinished activities,
        /// tasks, owned nodes and overrides. `owner` optionally anchors it to a stage node.
        "reel::scope";
        /// Run a story page in its own reading context using an ordinary Scope.
        /// The body may accept that Scope and finish it with an event result.
        /// Dialogue, choices, history, auto, skip, pause and auto conditions belong
        /// to the context. Exiting releases them and restores the parent's original
        /// objects. Delay, settings and read progress remain shared. Ordinary scopes
        /// inherit the current context. Only the main story task may enter reading.
        ///
        /// ```aside
        /// fn main() {
        ///     let parent = game.reading();
        ///     let destination = reading(body: |page| {
        ///         game.reading().set_auto(condition: false).pause(false);
        ///         narrator("A page with its own dialogue and history.");
        ///         page.finish("title");
        ///         await completion();
        ///     });
        ///     if destination != "title" || game.reading() != parent {
        ///         throw "reading page did not restore its parent";
        ///     }
        /// }
        /// ```
        "reel::reading";
        /// Request scope retirement. The first request returns true; repeated requests
        /// and calls through an extracted bound method after retirement return false.
        /// The scope handle retires on exit. Cancellation revokes its native region,
        /// nested scopes and owned work, then raises ScopeCancelled outside scope.
        /// The body can cancel itself; its current native response completes first.
        ///
        /// ```aside
        /// fn main() {
        ///     let mut cancel = null;
        ///     try {
        ///         scope(body: |s| { cancel = s.cancel; cancel(); await completion(); });
        ///     } catch error {
        ///         if error != "ScopeCancelled" { throw; }
        ///     }
        ///     if cancel() { throw "scope remained live"; }
        /// }
        /// ```
        "reel::scope::cancel";
        /// End this scope and return `value` from its original call. The first finish
        /// or cancel request returns true; later requests return false. This call
        /// responds before the body, nested scopes and owned work are retired.
        /// Remaining body statements and finally blocks are skipped. Await saves
        /// and exit animations before finishing, or perform them after scope returns.
        /// Extracted bound methods return false after retirement.
        ///
        /// ```aside
        /// fn main() {
        ///     let result = scope(body: |s| {
        ///         s.finish("continue");
        ///         await completion();
        ///     });
        ///     if result != "continue" { throw "scope result"; }
        /// }
        /// ```
        "reel::scope::finish";
        /// Park the current scope body until a callback finishes or cancels it.
        /// The original scope call receives the finish result.
        "reel::scope::wait";
        /// Start an action owned by this scope, optionally paused. With `skip: "finish"`,
        /// fast-forward can finish a running, finite activity in the current reading context.
        "reel::scope::run";
        /// Start a zero-argument task owned by this scope. Its result is awaitable.
        "reel::scope::spawn";
        /// Insert a node under parent at the optional child index and own its lifetime
        /// through this scope. Scope retirement removes the node.
        "reel::scope::add";
        /// Temporarily override named properties on a node. Return a removable override
        /// token; scope retirement restores the preceding effective values and bindings.
        "reel::scope::override";
        /// Remove this override token and restore the preceding effective properties.
        "reel::override::remove";
        /// Add a temporary x/y offset to a node while retaining its base position.
        /// Return an override token controlled by the scope.
        "reel::scope::offset";
    }
}
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
    pub(super) exit: Option<ScopeExit>,
    root: RootToken,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(super) enum ScopeExit {
    Cancel,
    Finish,
}

#[derive(Default)]
pub(super) struct Scopes {
    pub live: VecDeque<Scope>,
    charge: Option<HeapReservation>,
    checked_retirement: Option<u64>,
    #[cfg(test)]
    pub lifetime_checks: u64,
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
    pub(super) fn dispatch_scope_wait(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        let object = self.vm.host_arguments(ticket)?[0];
        if !self
            .current_scope(ticket.task)?
            .is_some_and(|scope| scope.same_bits(object))
        {
            return Err(AdapterError::Input("wait requires the current scope body"));
        }
        self.vm
            .respond(ticket, HostResponse::Pending(ticket.wait_id()))?;
        self.issued = None;
        Ok(())
    }

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
        !self.scopes.live.is_empty()
            && self.scopes.checked_retirement != Some(self.vm.heap().host_retirement_revision())
    }

    fn close_scope(&mut self, session: &mut Session, index: usize) -> Result<(), AdapterError> {
        self.end_reading(session, self.scopes.live[index].frame)?;
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
        self.results.owners_changed();
        self.particles.owners_changed();
        self.activities.after_script();
        Ok(())
    }

    pub(super) fn cancel_task_scopes(
        &mut self,
        session: &mut Session,
        task: TaskId,
    ) -> Result<(), AdapterError> {
        self.retire_flow_region(task, None)?;
        for index in (0..self.scopes.live.len()).rev() {
            if self.scopes.live[index].task == task {
                self.close_scope(session, index)?;
            }
        }
        Ok(())
    }

    pub(super) fn cancel_inner_scopes(
        &mut self,
        session: &mut Session,
        task: TaskId,
        frame: FrameId,
    ) -> Result<(), AdapterError> {
        self.retire_flow_region(task, Some(frame))?;
        for index in (0..self.scopes.live.len()).rev() {
            let scope = &self.scopes.live[index];
            if scope.task == task && scope.frame.0 > frame.0 {
                self.close_scope(session, index)?;
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
        self.end_native_region(session, task, frame, None)
    }

    fn end_native_region(
        &mut self,
        session: &mut Session,
        task: TaskId,
        frame: FrameId,
        result: Option<Value>,
    ) -> Result<(), AdapterError> {
        if let Some(value) = result {
            self.vm.finish_host_callback(task, frame, value)?;
        } else {
            self.vm.cancel_host_callback(task, frame)?;
        }
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
        self.cancel_inner_scopes(session, task, frame)?;
        Ok(())
    }

    pub(super) fn maintain_scopes(&mut self, session: &mut Session) -> Result<(), AdapterError> {
        if !self.scopes_pending() {
            return Ok(());
        }
        let retirement = self.vm.heap().host_retirement_revision();
        let mut index = 0;
        while index < self.scopes.live.len() {
            #[cfg(test)]
            {
                self.scopes.lifetime_checks += 1;
            }
            let scope = &self.scopes.live[index];
            let object = self.vm.heap().root_value(scope.root)?;
            let owner = self.vm.heap().host_payload(object, 1)?;
            if scope.exit.is_none() && owner_live(self.vm.heap(), owner) {
                index += 1;
                continue;
            }
            let task = scope.task;
            let result = if matches!(scope.exit, Some(ScopeExit::Finish)) {
                Some(self.vm.heap().host_payload(object, 4)?)
            } else {
                None
            };
            self.end_native_region(session, task, scope.frame, result)?;
            self.close_scope(session, index)?;
        }
        self.scopes.checked_retirement = Some(retirement);
        Ok(())
    }

    pub(super) fn dispatch_scope_exit(
        &mut self,
        ticket: HostTicket,
        exit: ScopeExit,
    ) -> Result<(), AdapterError> {
        let object = self.vm.host_arguments(ticket)?[0];
        let value = if matches!(exit, ScopeExit::Finish) {
            self.vm.host_arguments(ticket)?[1]
        } else {
            Value::NULL
        };
        match self.vm.heap().host_fields(object) {
            Ok(_) if !is_scope(self.vm.heap(), object) => {
                return Err(AdapterError::Input("scope exit requires a scope"));
            }
            Ok(_) | Err(aside_vm::HeapError::RetiredHost) => {}
            Err(error) => return Err(error.into()),
        }
        let mut requested = false;
        for scope in &mut self.scopes.live {
            if self.vm.heap().root_value(scope.root)?.same_bits(object) {
                requested = scope.exit.is_none();
                if requested {
                    self.vm.heap_mut().set_host_payload(object, 4, value)?;
                    scope.exit = Some(exit);
                    self.scopes.checked_retirement = None;
                }
                break;
            }
        }
        // Complete this call before maintenance can revoke its own callback region.
        self.vm
            .respond(ticket, HostResponse::Ready(Value::bool(requested)))?;
        self.issued = None;
        Ok(())
    }

    pub(super) fn dispatch_scope(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        reading: bool,
    ) -> Result<(), AdapterError> {
        if ticket.input == NativeInput::Entry {
            if ticket.context.no_wait || ticket.context.init || ticket.context.derived {
                return Err(VmError::Effect.into());
            }
            let args = self.vm.host_arguments(ticket)?;
            let body = args[0];
            let takes_scope = self.vm.callable_has_parameters(body)?;
            // CallScript stores this one scope value in the native continuation.
            // Validate its only configurable response limit before preparation.
            if self.vm.limits().max_continuation_values == 0
                || (takes_scope && self.vm.limits().max_arguments == 0)
            {
                return Err(VmError::Limit.into());
            }
            let owner = if args[1].tag() == ValueTag::Null {
                self.default_activity_owner(ticket.task)?
            } else {
                args[1]
            };
            validate_owner(self.vm.heap(), &self.stage, owner)?;
            results::reserve_queue(
                self.vm.heap(),
                &mut self.scopes.live,
                &mut self.scopes.charge,
            )?;
            let heap = self.vm.heap_mut();
            let fields = heap.alloc_table()?;
            let object = heap.alloc_host(
                fields,
                &[
                    Value::int(TAG),
                    owner,
                    Value::NULL,
                    Value::NULL,
                    Value::NULL,
                ],
                Some(&results::READONLY),
            )?;
            self.stage.types.mark(heap, object, "reel::types::Scope")?;
            let root = self
                .vm
                .heap_mut()
                .add_root(self.root_owner, RootKind::Host, object)?;
            if reading && let Err(error) = self.begin_reading(session, ticket) {
                self.vm.heap_mut().release_root(root)?;
                return Err(error);
            }
            // The ticket, callable and native slots remain the ones just checked.
            // CallScript now writes only existing roots; it allocates no storage
            // and cannot reject an author-controlled limit after Reading commits.
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
                        version: 1,
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
                exit: None,
                root,
            });
            self.scopes.checked_retirement = None;
        } else {
            let response = match ticket.input {
                NativeInput::CallbackReturned => {
                    HostResponse::Ready(self.vm.host_resume_value(ticket)?)
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
                self.close_scope(session, index)?;
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
            self.vm.heap_mut(),
            [scope, node, properties],
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
        let token = self.stage.create_offset(self.vm.heap_mut(), arguments)?;
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
        self.activities.after_script();
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
