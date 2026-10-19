//! One-shot component results. Pending native frames own all subscription values.
use super::{AdapterError, FaultKind, HostTicket, Session, SessionVm};
use aside_vm::{
    Heap, HeapError, HeapReservation, RootKind, RootToken, Value, ValueTag,
    bytecode::{FunctionId, FunctionKind, NativeId},
    executor::{Completion, HostResponse, Vm, WaitId},
};
use std::collections::VecDeque;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub(super) enum Kind {
    Completion = -1101,
    Task = -1102,
    Presentation = -1103,
    Particle = -1104,
    Operation = -1105,
    Capture = -1106,
    Window = -1107,
}
#[repr(u32)]
enum Slot {
    Tag,
    State,
    Owner,
    Value,
    Target,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Pending,
    Resolved,
    Rejected,
    Cancelled,
    Cancelling,
    Faulted,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Methods {
    settle: [FunctionId; 3],
    cancel_task: FunctionId,
    wait: NativeId,
}

pub(super) fn reserve_queue<T>(
    heap: &Heap,
    queue: &mut VecDeque<T>,
    charge: &mut Option<HeapReservation>,
) -> Result<(), AdapterError> {
    if queue.len() < queue.capacity() {
        return Ok(());
    }
    let capacity = queue
        .len()
        .max(4)
        .checked_mul(2)
        .ok_or(AdapterError::Allocation)?;
    let bytes = capacity
        .checked_mul(size_of::<T>())
        .ok_or(AdapterError::Allocation)? as u64;
    // Account for old and replacement storage until the move is complete.
    let mut next_charge = heap.budget_usage().reserve(bytes)?;
    let mut next = VecDeque::new();
    next.try_reserve_exact(capacity)
        .map_err(|_| AdapterError::Allocation)?;
    let actual = (next.capacity() * size_of::<T>()) as u64;
    if actual != bytes {
        next_charge = heap.budget_usage().reserve(actual)?;
    }
    next.append(queue);
    *queue = next;
    *charge = Some(next_charge);
    Ok(())
}
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Results {
    methods: Option<Methods>,
    #[serde(skip)]
    waits: VecDeque<(WaitId, Value)>,
    #[serde(skip)]
    charge: Option<HeapReservation>,
    remaining: usize,
    dirty: bool,
    stage_revision: u64,
    requested: u64,
    presented: u64,
    #[serde(skip)]
    presentations: VecDeque<RootToken>,
    #[serde(skip)]
    presentation_charge: Option<HeapReservation>,
    presentation_remaining: usize,
}
impl Results {
    pub(super) fn snapshot_records(&self) -> super::snapshot::RecordStream<'_> {
        use super::snapshot::{Part, Record};
        Box::new(
            std::iter::once_with(|| Record::new(Part::Results, self))
                .chain(
                    self.waits
                        .iter()
                        .map(|wait| Record::new(Part::ResultWait, wait)),
                )
                .chain(
                    self.presentations
                        .iter()
                        .map(|root| Record::new(Part::Presentation, root)),
                ),
        )
    }
    pub(super) fn restore_record(
        &mut self,
        record: &super::snapshot::Record,
        map: &aside_vm::snapshot::SnapshotMap,
        heap: &Heap,
    ) -> Result<(), AdapterError> {
        use super::snapshot::Part;
        match record.part {
            Part::Results => {
                *self = record.read()?;
                self.dirty = true;
            }
            Part::ResultWait => {
                let (wait, value): (WaitId, Value) = record.read()?;
                reserve_queue(heap, &mut self.waits, &mut self.charge)?;
                self.waits.push_back((map.wait(wait)?, map.value(value)?));
            }
            Part::Presentation => {
                let root = map.root(record.read()?)?;
                reserve_queue(heap, &mut self.presentations, &mut self.presentation_charge)?;
                self.presentations.push_back(root);
            }
            _ => unreachable!(),
        }
        Ok(())
    }
}
pub(super) fn readonly(_: &Heap, _: Value, _: u32, _: Value) -> Result<(), HeapError> {
    Err(HeapError::ReadOnly)
}
fn settle(heap: &mut Heap, object: Value, state: State, value: Value) -> Result<(), AdapterError> {
    heap.set_host_payload(object, Slot::Value as u32, value)?;
    heap.set_host_payload(object, Slot::State as u32, Value::int(state as i64))?;
    heap.set_host_payload(object, Slot::Owner as u32, Value::NULL)?;
    if matches!(kind(heap, object)?, Kind::Particle | Kind::Presentation) {
        heap.set_host_payload(object, Slot::Target as u32, Value::NULL)?;
    }
    Ok(())
}
pub(super) fn kind(heap: &Heap, object: Value) -> Result<Kind, AdapterError> {
    match heap.host_payload(object, Slot::Tag as u32)?.as_int() {
        Some(-1101) => Ok(Kind::Completion),
        Some(-1102) => Ok(Kind::Task),
        Some(-1103) => Ok(Kind::Presentation),
        Some(-1104) => Ok(Kind::Particle),
        Some(-1105) => Ok(Kind::Operation),
        Some(-1106) => Ok(Kind::Capture),
        Some(-1107) => Ok(Kind::Window),
        _ => Err(AdapterError::Input("expected completion or task")),
    }
}
fn state(heap: &mut Heap, object: Value) -> Result<State, AdapterError> {
    let kind = kind(heap, object)?;
    let state = match heap.host_payload(object, Slot::State as u32)?.as_int() {
        Some(0) => State::Pending,
        Some(1) => State::Resolved,
        Some(2) => State::Rejected,
        Some(3) => State::Cancelled,
        Some(4) => State::Cancelling,
        Some(5) => State::Faulted,
        _ => unreachable!("private result state"),
    };
    if state == State::Pending && kind != Kind::Task {
        let owner = heap.host_payload(object, Slot::Owner as u32)?;
        if owner.tag() != ValueTag::Null {
            match heap.host_fields(owner) {
                Err(HeapError::RetiredHost) => {
                    settle(heap, object, State::Cancelled, Value::NULL)?;
                    return Ok(State::Cancelled);
                }
                result => {
                    result?;
                }
            }
        }
    }
    Ok(state)
}
pub(super) fn pending(heap: &mut Heap, object: Value) -> Result<bool, AdapterError> {
    Ok(state(heap, object)? == State::Pending)
}
fn response(heap: &mut Heap, object: Value) -> Result<Option<Completion>, AdapterError> {
    let state = state(heap, object)?;
    let value = heap.host_payload(object, Slot::Value as u32)?;
    Ok(match state {
        State::Pending | State::Cancelling => None,
        State::Faulted => {
            let fault = heap.fault_id(value)?;
            Some(Completion::Error(aside_vm::NewFault {
                value: heap.fault_value(fault)?,
                code: heap.fault_info(fault)?.code,
                cause: Some(fault),
            }))
        }
        State::Resolved => Some(Completion::Ready(value)),
        State::Rejected | State::Cancelled => Some(Completion::Error(aside_vm::NewFault {
            value,
            code: aside_vm::FaultCode::core(if state == State::Rejected {
                FaultKind::Thrown
            } else {
                FaultKind::Cancelled
            }),
            cause: None,
        })),
    })
}
impl Results {
    /// One producer root per result, including while its presentation is disarmed.
    pub(super) fn create_presentation(
        &mut self,
        vm: &mut Vm,
        root_owner: aside_vm::RootOwner,
        owner: Value,
    ) -> Result<Value, AdapterError> {
        let result = self.create(vm, owner, Kind::Presentation)?;
        reserve_queue(
            vm.heap(),
            &mut self.presentations,
            &mut self.presentation_charge,
        )?;
        let root = vm.heap_mut().add_root(root_owner, RootKind::Host, result)?;
        self.presentations.push_back(root);
        self.changed();
        Ok(result)
    }
    pub(super) fn arm_presentation(
        &mut self,
        vm: &mut Vm,
        result: Value,
        value: Value,
    ) -> Result<u64, AdapterError> {
        let sequence = self
            .requested
            .checked_add(1)
            .ok_or(AdapterError::Input("presentation sequence exhausted"))?;
        let barrier = vm
            .heap_mut()
            .alloc_array(&[Value::int(sequence as i64), value])?;
        vm.heap_mut()
            .set_host_payload(result, Slot::Target as u32, barrier)?;
        self.requested = sequence;
        self.changed();
        Ok(sequence)
    }
    pub(super) fn disarm_presentation(
        &mut self,
        vm: &mut Vm,
        result: Value,
    ) -> Result<(), AdapterError> {
        vm.heap_mut()
            .set_host_payload(result, Slot::Target as u32, Value::NULL)?;
        self.changed();
        Ok(())
    }
    pub(super) fn create_window(
        &mut self,
        vm: &mut Vm,
        owner: Value,
    ) -> Result<Value, AdapterError> {
        self.create(vm, owner, Kind::Window)
    }
    pub(super) fn create_capture(
        &mut self,
        vm: &mut Vm,
        owner: Value,
    ) -> Result<Value, AdapterError> {
        self.create(vm, owner, Kind::Capture)
    }
    fn create(&mut self, vm: &mut Vm, owner: Value, kind: Kind) -> Result<Value, AdapterError> {
        if self.methods.is_none() {
            let package = vm.program().package();
            let find = |name: &str| {
                package
                    .functions
                    .iter()
                    .enumerate()
                    .find_map(|(index, function)| {
                        if let FunctionKind::Native(native) = function.kind
                            && package.symbols[package.natives[native.index()].symbol.index()]
                                == name
                        {
                            Some((FunctionId(index as u32), native))
                        } else {
                            None
                        }
                    })
                    .ok_or(AdapterError::Input("completion adapter missing"))
            };
            self.methods = Some(Methods {
                settle: [
                    find("reel::completion::resolve")?.0,
                    find("reel::completion::reject")?.0,
                    find("reel::completion::cancel")?.0,
                ],
                wait: find("reel::completion::await")?.1,
                cancel_task: find("reel::task::cancel")?.0,
            });
        }
        let methods = self.methods.as_ref().unwrap();
        let heap = vm.heap_mut();
        let fields = heap.alloc_table()?;
        let slots = [
            Value::int(kind as i64),
            Value::int(State::Pending as i64),
            owner,
            Value::NULL,
            Value::NULL,
        ];
        let object = heap.alloc_host(
            fields,
            &slots[..if matches!(kind, Kind::Particle | Kind::Presentation) {
                5
            } else {
                4
            }],
            if kind == Kind::Operation {
                None
            } else {
                Some(readonly)
            },
        )?;
        for (index, (name, function)) in ["resolve", "reject", "cancel"]
            .into_iter()
            .zip(methods.settle)
            .enumerate()
        {
            if matches!(kind, Kind::Particle | Kind::Operation)
                || (kind != Kind::Completion && name != "cancel")
            {
                continue;
            }
            let function = if kind == Kind::Task {
                methods.cancel_task
            } else {
                function
            };
            let method = vm.bind_native(object, function)?;
            let heap = vm.heap_mut();
            let key = heap.alloc_string(name)?;
            heap.table_store(
                fields,
                if kind != Kind::Completion {
                    0
                } else {
                    index as u32
                },
                key,
                method,
            )?;
        }
        vm.bind_awaitable(object, methods.wait)?;
        Ok(object)
    }
    pub(super) fn create_operation(&mut self, vm: &mut Vm) -> Result<Value, AdapterError> {
        self.create(vm, Value::NULL, Kind::Operation)
    }
    pub(super) fn create_particle(
        &mut self,
        vm: &mut Vm,
        owner: Value,
        target: Value,
    ) -> Result<Value, AdapterError> {
        let result = self.create(vm, owner, Kind::Particle)?;
        vm.heap_mut()
            .set_host_payload(result, Slot::Target as u32, target)?;
        Ok(result)
    }
    fn subscribe(
        &mut self,
        vm: &mut Vm,
        ticket: HostTicket,
        object: Value,
    ) -> Result<(), AdapterError> {
        if let Some(result) = response(vm.heap_mut(), object)? {
            let result = match result {
                Completion::Ready(value) => HostResponse::Ready(value),
                Completion::Error(error) => HostResponse::Error(error),
                _ => unreachable!(),
            };
            vm.respond(ticket, result)?;
        } else {
            reserve_queue(vm.heap(), &mut self.waits, &mut self.charge)?;
            let wait = ticket.wait_id();
            vm.respond(ticket, HostResponse::Pending(wait))?;
            self.waits.push_back((wait, object));
        }
        Ok(())
    }
    pub fn changed(&mut self) {
        self.dirty = true;
    }
    pub fn needs_work(&self, stage_revision: u64) -> bool {
        (!self.waits.is_empty() || !self.presentations.is_empty())
            && (self.dirty
                || self.remaining != 0
                || self.presentation_remaining != 0
                || self.stage_revision != stage_revision)
    }
    /// Finish a registration-order round before running more scripts. Values in
    /// cancelled native frames may already be collected: check WaitId first.
    pub fn maintain(
        &mut self,
        vm: &mut Vm,
        stage_revision: u64,
        mut current: impl FnMut(&mut Heap, Value, Value) -> Result<bool, AdapterError>,
    ) -> Result<bool, AdapterError> {
        if !self.needs_work(stage_revision) {
            return Ok(false);
        }
        if self.remaining == 0 && self.presentation_remaining == 0 {
            self.remaining = self.waits.len();
            self.presentation_remaining = self.presentations.len();
            self.dirty = false;
            self.stage_revision = stage_revision;
        }
        // Producers settle before consumers or scripts run, including requests
        // whose callers have not awaited them yet. Keep only their result rooted.
        for _ in 0..self.presentation_remaining.min(64) {
            let root = *self.presentations.front().unwrap();
            let object = vm.heap().root_value(root)?;
            let heap = vm.heap_mut();
            let pending = state(heap, object)? == State::Pending;
            let barrier = heap.host_payload(object, Slot::Target as u32)?;
            let mut ready = pending
                && barrier.tag() != ValueTag::Null
                && heap.sequence_get(barrier, 0)?.as_int().unwrap() as u64 <= self.presented;
            if ready && !current(heap, object, heap.host_payload(object, Slot::Owner as u32)?)? {
                heap.set_host_payload(object, Slot::Target as u32, Value::NULL)?;
                ready = false;
            }
            let keep = pending && !ready;
            if ready {
                let value = heap.sequence_get(barrier, 1)?;
                settle(heap, object, State::Resolved, value)?;
            }
            self.presentations.pop_front();
            if keep {
                self.presentations.push_back(root);
            } else {
                heap.release_root(root)?;
            }
            self.presentation_remaining -= 1;
        }
        if self.presentation_remaining != 0 {
            return Ok(true);
        }
        if self.presentations.is_empty() {
            self.presentations = VecDeque::new();
            self.presentation_charge = None;
        }
        for _ in 0..self.remaining.min(64) {
            let &(wait, object) = self.waits.front().unwrap();
            let keep = if vm.wait_is_pending(wait) {
                if let Some(result) = response(vm.heap_mut(), object)? {
                    vm.complete(wait, result)?;
                    false
                } else {
                    true
                }
            } else {
                false
            };
            self.waits.pop_front();
            if keep {
                self.waits.push_back((wait, object));
            }
            self.remaining -= 1;
        }
        if self.waits.is_empty() {
            self.waits = VecDeque::new();
            self.charge = None;
        }
        Ok(self.needs_work(stage_revision))
    }
    pub fn clear(&mut self) {
        // The session's bounded owner scan retires outstanding producer roots.
        self.presentations = VecDeque::new();
        self.presentation_charge = None;
        self.presentation_remaining = 0;
        self.waits = VecDeque::new();
        self.charge = None;
        self.remaining = 0;
        self.dirty = false;
    }
}

