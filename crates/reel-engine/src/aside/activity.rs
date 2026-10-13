//! Actions and running activities live in the VM heap. The active list is one
//! traced root, not a second object store or an ever-growing terminal ledger.
use super::{
    AdapterError, FaultKind, HostTicket,
    stage::{self, NodeProperty, StageBinding},
};
use aside_host::{CallContext, PropertySchema};
use aside_vm::bytecode::Effects;
use aside_vm::executor::{Completion, HostResponse, Vm, WaitId};
use aside_vm::{Heap, HeapError, RootKind, RootOwner, RootToken, Value, ValueTag};

const ACTION: i64 = -1001;
const ACTIVITY: i64 = -1002;
// Action: tag, target, duration, easing, goals (field/end pairs).
const TARGET: u32 = 1;
const DURATION: u32 = 2;
const EASING: u32 = 3;
const GOALS: u32 = 4;
// Activity: tag, state, action, owner, tracks, elapsed, last time, next, results.
const STATE: u32 = 1;
const DESCRIPTION: u32 = 2;
const OWNER: u32 = 3;
const TRACKS: u32 = 4;
const ELAPSED: u32 = 5;
const LAST: u32 = 6;
const NEXT: u32 = 7;
const RESULTS: u32 = 8;
const PREVIOUS: u32 = 9;
// Private stage-node payload slot, independent of visible field names.
const NODE_CLAIMS: u32 = 3;
const RUNNING: i64 = 0;
const PAUSED: i64 = 1;
const COMPLETED: i64 = 2;
const STOPPED: i64 = 3;
const CANCELLED: i64 = 4;

#[derive(Default)]
pub(super) struct Activities {
    head: Option<RootToken>,
    time: f64,
    observed_time: f64,
    running: usize,
    wait: Option<(WaitId, Value)>,
    cursor: Option<Value>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivityProgress {
    pub work: u32,
    pub pending: bool,
    pub woke: bool,
}

fn payload(heap: &Heap, value: Value, slot: u32) -> Result<Value, AdapterError> {
    Ok(heap.host_payload(value, slot)?)
}
fn tagged(heap: &Heap, value: Value, tag: i64) -> Result<(), AdapterError> {
    heap.host_fields(value)?;
    if heap.host_payload(value, 0)?.as_int() != Some(tag) {
        return Err(AdapterError::Input("expected action or activity"));
    }
    Ok(())
}
fn state(heap: &Heap, activity: Value) -> Result<i64, AdapterError> {
    Ok(payload(heap, activity, STATE)?.as_int().unwrap())
}
fn readonly(_: &Heap, _: u32, _: Value) -> Result<(), HeapError> {
    Err(HeapError::TypeMismatch)
}

impl Activities {
    pub fn needs_time(&self) -> bool {
        self.running != 0
    }
    pub fn pending(&self) -> bool {
        self.cursor.is_some()
    }

    pub fn describe(
        heap: &mut Heap,
        stage: &StageBinding,
        args: &[Value],
        read_limit: usize,
    ) -> Result<Value, AdapterError> {
        let target = args[0];
        let offset = stage.property_offset(heap, target)?;
        let duration = stage::number(args[1])?;
        if duration < 0. {
            return Err(AdapterError::Input("duration must be nonnegative"));
        }
        let easing = match super::text::read_string(heap, args[2], read_limit)?.as_str() {
            "linear" => 0,
            "in" => 1,
            "out" => 2,
            "in_out" => 3,
            _ => {
                return Err(AdapterError::Input(
                    "easing requires linear, in, out or in_out",
                ));
            }
        };
        let goals = heap.alloc_array(&[])?;
        for ((index, _), &value) in NodeProperty::PROPERTIES
            .iter()
            .enumerate()
            .filter(|(_, p)| p.animatable)
            .zip(&args[3..])
        {
            if value.tag() == ValueTag::Null {
                continue;
            }
            stage::validate_layout_property(heap, index as u32, value)?;
            stage::number(value)?;
            heap.array_push(goals, Value::int(i64::from(offset) + index as i64))?;
            heap.array_push(goals, value)?;
        }
        let fields = heap.alloc_table()?;
        Ok(heap.alloc_host(
            fields,
            &[
                Value::int(ACTION),
                target,
                Value::float(duration),
                Value::int(easing),
                goals,
            ],
            Some(readonly),
        )?)
    }

