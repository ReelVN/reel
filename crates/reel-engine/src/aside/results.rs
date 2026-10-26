//! One-shot component results. Pending native frames own all subscription values.
use super::{AdapterError, FaultKind, HostTicket, Session, SessionVm};

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{Member, NamedType, named};
    api.refine("reel::completion", named("reel::types::Completion"), &[]);
    api.refine(
        "reel::spawn",
        named("reel::types::Task"),
        &[("work", TypeHint::Closure)],
    );
    api.refine(
        "reel::stage::presented",
        named("reel::types::Presentation"),
        &[],
    );
    for name in ["resolve", "reject", "cancel"] {
        api.refine(&format!("reel::completion::{name}"), TypeHint::Bool, &[]);
    }
    api.refine(
        "reel::task::cancel",
        TypeHint::Bool,
        &[("task", named("reel::types::Task"))],
    );
    for (name, observable, methods) in [
        ("Completion", true, &["resolve", "reject", "cancel"][..]),
        ("Task", true, &["cancel"][..]),
        ("Presentation", false, &["cancel"][..]),
        ("Capture", false, &["cancel"][..]),
        ("AssetRead", false, &["cancel"][..]),
        ("WindowOperation", false, &["cancel"][..]),
        ("Condition", true, &["cancel"][..]),
        ("ParticleActivity", false, &[][..]),
        ("Operation", false, &[][..]),
    ] {
        let ty = format!("reel::types::{name}");
        let mut members = Vec::new();
        if observable {
            members.push(Member::field(
                &ty,
                "pending",
                TypeHint::Bool,
                None,
                "Whether this result has not reached a terminal outcome.",
                Some(aside_vm::bytecode::Const::Bool(true)),
            ));
        }
        for method in methods {
            let native = if name == "Task" {
                "reel::task::cancel".into()
            } else {
                format!("reel::completion::{method}")
            };
            members.push(Member::method(api.native(&native).clone().bound(method)));
        }
        let mut descriptor = NamedType::new(
            &ty,
            "An awaitable owned operation with a shared terminal result.",
            members,
        );
        descriptor.await_result = Some(match name {
            "Capture" | "AssetRead" => named("Bytes"),
            "WindowOperation" => TypeHint::Union(vec![TypeHint::Table, TypeHint::Bool]),
            "Condition" => TypeHint::Bool,
            "ParticleActivity" => TypeHint::String,
            _ => TypeHint::Any,
        });
        api.types.push(descriptor);
    }
    api.refine("reel::when", named("reel::types::Condition"), &[]);
    geometry::types::describe(api);
}

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Wait until an accepted frame includes current stage changes and prepared assets.
        /// Return an awaitable result owned by the supplied node or current owner.
        /// Other tasks and media continue while presentation is pending.
        "reel::stage::presented";
        /// Create a one-shot awaitable with observable `pending`. Resolve accepts any
        /// value; reject throws the supplied value; cancel throws Cancelled. The first
        /// settlement returns true, later attempts false. Owner retirement cancels it.
        "reel::completion";
        /// Settle a pending completion with the supplied value, defaulting to null.
        /// Return true only for the first settlement. Values are returned without adoption.
        "reel::completion::resolve";
        /// Reject a pending completion with this error value. Await rethrows it while
        /// preserving the producer fault when available. Return true on first settlement.
        "reel::completion::reject";
        /// Cancel a pending completion. Its waiters receive Cancelled. Return true only
        /// when this call settles the result.
        "reel::completion::cancel";
        /// Wait for a completion or task result. Repeated awaits observe its terminal
        /// value or fault; cancellation of one consumer removes only that consumer's wait.
        "reel::completion::await";
        /// Start a zero-argument callable as an owned task and return its awaitable result.
        /// It begins after the caller yields or returns. Background tasks can animate and
        /// handle input; main-story dialogue and choice remain on the main task.
        "reel::spawn";
        /// Request cancellation of this task. Return true for the first request.
        /// `pending` stays true until cleanup finishes. Cancellation retires owned work
        /// without executing script finally blocks.
        "reel::task::cancel";
    }
}
use aside_vm::{
    Heap, HeapError, HeapReservation, RootKind, RootToken, Value, ValueTag,
    bytecode::{FunctionId, FunctionKind, NativeId},
    executor::{Completion, HostResponse, Vm, WaitId},
};
use std::collections::VecDeque;
pub(super) mod geometry;

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
    Condition = -1108,
    AssetRead = -1109,
}