pub(super) fn cancelling(heap: &mut Heap, object: Value) -> Result<(), AdapterError> {
    heap.set_host_payload(
        object,
        Slot::State as u32,
        Value::int(State::Cancelling as i64),
    )?;
    Ok(())
}

pub(super) enum TaskResult {
    Returned(Value),
    Faulted(Value),
    Rejected(Value),
    Cancelled,
}
pub(super) fn finish_task(
    heap: &mut Heap,
    object: Value,
    result: TaskResult,
) -> Result<(), AdapterError> {
    let (state, value) = match result {
        TaskResult::Returned(value) => (State::Resolved, value),
        TaskResult::Faulted(fault) => (State::Faulted, fault),
        TaskResult::Rejected(value) => (State::Rejected, value),
        TaskResult::Cancelled => (State::Cancelled, Value::NULL),
    };
    settle(heap, object, state, value)
}
impl SessionVm {
    /// Include this request sequence in the next stable scene projection.
    pub fn presentation_request(&self) -> u64 {
        self.results.requested
    }

    /// A host calls this only after accepting the frame carrying this request.
    pub fn accept_presentation(
        &mut self,
        session: &Session,
        request: u64,
    ) -> Result<(), AdapterError> {
        self.same_session(session)?;
        if request > self.results.requested {
            return Err(AdapterError::Input("unknown presentation request"));
        }
        if request > self.results.presented {
            self.results.presented = request;
            self.results.changed();
        }
        Ok(())
    }