    fn first(&self, heap: &Heap) -> Result<Value, AdapterError> {
        Ok(self
            .head
            .map(|root| heap.root_value(root))
            .transpose()?
            .unwrap_or(Value::NULL))
    }

    fn set_state(
        &mut self,
        heap: &mut Heap,
        activity: Value,
        next: i64,
    ) -> Result<(), AdapterError> {
        let old = state(heap, activity)?;
        if old >= COMPLETED || old == next {
            return Ok(());
        }
        if old == RUNNING {
            self.running -= 1;
        }
        if next == RUNNING {
            self.running += 1;
        }
        heap.set_host_payload(activity, STATE, Value::int(next))?;
        if next >= COMPLETED {
            self.unlink(heap, activity)?;
        }
        Ok(())
    }

    fn unlink(&mut self, heap: &mut Heap, activity: Value) -> Result<(), AdapterError> {
        let next = payload(heap, activity, NEXT)?;
        let previous = payload(heap, activity, PREVIOUS)?;
        if self.cursor.is_some_and(|v| v.same_bits(activity)) {
            self.cursor = (next.tag() != ValueTag::Null).then_some(next);
        }
        if previous.tag() == ValueTag::Null {
            heap.set_root(self.head.unwrap(), next)?;
        } else {
            heap.set_host_payload(previous, NEXT, next)?;
        }
        if next.tag() != ValueTag::Null {
            heap.set_host_payload(next, PREVIOUS, previous)?;
        }
        let target = payload(heap, payload(heap, activity, DESCRIPTION)?, TARGET)?;
        if heap.host_fields(target).is_ok() {
            let claims = payload(heap, target, NODE_CLAIMS)?;
            let tracks = payload(heap, activity, TRACKS)?;
            for i in (0..heap.sequence_len(tracks)?).step_by(4) {
                let field = heap.sequence_get(tracks, i)?.as_int().unwrap() as u32;
                if heap.sequence_get(claims, field)?.same_bits(activity) {
                    heap.set_field(claims, field, Value::NULL)?;
                }
            }
        }
        for slot in [DESCRIPTION, OWNER, TRACKS, NEXT, PREVIOUS] {
            heap.set_host_payload(activity, slot, Value::NULL)?;
        }
        if self.first(heap)?.tag() == ValueTag::Null
            && let Some(root) = self.head.take()
        {
            heap.release_root(root)?;
        }
        Ok(())
    }

    /// Check explicit retirement and script writes before any host mutation,
    /// including same-value assignments and writes made while paused.
    fn reconcile(&mut self, heap: &mut Heap, activity: Value) -> Result<(), AdapterError> {
        if state(heap, activity)? >= COMPLETED {
            return Ok(());
        }
        let action = payload(heap, activity, DESCRIPTION)?;
        let target = payload(heap, action, TARGET)?;
        let owner = payload(heap, activity, OWNER)?;
        if heap.host_fields(target).is_err()
            || (owner.tag() != ValueTag::Null && heap.host_fields(owner).is_err())
        {
            return self.set_state(heap, activity, CANCELLED);
        }
        let tracks = payload(heap, activity, TRACKS)?;
        for i in (0..heap.sequence_len(tracks)?).step_by(4) {
            let field = heap.sequence_get(tracks, i)?.as_int().unwrap() as u32;
            let version = heap.sequence_get(tracks, i + 3)?.as_int().unwrap() as u64;
            if heap.host_write_version(target, field)? != version {
                return self.set_state(heap, activity, STOPPED);
            }
        }
        Ok(())
    }

