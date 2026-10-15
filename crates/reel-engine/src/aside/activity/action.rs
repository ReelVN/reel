//! Immutable author descriptions. Combinators use ordinary VM rest binding.
use super::*;
use aside_vm::executor::{Continuation, FaultRetention, NativeInput};

pub(super) const KIND: u32 = 5;
pub(super) const TIMES: u32 = 6;
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub(in crate::aside) enum Kind {
    Tween,
    Sequence,
    Parallel,
    Repeat,
    Delay,
    Clock,
}

impl Kind {
    pub(super) fn is_leaf(self) -> bool {
        matches!(self, Self::Tween | Self::Delay | Self::Clock)
    }
}

pub(super) fn kind(heap: &Heap, action: Value) -> Result<Kind, AdapterError> {
    Ok(match payload(heap, action, KIND)?.as_int().unwrap() {
        0 => Kind::Tween,
        1 => Kind::Sequence,
        2 => Kind::Parallel,
        3 => Kind::Repeat,
        4 => Kind::Delay,
        5 => Kind::Clock,
        _ => unreachable!("private action schema"),
    })
}

pub(super) fn duration(heap: &Heap, action: Value) -> Result<f64, AdapterError> {
    tagged(heap, action, ACTION)?;
    Ok(payload(heap, action, DURATION)?
        .as_float()
        .unwrap_or(f64::INFINITY))
}

fn description(
    heap: &mut Heap,
    kind: Kind,
    children: Value,
    seconds: f64,
    times: Value,
) -> Result<Value, AdapterError> {
    let fields = heap.alloc_table()?;
    Ok(heap.alloc_host(
        fields,
        &[
            Value::int(ACTION),
            Value::NULL,
            if seconds.is_infinite() {
                Value::NULL
            } else {
                Value::float(seconds)
            },
            Value::NULL,
            children,
            Value::int(kind as i64),
            times,
        ],
        Some(readonly),
    )?)
}

/// The fresh bound rest array is private to this call. Retain it directly in
/// NativeFrame and then in the immutable description, without a second copy.
pub(in crate::aside) fn compose(
    vm: &mut Vm,
    ticket: HostTicket,
    kind: Kind,
    budget: u32,
) -> Result<u32, AdapterError> {
    let (children, mut index, mut seconds) = match ticket.input {
        NativeInput::Entry => (vm.host_arguments(ticket)?[0], 0, 0.0_f64),
        NativeInput::Continue { .. } => {
            let saved = vm.host_continuation(ticket)?;
            (
                saved[0],
                saved[1].as_int().unwrap() as u32,
                saved[2].as_float().unwrap(),
            )
        }
        _ => return Err(AdapterError::Input("unexpected action continuation")),
    };
    let count = vm.heap().sequence_len(children)?;
    let mut work = 0;
    while index < count && work < budget {
        let child = vm.heap().sequence_get(children, index)?;
        let child_seconds = duration(vm.heap(), child)?;
        let combined = if kind == Kind::Parallel {
            seconds.max(child_seconds)
        } else {
            seconds + child_seconds
        };
        if combined.is_infinite() && seconds.is_finite() && child_seconds.is_finite() {
            return Err(AdapterError::Input("combined action duration overflow"));
        }
        seconds = combined;
        index += 1;
        work += 1;
    }
    if index < count {
        vm.respond(
            ticket,
            HostResponse::Continue(Continuation {
                version: 1,
                stage: 0,
                values: &[
                    children,
                    Value::int(i64::from(index)),
                    Value::float(seconds),
                ],
                faults: FaultRetention::Preserve,
            }),
        )?;
    } else {
        let value = description(vm.heap_mut(), kind, children, seconds, Value::NULL)?;
        vm.respond(ticket, HostResponse::Ready(value))?;
    }
    Ok(work)
}

pub(in crate::aside) fn delay(heap: &mut Heap, seconds: Value) -> Result<Value, AdapterError> {
    let seconds = stage::number(seconds)?;
    if seconds < 0. {
        return Err(AdapterError::Input("delay seconds must be nonnegative"));
    }
    description(heap, Kind::Delay, Value::NULL, seconds, Value::NULL)
}

pub(super) fn clock(heap: &mut Heap, interval: Value) -> Result<Value, AdapterError> {
    let interval = stage::number(interval)?;
    if interval <= 0. {
        return Err(AdapterError::Input("clock interval must be positive"));
    }
    // Clock is an endless leaf; TIMES holds its interval instead of a repeat count.
    description(
        heap,
        Kind::Clock,
        Value::NULL,
        f64::INFINITY,
        Value::float(interval),
    )
}

pub(in crate::aside) fn repeat(
    heap: &mut Heap,
    action: Value,
    times: Value,
) -> Result<Value, AdapterError> {
    let single = duration(heap, action)?;
    let count = if times.tag() == ValueTag::Null {
        None
    } else {
        Some(
            times
                .as_int()
                .filter(|&value| value >= 0)
                .ok_or(AdapterError::Input(
                    "repeat times must be a nonnegative integer or null",
                ))?,
        )
    };
    let total = match count {
        Some(0) => 0.,
        Some(count) => {
            let total = single * count as f64;
            if total.is_infinite() && single.is_finite() {
                return Err(AdapterError::Input("repeated action duration overflow"));
            }
            total
        }
        None if single == 0. => {
            return Err(AdapterError::Input("continuous repeat must advance time"));
        }
        None => f64::INFINITY,
    };
    let children = heap.alloc_array(&[action])?;
    description(heap, Kind::Repeat, children, total, times)
}
pub(in crate::aside) fn tween(
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
    let easing =
        super::easing::Easing::parse(&super::super::text::read_string(heap, args[2], read_limit)?)?;
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
    let common = NodeProperty::PROPERTIES
        .iter()
        .filter(|p| p.animatable)
        .count();
    for ((index, _), &value) in stage::camera::CameraField::PROPERTIES
        .iter()
        .enumerate()
        .filter(|(_, p)| p.animatable)
        .zip(&args[3 + common..])
    {
        if value.tag() == ValueTag::Null {
            continue;
        }
        let id = stage.id(heap, target)?;
        if !matches!(
            stage.scene.node(id).unwrap().kind,
            reel_ui::stage::NodeKind::Camera(_)
        ) {
            return Err(AdapterError::Input(
                "camera animation requires a camera target",
            ));
        }
        stage::camera::CameraField::validate(heap, index as u32, value)?;
        heap.array_push(goals, Value::int(index as i64))?;
        heap.array_push(goals, value)?;
    }
    let fields = heap.alloc_table()?;
    Ok(heap.alloc_host(
        fields,
        &[
            Value::int(ACTION),
            target,
            Value::float(duration),
            Value::int(easing as i64),
            goals,
            Value::int(Kind::Tween as i64),
            Value::NULL,
        ],
        Some(readonly),
    )?)
}