    pub(super) fn dispatch_result(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        symbol: &str,
    ) -> Result<(), AdapterError> {
        let arguments = self.vm.host_arguments(ticket)?;
        let object = arguments[0];
        if symbol == "reel::spawn" {
            if ticket.context.no_wait || ticket.context.init || ticket.context.derived {
                return Err(AdapterError::Vm(aside_vm::executor::VmError::Effect));
            }
            if !self
                .vm
                .heap()
                .accepts_builtin(aside_vm::bytecode::BuiltinType::Closure, object)?
            {
                return Err(AdapterError::Input("spawn requires a function"));
            }
            let owner = if arguments[1].tag() == ValueTag::Null {
                self.default_activity_owner(ticket.task)?
            } else {
                arguments[1]
            };
            let node = if owner.tag() == ValueTag::Null {
                None
            } else {
                Some(self.stage.id(self.vm.heap(), owner)?)
            };
            let result = self.results.create(&mut self.vm, owner, Kind::Task)?;
            self.start_owned_task(session, node, object, &[], Some(result))?;
            self.vm.respond(ticket, HostResponse::Ready(result))?;
            self.issued = None;
            return Ok(());
        }
        if symbol == "reel::task::cancel" {
            if kind(self.vm.heap(), object)? != Kind::Task {
                return Err(AdapterError::Input("expected task"));
            }
            let pending = state(self.vm.heap_mut(), object)? == State::Pending;
            self.vm
                .respond(ticket, HostResponse::Ready(Value::bool(pending)))?;
            self.issued = None;
            if pending {
                let index = self
                    .tasks
                    .iter()
                    .position(|task| {
                        task.result.is_some_and(|root| {
                            self.vm
                                .heap()
                                .root_value(root)
                                .is_ok_and(|value| value.same_bits(object))
                        })
                    })
                    .ok_or(AdapterError::Input("pending task missing from scheduler"))?;
                self.cancel_callback(session, index)?;
            }
            return Ok(());
        }
        if symbol == "reel::completion::await" {
            self.results.subscribe(&mut self.vm, ticket, object)?;
        } else {
            let value = if matches!(symbol, "reel::completion" | "reel::stage::presented") {
                let presentation = symbol == "reel::stage::presented";
                if presentation
                    && (ticket.context.no_wait || ticket.context.init || ticket.context.derived)
                {
                    return Err(AdapterError::Vm(aside_vm::executor::VmError::Effect));
                }
                let owner = if object.tag() == ValueTag::Null {
                    self.default_activity_owner(ticket.task)?
                } else {
                    object
                };
                if owner.tag() != ValueTag::Null {
                    self.stage.id(self.vm.heap(), owner)?;
                }
                let kind = if presentation {
                    Kind::Presentation
                } else {
                    Kind::Completion
                };
                if presentation {
                    let result =
                        self.results
                            .create_presentation(&mut self.vm, self.root_owner, owner)?;
                    self.results
                        .arm_presentation(&mut self.vm, result, Value::NULL)?;
                    result
                } else {
                    self.results.create(&mut self.vm, owner, kind)?
                }
            } else {
                let kind = kind(self.vm.heap(), object)?;
                if kind != Kind::Completion
                    && !(matches!(kind, Kind::Presentation | Kind::Capture | Kind::Window)
                        && symbol == "reel::completion::cancel")
                {
                    return Err(AdapterError::Input("expected completion"));
                }
                let value = arguments.get(1).copied().unwrap_or(Value::NULL);
                let pending = state(self.vm.heap_mut(), object)? == State::Pending;
                if pending {
                    let state = match symbol {
                        "reel::completion::resolve" => State::Resolved,
                        "reel::completion::reject" => State::Rejected,
                        "reel::completion::cancel" => State::Cancelled,
                        _ => unreachable!(),
                    };
                    let owner = self.vm.heap().host_payload(object, Slot::Owner as u32)?;
                    settle(self.vm.heap_mut(), object, state, value)?;
                    if state == State::Cancelled && owner.tag() != ValueTag::Null {
                        self.virtual_result_cancelled(owner, object)?;
                    }
                    self.results.changed();
                }
                Value::bool(pending)
            };
            self.vm.respond(ticket, HostResponse::Ready(value))?;
        }
        self.issued = None;
        Ok(())
    }
}