    pub fn start(
        &mut self,
        vm: &mut Vm,
        stage: &StageBinding,
        root_owner: RootOwner,
        action: Value,
        owner: Value,
    ) -> Result<Value, AdapterError> {
        tagged(vm.heap(), action, ACTION)?;
        let target = payload(vm.heap(), action, TARGET)?;
        stage.id(vm.heap(), target)?;
        if owner.tag() != ValueTag::Null {
            stage.id(vm.heap(), owner)?;
        }
        let native = vm
            .program()
            .package()
            .natives
            .iter()
            .position(|n| {
                vm.program().package().symbols[n.symbol.index()] == "reel::activity::await"
            })
            .ok_or(AdapterError::Input("activity await adapter missing"))?;
        let heap = vm.heap_mut();
        heap.track_host_writes(target)?;
        let goals = payload(heap, action, GOALS)?;
        let fields = heap.host_fields(target)?;
        let mut claims = payload(heap, target, NODE_CLAIMS)?;
        if claims.tag() == ValueTag::Null {
            claims = heap.alloc_array(&[])?;
            for _ in 0..heap.table_len(fields)? {
                heap.array_push(claims, Value::NULL)?;
            }
            heap.set_host_payload(target, NODE_CLAIMS, claims)?;
        }
        let tracks = heap.alloc_array(&[])?;
        for i in (0..heap.sequence_len(goals)?).step_by(2) {
            let field = heap.sequence_get(goals, i)?.as_int().unwrap() as u32;
            let start = stage::number(heap.table_entry(fields, field)?.1)?;
            let end = stage::number(heap.sequence_get(goals, i + 1)?)?;
            for value in [
                Value::int(i64::from(field)),
                Value::float(start),
                Value::float(end),
                Value::int(heap.host_write_version(target, field)? as i64),
            ] {
                heap.array_push(tracks, value)?;
            }
        }
        let results = heap.alloc_array(&[])?;
        for name in ["Completed", "Stopped"] {
            let value = heap.alloc_string(name)?;
            heap.array_push(results, value)?;
        }
        let fields = heap.alloc_table()?;
        let activity = heap.alloc_host(
            fields,
            &[
                Value::int(ACTIVITY),
                Value::int(RUNNING),
                action,
                owner,
                tracks,
                Value::float(0.),
                Value::float(self.time),
                Value::NULL,
                results,
                Value::NULL,
            ],
            Some(readonly),
        )?;
        // All fallible allocations precede takeover of existing activities.
        // Hold a spare root until admission commits, even if takeover removes
        // the last old activity and releases the old list root.
        let root = heap.add_root(root_owner, RootKind::Host, activity)?;
        if let Err(error) = vm.bind_awaitable(activity, aside_vm::bytecode::NativeId(native as u32))
        {
            vm.heap_mut().release_root(root)?;
            return Err(error.into());
        }
        let heap = vm.heap_mut();
        for i in (0..heap.sequence_len(tracks)?).step_by(4) {
            let field = heap.sequence_get(tracks, i)?.as_int().unwrap() as u32;
            let old = heap.sequence_get(claims, field)?;
            if old.tag() != ValueTag::Null {
                self.set_state(heap, old, STOPPED)?;
            }
        }
        let first = self.first(heap)?;
        heap.set_host_payload(activity, NEXT, first)?;
        if first.tag() != ValueTag::Null {
            heap.set_host_payload(first, PREVIOUS, activity)?;
        }
        if let Some(existing) = self.head {
            heap.set_root(existing, activity)?;
            heap.release_root(root)?;
        } else {
            self.head = Some(root);
        }
        for i in (0..heap.sequence_len(tracks)?).step_by(4) {
            let field = heap.sequence_get(tracks, i)?.as_int().unwrap() as u32;
            heap.set_field(claims, field, activity)?;
        }
        self.running += 1;
        self.update(heap, activity, false)?;
        self.maintain(vm)?;
        Ok(activity)
    }