#[repr(u32)]
enum Slot {
    Tag,
    State,
    Owner,
    Value,
    Target,
    Attachment,
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
    #[serde(skip)]
    type_ids: [Option<aside_vm::HostTypeId>; 9],
    #[serde(skip)]
    geometry_keys: super::observable::SchemaKeys,
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
    #[serde(skip)]
    owned: VecDeque<RootToken>,
    #[serde(skip)]
    owned_charge: Option<HeapReservation>,
    owned_remaining: usize,
    owned_dirty: bool,
    owner_revision: u64,
    #[cfg(test)]
    #[serde(skip)]
    pub(super) owned_inspections: usize,
}
impl Results {
    pub(super) fn request_presentation(&mut self) -> Result<u64, AdapterError> {
        self.requested = self
            .requested
            .checked_add(1)
            .ok_or(AdapterError::Input("presentation sequence exhausted"))?;
        self.changed();
        Ok(self.requested)
    }

    pub(super) fn presentation_reached(&self, request: u64) -> bool {
        request <= self.presented
    }

    pub(super) fn presentation_roots(&self) -> impl Iterator<Item = RootToken> + '_ {
        self.presentations.iter().copied()
    }
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
                )
                .chain(
                    self.owned
                        .iter()
                        .map(|root| Record::new(Part::OwnedResult, root)),
                ),
        )
    }
    pub(super) fn restore_record(
        &mut self,
        record: &super::snapshot::Record,
        map: &aside_vm::snapshot::SnapshotMap,
        heap: &mut Heap,
    ) -> Result<(), AdapterError> {
        use super::snapshot::Part;
        match record.part {
            Part::Results => {
                *self = record.read()?;
                self.presented = 0;
                self.dirty = true;
                self.owned_dirty = true;
            }
            Part::ResultWait => {
                let (wait, value): (WaitId, Value) = record.read()?;
                reserve_queue(heap, &mut self.waits, &mut self.charge)?;
                self.waits.push_back((map.wait(wait)?, map.value(value)?));
            }
            Part::Presentation => {
                let root = map.root(record.read()?)?;
                geometry::restore(heap, heap.root_value(root)?)?;
                reserve_queue(heap, &mut self.presentations, &mut self.presentation_charge)?;
                self.presentations.push_back(root);
            }
            Part::OwnedResult => {
                let root = map.root(record.read()?)?;
                reserve_queue(heap, &mut self.owned, &mut self.owned_charge)?;
                self.owned.push_back(root);
            }
            _ => unreachable!(),
        }
        Ok(())
    }
}
pub(super) fn readonly(
    heap: &aside_vm::PropertyRead<'_>,
    object: Value,
    field: u32,
    value: Value,
) -> Result<(), HeapError> {
    // The property is registered read-only. This validator also runs when the
    // producer publishes it; methods remain immutable ordinary fields.
    if field == 0
        && matches!(
            heap.host_payload(object, 0)?.as_int(),
            Some(-1101 | -1102 | -1108)
        )
        && value.as_bool().is_some()
    {
        Ok(())
    } else {
        Err(HeapError::ReadOnly)
    }
}
fn settle(heap: &mut Heap, object: Value, state: State, value: Value) -> Result<(), AdapterError> {
    // Publication may reserve property traversal space. Complete it before the
    // infallible writes to this validated result's private terminal slots.
    if matches!(
        kind(heap, object)?,
        Kind::Completion | Kind::Task | Kind::Condition
    ) {
        super::observable::publish(heap, object, 0, Value::bool(false))?;
    }
    if kind(heap, object)? == Kind::Condition {
        let output = heap.host_payload(object, Slot::Attachment as u32)?;
        if output.tag() != ValueTag::Null {
            heap.disconnect_derived(output)?;
            heap.set_host_payload(object, Slot::Attachment as u32, Value::NULL)?;
        }
    }
    heap.set_host_payload(object, Slot::Value as u32, value)?;
    heap.set_host_payload(object, Slot::State as u32, Value::int(state as i64))?;
    heap.set_host_payload(object, Slot::Owner as u32, Value::NULL)?;
    if matches!(kind(heap, object)?, Kind::Particle | Kind::Presentation) {
        heap.set_host_payload(object, Slot::Target as u32, Value::NULL)?;
    }
    if kind(heap, object)? == Kind::Presentation {
        heap.set_host_payload(object, Slot::Attachment as u32, Value::NULL)?;
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
        Some(-1108) => Ok(Kind::Condition),
        Some(-1109) => Ok(Kind::AssetRead),
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
        if kind == Kind::Presentation && geometry::retired(heap, object)? {
            settle(heap, object, State::Cancelled, Value::NULL)?;
            return Ok(State::Cancelled);
        }
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
pub(super) fn owner(heap: &Heap, object: Value) -> Result<Value, AdapterError> {
    Ok(heap.host_payload(object, Slot::Owner as u32)?)
}
pub(super) fn attach(heap: &mut Heap, result: Value, value: Value) -> Result<(), AdapterError> {
    heap.set_host_payload(result, Slot::Attachment as u32, value)?;
    Ok(())
}
pub(super) fn attached(heap: &Heap, result: Value) -> Result<Value, AdapterError> {
    Ok(heap.host_payload(result, Slot::Attachment as u32)?)
}
pub(super) fn asset_path(heap: &Heap, result: Value) -> Result<Value, AdapterError> {
    if kind(heap, result)? != Kind::AssetRead {
        return Err(AdapterError::Input("expected asset read result"));
    }
    let value = heap.host_payload(result, Slot::Target as u32)?;
    if heap.info(value)?.kind != aside_vm::ObjectKind::String {
        return Err(AdapterError::Input("asset read source requires string"));
    }
    Ok(value)
}
pub(super) fn asset_error(heap: &Heap, result: Value) -> Result<Value, AdapterError> {
    Ok(heap.host_payload(result, Slot::Value as u32)?)
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
    pub(super) fn create_condition(
        &mut self,
        vm: &mut Vm,
        root_owner: aside_vm::RootOwner,
        owner: Value,
    ) -> Result<Value, AdapterError> {
        let result = self.create(vm, owner, Kind::Condition)?;
        self.track_completion(vm, root_owner, result)?;
        Ok(result)
    }
    pub(super) fn settle_condition(
        &mut self,
        heap: &mut Heap,
        result: Value,
        value: Value,
        failed: bool,
    ) -> Result<(), AdapterError> {
        if pending(heap, result)? {
            settle(
                heap,
                result,
                if failed {
                    State::Faulted
                } else {
                    State::Resolved
                },
                value,
            )?;
            self.changed();
            self.owners_changed();
        }
        Ok(())
    }
    /// One producer root per result, including while its presentation is disarmed.
    pub(super) fn create_presentation(
        &mut self,
        vm: &mut Vm,
        root_owner: aside_vm::RootOwner,
        owner: Value,
        ty: Option<aside_vm::HostTypeId>,
    ) -> Result<Value, AdapterError> {
        let result = self.create_typed(vm, owner, Kind::Presentation, ty)?;
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
    pub(super) fn create_asset_read(
        &mut self,
        vm: &mut Vm,
        owner: Value,
        path: Value,
        chunk: usize,
    ) -> Result<Value, AdapterError> {
        let result = self.create(vm, owner, Kind::AssetRead)?;
        vm.heap_mut()
            .set_host_payload(result, Slot::Target as u32, path)?;
        // A transfer can exhaust VM capacity. Preserve a catchable terminal
        // message before IO begins, while constructing this result can fail
        // through the ordinary native admission boundary.
        let fallback = super::text::alloc_string(
            vm.heap_mut(),
            "asset data could not be retained within the VM budget",
            chunk,
        )?;
        vm.heap_mut()
            .set_host_payload(result, Slot::Value as u32, fallback)?;
        Ok(result)
    }
    fn create(&mut self, vm: &mut Vm, owner: Value, kind: Kind) -> Result<Value, AdapterError> {
        self.create_typed(vm, owner, kind, None)
    }
    fn create_typed(
        &mut self,
        vm: &mut Vm,
        owner: Value,
        kind: Kind,
        ty: Option<aside_vm::HostTypeId>,
    ) -> Result<Value, AdapterError> {
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
            Value::NULL,
        ];
        let object = heap.alloc_host(
            fields,
            &slots[..if matches!(kind, Kind::Presentation | Kind::Condition | Kind::AssetRead) {
                6
            } else if kind == Kind::Particle {
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
        let type_index = (-(kind as i64) - 1101) as usize;
        let type_id = if let Some(id) = ty.or(self.type_ids[type_index]) {
            id
        } else {
            let name = match kind {
                Kind::Completion => "Completion",
                Kind::Task => "Task",
                Kind::Presentation => "Presentation",
                Kind::Particle => "ParticleActivity",
                Kind::Operation => "Operation",
                Kind::Capture => "Capture",
                Kind::Window => "WindowOperation",
                Kind::Condition => "Condition",
                Kind::AssetRead => "AssetRead",
            };
            let id = heap.host_type(&format!("reel::types::{name}"))?;
            self.type_ids[type_index] = Some(id);
            id
        };
        heap.set_host_type(object, type_id)?;
        let observable = matches!(kind, Kind::Completion | Kind::Task | Kind::Condition);
        if observable {
            let key = heap.alloc_string("pending")?;
            heap.table_store(fields, 0, key, Value::bool(true))?;
            heap.observe_host_properties(object, 0, 1)?;
        }
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
                u32::from(observable)
                    + if kind != Kind::Completion {
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
    fn track_completion(
        &mut self,
        vm: &mut Vm,
        root_owner: aside_vm::RootOwner,
        result: Value,
    ) -> Result<(), AdapterError> {
        if owner(vm.heap(), result)?.tag() == ValueTag::Null {
            return Ok(());
        }
        reserve_queue(vm.heap(), &mut self.owned, &mut self.owned_charge)?;
        let weak = vm.heap_mut().alloc_weakref(result)?;
        let root = vm.heap_mut().add_root(root_owner, RootKind::Host, weak)?;
        self.owned.push_back(root);
        Ok(())
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
    pub fn owners_changed(&mut self) {
        self.owned_dirty = true;
        self.changed();
    }
    pub fn needs_work(&self, stage_revision: u64, owner_revision: u64) -> bool {
        !self.owned.is_empty()
            && (self.owned_dirty
                || self.owned_remaining != 0
                || self.owner_revision != owner_revision)
            || (!self.waits.is_empty() || !self.presentations.is_empty())
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
        owner_revision: u64,
        mut current: impl FnMut(&mut Heap, Value, Value) -> Result<bool, AdapterError>,
    ) -> Result<bool, AdapterError> {
        if !self.needs_work(stage_revision, owner_revision) {
            return Ok(false);
        }
        if self.owned_remaining == 0 && (self.owned_dirty || self.owner_revision != owner_revision)
        {
            self.owned_remaining = self.owned.len();
            self.owned_dirty = false;
            self.owner_revision = owner_revision;
        }
        for _ in 0..self.owned_remaining.min(64) {
            #[cfg(test)]
            {
                self.owned_inspections += 1;
            }
            let root = *self.owned.front().unwrap();
            let heap = vm.heap_mut();
            let weak = heap.root_value(root)?;
            let keep = if let Some(object) = heap.peek_weakref(weak)? {
                state(heap, object)? == State::Pending
            } else {
                false
            };
            self.owned.pop_front();
            if keep {
                self.owned.push_back(root);
            } else {
                heap.release_root(root)?;
            }
            self.owned_remaining -= 1;
        }
        if self.owned_remaining != 0 {
            return Ok(true);
        }
        if self.owned.is_empty() {
            self.owned = VecDeque::new();
            self.owned_charge = None;
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
                && heap.sequence_get(barrier, 0)?.as_int().unwrap() as u64 <= self.presented
                && (heap.host_payload(object, Slot::Attachment as u32)?.tag() == ValueTag::Null
                    || heap.sequence_get(barrier, 1)?.tag() != ValueTag::Null);
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
        Ok(self.needs_work(stage_revision, owner_revision))
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
        self.owned = VecDeque::new();
        self.owned_charge = None;
        self.owned_remaining = 0;
        self.owned_dirty = false;
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
        let object = if symbol == "reel::scope::spawn" {
            arguments[1]
        } else {
            arguments[0]
        };
        if matches!(symbol, "reel::spawn" | "reel::scope::spawn") {
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
            let owner = if symbol == "reel::scope::spawn" {
                arguments[0]
            } else if arguments[1].tag() == ValueTag::Null {
                self.default_activity_owner(ticket.task)?
            } else {
                arguments[1]
            };
            super::scopes::validate_owner(self.vm.heap(), &self.stage, owner)?;
            let result = self.results.create(&mut self.vm, owner, Kind::Task)?;
            self.start_owned_task(session, None, object, &[], Some(result))?;
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
                super::scopes::validate_owner(self.vm.heap(), &self.stage, owner)?;
                let kind = if presentation {
                    Kind::Presentation
                } else {
                    Kind::Completion
                };
                if presentation {
                    let result = self.results.create_presentation(
                        &mut self.vm,
                        self.root_owner,
                        owner,
                        None,
                    )?;
                    self.results
                        .arm_presentation(&mut self.vm, result, Value::NULL)?;
                    result
                } else {
                    let result = self.results.create(&mut self.vm, owner, kind)?;
                    self.results
                        .track_completion(&mut self.vm, self.root_owner, result)?;
                    result
                }
            } else {
                let kind = kind(self.vm.heap(), object)?;
                if kind != Kind::Completion
                    && !(matches!(
                        kind,
                        Kind::Presentation
                            | Kind::Capture
                            | Kind::Window
                            | Kind::Condition
                            | Kind::AssetRead
                    ) && symbol == "reel::completion::cancel")
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
                    if matches!(kind, Kind::Completion | Kind::Condition) {
                        self.results.owners_changed();
                    }
                }
                Value::bool(pending)
            };
            self.vm.respond(ticket, HostResponse::Ready(value))?;
        }
        self.issued = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_pending_publication_failure_preserves_the_terminal_transaction() {
        let mut heap = Heap::new(
            aside_vm::SessionEpoch(1),
            aside_vm::HeapLimits {
                max_bytes: 1_000_000,
                ..Default::default()
            },
        )
        .unwrap();
        let fields = heap.alloc_table().unwrap();
        let name = heap.alloc_string("pending").unwrap();
        heap.table_store(fields, 0, name, Value::bool(true))
            .unwrap();
        let result = heap
            .alloc_host(
                fields,
                &[
                    Value::int(Kind::Completion as i64),
                    Value::int(State::Pending as i64),
                    Value::NULL,
                    Value::NULL,
                ],
                Some(readonly),
            )
            .unwrap();
        heap.observe_host_properties(result, 0, 1).unwrap();
        let usage = heap.budget_usage();
        let full = usage.reserve(usage.remaining_bytes()).unwrap();
        assert!(settle(&mut heap, result, State::Resolved, Value::int(42)).is_err());
        assert!(pending(&mut heap, result).unwrap());
        assert_eq!(heap.table_entry(fields, 0).unwrap().1.as_bool(), Some(true));
        assert!(
            heap.host_payload(result, Slot::Value as u32)
                .unwrap()
                .same_bits(Value::NULL)
        );
        drop(full);
        settle(&mut heap, result, State::Resolved, Value::int(42)).unwrap();
        assert!(!pending(&mut heap, result).unwrap());
        assert_eq!(
            heap.table_entry(fields, 0).unwrap().1.as_bool(),
            Some(false)
        );
        assert_eq!(
            heap.host_payload(result, Slot::Value as u32)
                .unwrap()
                .as_int(),
            Some(42)
        );
    }
}
