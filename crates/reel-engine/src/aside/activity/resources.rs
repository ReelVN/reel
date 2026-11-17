//! Action descriptions and occurrence progress in the shared logical graph.
use super::*;
use crate::aside::data::{LogicalReference, TypedDataError};
use aside_vm::{HeapHandle, HeapReservation};
use std::borrow::Cow;
mod descriptions;
mod progress;

struct Pending {
    handle: HeapHandle,
    saved: State,
    root_scope: bool,
    progress: Value,
    node: u32,
    track: u32,
    node_started: bool,
    build: Option<graph::Build>,
    admitted: bool,
    adopt_node: u32,
}
#[derive(Default)]
pub(super) struct Restoring {
    entries: BTreeMap<u64, Pending>,
    prepare_after: Option<u64>,
    adopt_after: Option<u64>,
    charge: Option<HeapReservation>,
    await_scan: usize,
    await_native: Option<aside_vm::bytecode::NativeId>,
    default_owner: Option<Value>,
}
fn is(heap: &Heap, value: Value, tag: i64) -> bool {
    heap.host_payload(value, 0)
        .is_ok_and(|v| v.as_int() == Some(tag))
}
impl Activities {
    fn charge_restoring(&mut self, heap: &Heap, bytes: u64) -> Result<(), TypedDataError> {
        if self.logical.charge.is_none() {
            self.logical.charge = Some(heap.budget_usage().reserve(0)?);
        }
        let charge = self.logical.charge.as_mut().unwrap();
        charge.resize(
            charge
                .bytes()
                .checked_add(bytes)
                .ok_or(HeapError::AllocationFailure)?,
        )?;
        Ok(())
    }
    pub(in crate::aside) fn resource_property_name(
        heap: &Heap,
        property: PropertyId,
    ) -> Result<Option<&'static str>, AdapterError> {
        if is(heap, property.host, ACTIVITY)
            && heap.table_len(heap.host_fields(property.host)?)? == 2
        {
            return Ok(match property.field {
                0 => Some("ticks"),
                1 => Some("elapsed"),
                _ => None,
            });
        }
        Ok(None)
    }
    pub(in crate::aside) fn resource_header(
        &self,
        heap: &Heap,
        value: Value,
    ) -> Option<Result<LogicalReference, TypedDataError>> {
        if progress::source(heap, value) {
            return Some(
                progress::count(heap, value)
                    .map(|fields| LogicalReference {
                        payload: Vec::new(),
                        kind: "reel.activity.progress".into(),
                        version: 1,
                        fields,
                    })
                    .map_err(Into::into),
            );
        }
        if is(heap, value, ACTION) {
            return Some(descriptions::header(heap, value));
        }
        if !is(heap, value, ACTIVITY) {
            return None;
        }
        Some((|| {
            let clock = heap.table_len(heap.host_fields(value)?)? == 2;
            let finished = terminal(state(heap, value)?);
            Ok(LogicalReference {
                payload: Vec::new(),
                kind: match (clock, finished) {
                    (false, false) => "reel.activity.running",
                    (true, false) => "reel.activity.clock",
                    (false, true) => "reel.activity.terminal",
                    (true, true) => "reel.activity.clock_terminal",
                }
                .into(),
                version: 1,
                fields: (if finished { 1 } else { 5 }) + if clock { 2 } else { 0 },
            })
        })())
    }
    pub(in crate::aside) fn resource_field(
        &self,
        heap: &Heap,
        stage: &StageBinding,
        value: Value,
        index: u32,
        default_owner: Value,
    ) -> Result<(Cow<'static, str>, Value), TypedDataError> {
        if progress::source(heap, value) {
            return progress::field(self, heap, stage, value, index);
        }
        if is(heap, value, ACTION) {
            return descriptions::field(heap, stage, value, index);
        }
        let finished = terminal(state(heap, value)?);
        let clock_index = if finished { 1 } else { 5 };
        Ok(match index {
            0 => ("state".into(), root_get(heap, value, RootSlot::State)?),
            i if i == clock_index => (
                "ticks".into(),
                heap.host_property(PropertyId {
                    host: value,
                    field: 0,
                })?,
            ),
            i if i == clock_index + 1 => (
                "clock_elapsed".into(),
                heap.host_property(PropertyId {
                    host: value,
                    field: 1,
                })?,
            ),
            1 if !finished => (
                "description".into(),
                root_get(heap, value, RootSlot::Description)?,
            ),
            2 if !finished => (
                "owner".into(),
                stage::resources::logical_owner(
                    heap,
                    stage,
                    root_get(heap, value, RootSlot::Owner)?,
                    default_owner,
                )?
                .0,
            ),
            3 if !finished => ("progress".into(), root_get(heap, value, RootSlot::Graph)?),
            4 if !finished => (
                "root_scope".into(),
                Value::bool(
                    stage::resources::logical_owner(
                        heap,
                        stage,
                        root_get(heap, value, RootSlot::Owner)?,
                        default_owner,
                    )?
                    .1,
                ),
            ),
            _ => return Err(TypedDataError::Contract("invalid activity resource field")),
        })
    }
    pub(in crate::aside) fn resource_allocate(
        &mut self,
        heap: &mut Heap,
        stage: &StageBinding,
        _owner: RootOwner,
        header: &LogicalReference,
    ) -> Result<Value, TypedDataError> {
        if header.kind == "reel.activity.progress" {
            // The current builder's branch stack and writer index are temporary
            // native storage, bounded by the saved occurrences and tracks.
            self.charge_restoring(heap, 256 + u64::from(header.fields) * 128)?;
            return progress::allocate(heap);
        }
        if header.kind.starts_with("reel.action.") {
            return descriptions::allocate(heap, stage, header);
        }
        let clock = match header.kind.as_str() {
            "reel.activity.running" | "reel.activity.terminal" => false,
            "reel.activity.clock" | "reel.activity.clock_terminal" => true,
            _ => {
                return Err(TypedDataError::Contract(
                    "unsupported activity resource kind",
                ));
            }
        };
        let fields = heap.alloc_table()?;
        if clock {
            for (i, name) in ["ticks", "elapsed"].into_iter().enumerate() {
                let key = heap.alloc_string(name)?;
                heap.table_store(
                    fields,
                    i as u32,
                    key,
                    if i == 0 {
                        Value::int(0)
                    } else {
                        Value::float(0.)
                    },
                )?;
            }
        }
        let results = heap.alloc_array(&[])?;
        for name in [
            "Completed",
            "Stopped",
            "animated property requires a numeric start value",
        ] {
            let value = heap.alloc_string(name)?;
            heap.array_push(results, value)?;
        }
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or(AdapterError::Input("activity identity exhausted"))?;
        let mut slots = [Value::NULL; RootSlot::Forcing as usize + 1];
        slots[RootSlot::Tag as usize] = Value::int(ACTIVITY);
        slots[RootSlot::State as usize] = Value::int(State::Paused as i64);
        slots[RootSlot::Graph as usize] = heap.alloc_array(&[])?;
        slots[RootSlot::Results as usize] = results;
        slots[RootSlot::Serial as usize] = Value::int(self.serial as i64);
        slots[RootSlot::PausedAt as usize] = Value::float(self.time);
        slots[RootSlot::Forcing as usize] = Value::bool(false);
        let value = heap.alloc_host(fields, &slots, if clock { None } else { Some(&READONLY) })?;
        stage.types.mark(
            heap,
            value,
            if clock {
                "reel::types::Clock"
            } else {
                "reel::types::Activity"
            },
        )?;
        if clock {
            heap.observe_host_properties(value, 0, 2)?;
        }
        self.charge_restoring(heap, size_of::<Pending>() as u64 + 512)?;
        let handle = heap.handle(value)?;
        self.logical.entries.insert(
            handle.key,
            Pending {
                handle,
                saved: State::Paused,
                root_scope: false,
                progress: Value::NULL,
                node: 0,
                track: 0,
                node_started: false,
                build: None,
                admitted: false,
                adopt_node: 0,
            },
        );
        Ok(value)
    }
    pub(in crate::aside) fn resource_connect(
        &mut self,
        heap: &mut Heap,
        stage: &StageBinding,
        value: Value,
        name: &str,
        field: Value,
    ) -> Result<(), TypedDataError> {
        if progress::restored(heap, value) {
            return progress::connect(heap, value, name, field);
        }
        if is(heap, value, ACTION) {
            return descriptions::connect(heap, stage, value, name, field);
        }
        let pending = self
            .logical
            .entries
            .get_mut(&heap.handle(value)?.key)
            .ok_or(TypedDataError::Contract("activity shell is missing"))?;
        match name {
            "description" => {
                tagged(heap, field, ACTION)?;
                root_set(heap, value, RootSlot::Description, field)?;
            }
            "owner" => {
                if present(field) {
                    stage.id(heap, field)?;
                }
                root_set(heap, value, RootSlot::Owner, field)?;
            }
            "state" => {
                pending.saved = match field.as_int() {
                    Some(0) => State::Running,
                    Some(1) => State::Paused,
                    Some(3) => State::Completed,
                    Some(4) => State::Stopped,
                    Some(5) => State::Cancelled,
                    Some(6) => State::Failed,
                    _ => {
                        return Err(TypedDataError::Contract(
                            "activity is between committed execution phases",
                        ));
                    }
                }
            }
            "root_scope" => pending.root_scope = field.as_bool().ok_or(HeapError::TypeMismatch)?,
            "progress" => {
                heap.sequence_len(field)?;
                pending.progress = field;
            }
            "ticks" => {
                if !field.as_int().is_some_and(|v| v >= 0) {
                    return Err(TypedDataError::Contract("invalid clock ticks"));
                }
                super::super::observable::publish(heap, value, 0, field)?;
            }
            "clock_elapsed" => {
                stage::nonnegative(field)?;
                super::super::observable::publish(heap, value, 1, field)?;
            }
            _ => return Err(TypedDataError::Contract("unknown activity resource field")),
        }
        Ok(())
    }
    pub(in crate::aside) fn resource_finish(
        &mut self,
        heap: &mut Heap,
        stage: &StageBinding,
        owner: RootOwner,
        value: Value,
        cursor: &mut u32,
    ) -> Result<bool, TypedDataError> {
        if progress::restored(heap, value) {
            return Ok(true);
        }
        if is(heap, value, ACTION) {
            return descriptions::finish(heap, value, cursor);
        }
        let handle = heap.handle(value)?;
        let pending = self
            .logical
            .entries
            .get_mut(&handle.key)
            .ok_or(TypedDataError::Contract("activity shell is missing"))?;
        if terminal(pending.saved) {
            root_set(
                heap,
                value,
                RootSlot::State,
                Value::int(pending.saved as i64),
            )?;
            root_set(heap, value, RootSlot::Graph, Value::NULL)?;
            pending.admitted = true;
            return Ok(true);
        }
        if *cursor == 0 {
            let build = pending
                .build
                .get_or_insert_with(|| graph::Build::new(value));
            let done = build.step(heap, stage, 1)?.0;
            if heap.sequence_len(root_get(heap, value, RootSlot::Graph)?)?
                > heap.sequence_len(pending.progress)?
            {
                return Err(TypedDataError::Contract(
                    "saved progress does not match action occurrence count",
                ));
            }
            if done {
                pending.build = None;
                *cursor = 1;
            }
            return Ok(false);
        }
        if !progress::apply(heap, stage, pending, self.time, value)? {
            return Ok(false);
        }
        if !pending.admitted {
            if self.jobs.is_none() {
                self.jobs = Some(heap.add_root(owner, RootKind::Host, Value::NULL)?);
            }
            if self.head.is_none() {
                self.head = Some(heap.add_root(owner, RootKind::Host, Value::NULL)?);
            }
            let first = self.first(heap)?;
            root_set(heap, value, RootSlot::Next, first)?;
            if present(first) {
                root_set(heap, first, RootSlot::Previous, value)?;
            }
            heap.set_root(self.head.unwrap(), value)?;
            self.logical.entries.get_mut(&handle.key).unwrap().admitted = true;
        }
        Ok(true)
    }
    /// Bind current native await adapters before starting the current bootstrap.
    pub(in crate::aside) fn prepare_restored_step(
        &mut self,
        vm: &mut Vm,
        work: u32,
    ) -> Result<bool, AdapterError> {
        use std::ops::Bound::{Excluded, Unbounded};
        for _ in 0..work {
            let entry = if let Some(after) = self.logical.prepare_after {
                self.logical
                    .entries
                    .range((Excluded(after), Unbounded))
                    .next()
            } else {
                self.logical.entries.first_key_value()
            };
            let Some((&handle, pending)) = entry else {
                return Ok(true);
            };
            if self.logical.await_native.is_none() {
                let package = vm.program().package();
                let index = self.logical.await_scan;
                let definition = package
                    .natives
                    .get(index)
                    .ok_or(AdapterError::Input("activity await adapter missing"))?;
                self.logical.await_scan += 1;
                if package.symbols[definition.symbol.index()] == "reel::activity::await" {
                    self.logical.await_native = Some(aside_vm::bytecode::NativeId(index as u32));
                }
                continue;
            }
            if let Ok(value) = vm.heap().import_handle(pending.handle) {
                vm.bind_awaitable(value, self.logical.await_native.unwrap())?;
            }
            self.logical.prepare_after = Some(handle);
        }
        Ok(false)
    }
    /// Retain the current Root's scope identity until restored activities adopt it.
    pub(in crate::aside) fn set_restored_default_owner(&mut self, owner: Value) {
        self.logical.default_owner = Some(owner);
    }
    /// Rebase one frozen occurrence per unit and reconnect its deadline on adoption.
    pub(in crate::aside) fn adopt_restored_step(
        &mut self,
        heap: &mut Heap,
        work: u32,
    ) -> Result<bool, AdapterError> {
        use std::ops::Bound::{Excluded, Unbounded};
        for _ in 0..work {
            let entry = if let Some(after) = self.logical.adopt_after {
                self.logical
                    .entries
                    .range_mut((Excluded(after), Unbounded))
                    .next()
            } else {
                self.logical.entries.iter_mut().next()
            };
            let Some((&handle, pending)) = entry else {
                self.logical = Restoring::default();
                return Ok(true);
            };
            let saved = pending.saved;
            if let Ok(value) = heap.import_handle(pending.handle)
                && !terminal(state(heap, value)?)
            {
                let nodes = root_get(heap, value, RootSlot::Graph)?;
                if pending.adopt_node < heap.sequence_len(nodes)? {
                    let index = pending.adopt_node;
                    pending.adopt_node += 1;
                    let current = heap.sequence_get(nodes, index)?;
                    let start = graph::get(heap, current, graph::Slot::StartedAt)?
                        .as_float()
                        .unwrap();
                    let frozen = root_get(heap, value, RootSlot::PausedAt)?
                        .as_float()
                        .unwrap();
                    graph::set(
                        heap,
                        current,
                        graph::Slot::StartedAt,
                        Value::float(start + self.time - frozen),
                    )?;
                    if saved == State::Running
                        && graph::kind(heap, current)?.is_leaf()
                        && graph::phase(heap, current)? == graph::Phase::Active
                    {
                        self.schedule(heap, current)?;
                    } else if saved == State::Running
                        && index == 0
                        && graph::phase(heap, current)? == graph::Phase::Inactive
                    {
                        self.queue(heap, current, graph::Job::Activate, self.time, false)?;
                    }
                    continue;
                }
                root_set(heap, value, RootSlot::PausedAt, Value::float(self.time))?;
                if pending.root_scope {
                    root_set(
                        heap,
                        value,
                        RootSlot::Owner,
                        self.logical.default_owner.ok_or(AdapterError::Input(
                            "restored Root scope owner is unavailable",
                        ))?,
                    )?;
                }
                root_set(heap, value, RootSlot::State, Value::int(saved as i64))?;
            }
            self.logical.adopt_after = Some(handle);
        }
        Ok(false)
    }
}