    fn update(
        &mut self,
        heap: &mut Heap,
        activity: Value,
        finish: bool,
    ) -> Result<(), AdapterError> {
        self.reconcile(heap, activity)?;
        let current = state(heap, activity)?;
        if current >= COMPLETED || (current == PAUSED && !finish) {
            return Ok(());
        }
        let action = payload(heap, activity, DESCRIPTION)?;
        let duration = payload(heap, action, DURATION)?.as_float().unwrap();
        let elapsed = if finish {
            duration
        } else {
            (payload(heap, activity, ELAPSED)?.as_float().unwrap()
                + (self.time - payload(heap, activity, LAST)?.as_float().unwrap()))
            .min(duration)
        };
        heap.set_host_payload(activity, ELAPSED, Value::float(elapsed))?;
        heap.set_host_payload(activity, LAST, Value::float(self.time))?;
        let t = if duration == 0. {
            1.
        } else {
            (elapsed / duration).clamp(0., 1.)
        };
        let t = match payload(heap, action, EASING)?.as_int().unwrap() {
            1 => t * t * t,
            2 => 1. - (1. - t).powi(3),
            3 if t < 0.5 => 4. * t * t * t,
            3 => 1. - (-2. * t + 2.).powi(3) / 2.,
            _ => t,
        };
        let fields = heap.host_fields(payload(heap, action, TARGET)?)?;
        let tracks = payload(heap, activity, TRACKS)?;
        for i in (0..heap.sequence_len(tracks)?).step_by(4) {
            let field = heap.sequence_get(tracks, i)?.as_int().unwrap() as u32;
            let start = heap.sequence_get(tracks, i + 1)?.as_float().unwrap();
            let end = heap.sequence_get(tracks, i + 2)?.as_float().unwrap();
            // Weighted endpoints avoid overflowing end-start for finite extremes.
            let value = Value::float(if t == 1. {
                end
            } else {
                start * (1. - t) + end * t
            });
            let (key, old) = heap.table_entry(fields, field)?;
            if !value.same_bits(old) {
                heap.table_store(fields, field, key, value)?;
            }
        }
        if elapsed >= duration {
            self.set_state(heap, activity, COMPLETED)?;
        }
        Ok(())
    }

    pub fn control(
        &mut self,
        vm: &mut Vm,
        activity: Value,
        command: &str,
    ) -> Result<Value, AdapterError> {
        tagged(vm.heap(), activity, ACTIVITY)?;
        self.update(vm.heap_mut(), activity, command == "reel::finish")?;
        if state(vm.heap(), activity)? < COMPLETED {
            let next = match command {
                "reel::pause" => PAUSED,
                "reel::resume" => RUNNING,
                "reel::stop" => STOPPED,
                _ => unreachable!(),
            };
            vm.heap_mut()
                .set_host_payload(activity, LAST, Value::float(self.time))?;
            self.set_state(vm.heap_mut(), activity, next)?;
        }
        self.maintain(vm)?;
        Ok(activity)
    }

    fn response(heap: &Heap, activity: Value) -> Result<Completion, AdapterError> {
        let status = state(heap, activity)?;
        Ok(if status == CANCELLED {
            Completion::Error(aside_vm::NewFault {
                value: Value::NULL,
                code: aside_vm::FaultCode::core(FaultKind::Cancelled),
                cause: None,
            })
        } else {
            Completion::Ready(heap.sequence_get(
                payload(heap, activity, RESULTS)?,
                (status - COMPLETED) as u32,
            )?)
        })
    }

    pub fn subscribe(
        &mut self,
        vm: &mut Vm,
        ticket: HostTicket,
        activity: Value,
    ) -> Result<(), AdapterError> {
        tagged(vm.heap(), activity, ACTIVITY)?;
        self.reconcile(vm.heap_mut(), activity)?;
        if state(vm.heap(), activity)? >= COMPLETED {
            match Self::response(vm.heap(), activity)? {
                Completion::Ready(value) => vm.respond(ticket, HostResponse::Ready(value))?,
                Completion::Error(error) => vm.respond(ticket, HostResponse::Error(error))?,
                _ => unreachable!(),
            }
        } else {
            let wait = ticket.wait_id();
            vm.respond(ticket, HostResponse::Pending(wait))?;
            self.wait = Some((wait, activity));
        }
        self.maintain(vm)?;
        Ok(())
    }

    /// The native frame roots its own awaited value. Invalidated subscriptions
    /// are discarded without touching the independently owned producer.
    fn maintain(&mut self, vm: &mut Vm) -> Result<bool, AdapterError> {
        let mut woke = false;
        if let Some((wait, activity)) = self.wait {
            if !vm.wait_is_pending(wait) {
                self.wait = None;
            } else if state(vm.heap(), activity)? >= COMPLETED {
                vm.complete(wait, Self::response(vm.heap(), activity)?)?;
                self.wait = None;
                woke = true;
            }
        }
        Ok(woke)
    }

    pub fn advance(
        &mut self,
        vm: &mut Vm,
        seconds: f64,
        budget: u32,
    ) -> Result<ActivityProgress, AdapterError> {
        if !seconds.is_finite() || seconds < self.observed_time {
            return Err(AdapterError::Input(
                "activity clock must be finite and monotonic",
            ));
        }
        self.observed_time = seconds;
        if self.cursor.is_none() {
            self.time = seconds;
            let first = self.first(vm.heap())?;
            self.cursor = (first.tag() != ValueTag::Null).then_some(first);
        }
        let mut work = 0;
        while work < budget
            && let Some(activity) = self.cursor
        {
            let next = payload(vm.heap(), activity, NEXT)?;
            self.cursor = (next.tag() != ValueTag::Null).then_some(next);
            self.update(vm.heap_mut(), activity, false)?;
            work += 1;
        }
        Ok(ActivityProgress {
            work,
            pending: self.cursor.is_some(),
            woke: self.maintain(vm)?,
        })
    }

    pub fn revoke(&mut self, heap: &mut Heap) -> Result<(), AdapterError> {
        if let Some(root) = self.head.take() {
            heap.release_root(root)?;
        }
        self.running = 0;
        self.wait = None;
        self.cursor = None;
        Ok(())
    }
}

#[aside_host::native(symbol = "reel::pause", effects = Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST), capabilities = ["reel.stage"])]
pub(super) fn pause(
    #[aside(context)] context: &mut CallContext<'_, Activities>,
    activity: Value,
) -> Result<Value, AdapterError> {
    context.host.control(context.vm, activity, pause::SYMBOL)
}

#[aside_host::native(symbol = "reel::resume", effects = Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST), capabilities = ["reel.stage"])]
pub(super) fn resume(
    #[aside(context)] context: &mut CallContext<'_, Activities>,
    activity: Value,
) -> Result<Value, AdapterError> {
    context.host.control(context.vm, activity, resume::SYMBOL)
}

#[aside_host::native(symbol = "reel::stop", effects = Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST), capabilities = ["reel.stage"])]
pub(super) fn stop(
    #[aside(context)] context: &mut CallContext<'_, Activities>,
    activity: Value,
) -> Result<Value, AdapterError> {
    context.host.control(context.vm, activity, stop::SYMBOL)
}

#[aside_host::native(symbol = "reel::finish", effects = Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST), capabilities = ["reel.stage"])]
pub(super) fn finish(
    #[aside(context)] context: &mut CallContext<'_, Activities>,
    activity: Value,
) -> Result<Value, AdapterError> {
    context.host.control(context.vm, activity, finish::SYMBOL)
}
