//! Immutable descriptions use public property names and ordinary graph edges.
use super::*;

pub(super) fn header(heap: &Heap, value: Value) -> Result<LogicalReference, TypedDataError> {
    let kind = action::kind(heap, value)?;
    let fields = match kind {
        action::Kind::Tween => {
            3 + 2 * heap.sequence_len(payload(heap, value, GOALS)?)? / action::GOAL_LEN as u32
        }
        action::Kind::Sequence | action::Kind::Parallel => 2,
        action::Kind::Repeat => 3,
        _ => 1,
    };
    let name = match kind {
        action::Kind::Tween => "tween",
        action::Kind::Sequence => "sequence",
        action::Kind::Parallel => "parallel",
        action::Kind::Repeat => "repeat",
        action::Kind::Delay => "delay",
        action::Kind::Clock => "clock",
    };
    Ok(LogicalReference {
        payload: Vec::new(),
        kind: format!("reel.action.{name}"),
        version: 1,
        fields,
    })
}
pub(super) fn field(
    heap: &Heap,
    stage: &StageBinding,
    value: Value,
    index: u32,
) -> Result<(Cow<'static, str>, Value), TypedDataError> {
    let kind = action::kind(heap, value)?;
    Ok(match kind {
        action::Kind::Tween => match index {
            0 => ("target".into(), payload(heap, value, TARGET)?),
            1 => ("duration".into(), payload(heap, value, DURATION)?),
            2 => ("easing".into(), payload(heap, value, EASING)?),
            _ => {
                let goals = payload(heap, value, GOALS)?;
                let offset = (index - 3) / 2 * action::GOAL_LEN as u32;
                let target = payload(heap, value, TARGET)?;
                let field = heap
                    .sequence_get(goals, offset)?
                    .as_int()
                    .ok_or(HeapError::TypeMismatch)? as u32;
                let name = stage
                    .resource_property_name(
                        heap,
                        PropertyId {
                            host: target,
                            field,
                        },
                    )?
                    .ok_or(TypedDataError::Contract(
                        "tween target must be a public scene property",
                    ))?;
                let goal = (index - 3).is_multiple_of(2);
                (
                    format!("{}.{name}", if goal { "goal" } else { "from" }).into(),
                    heap.sequence_get(goals, offset + if goal { 2 } else { 1 })?,
                )
            }
        },
        action::Kind::Clock if index == 0 => {
            ("interval".into(), payload(heap, value, action::TIMES)?)
        }
        action::Kind::Sequence | action::Kind::Parallel | action::Kind::Repeat => match index {
            0 => ("children".into(), payload(heap, value, GOALS)?),
            1 => ("duration".into(), payload(heap, value, DURATION)?),
            2 if kind == action::Kind::Repeat => {
                ("times".into(), payload(heap, value, action::TIMES)?)
            }
            _ => return Err(TypedDataError::Contract("invalid composed action field")),
        },
        action::Kind::Delay if index == 0 => ("duration".into(), payload(heap, value, DURATION)?),
        _ => return Err(TypedDataError::Contract("invalid action field")),
    })
}
pub(super) fn allocate(
    heap: &mut Heap,
    stage: &StageBinding,
    header: &LogicalReference,
) -> Result<Value, TypedDataError> {
    let kind = match header.kind.as_str() {
        "reel.action.tween" => action::Kind::Tween,
        "reel.action.sequence" => action::Kind::Sequence,
        "reel.action.parallel" => action::Kind::Parallel,
        "reel.action.repeat" => action::Kind::Repeat,
        "reel.action.delay" => action::Kind::Delay,
        "reel.action.clock" => action::Kind::Clock,
        _ => return Err(TypedDataError::Contract("unsupported action resource kind")),
    };
    let fields = heap.alloc_table()?;
    let goals = heap.alloc_array(&[])?;
    let value = heap.alloc_host(
        fields,
        &[
            Value::int(ACTION),
            Value::NULL,
            Value::NULL,
            Value::NULL,
            goals,
            Value::int(kind as i64),
            Value::NULL,
        ],
        Some(&READONLY),
    )?;
    stage.types.mark(heap, value, "reel::types::Action")?;
    Ok(value)
}
pub(super) fn connect(
    heap: &mut Heap,
    stage: &StageBinding,
    value: Value,
    name: &str,
    field: Value,
) -> Result<(), TypedDataError> {
    let kind = action::kind(heap, value)?;
    match name {
        "target" if kind == action::Kind::Tween => {
            stage.animation_target(heap, field)?;
            heap.set_host_payload(value, TARGET, field)?;
        }
        "duration" if kind != action::Kind::Clock => {
            let duration = if present(field) {
                Value::float(stage::nonnegative(field)?)
            } else if kind.is_leaf() {
                return Err(TypedDataError::Contract(
                    "finite action duration is missing",
                ));
            } else {
                Value::NULL
            };
            heap.set_host_payload(value, DURATION, duration)?;
        }
        "easing" if kind == action::Kind::Tween => {
            if !field.as_int().is_some_and(|v| (0..=21).contains(&v))
                && aside_std::curve_key_count(heap, field).is_err()
            {
                return Err(TypedDataError::Contract(
                    "logical tween requires a built-in easing or scalar curve",
                ));
            }
            heap.set_host_payload(value, EASING, field)?;
        }
        "interval" if kind == action::Kind::Clock => {
            stage::positive(field)?;
            heap.set_host_payload(value, action::TIMES, field)?;
        }
        "children" if !kind.is_leaf() => {
            heap.sequence_len(field)?;
            heap.set_host_payload(value, GOALS, field)?;
        }
        "times" if kind == action::Kind::Repeat => {
            if present(field) && !field.as_int().is_some_and(|n| n >= 0) {
                return Err(TypedDataError::Contract("invalid action repeat count"));
            }
            heap.set_host_payload(value, action::TIMES, field)?;
        }
        _ if kind == action::Kind::Tween => {
            let target = payload(heap, value, TARGET)?;
            let goals = payload(heap, value, GOALS)?;
            if let Some(name) = name.strip_prefix("from.") {
                let expected = stage.resource_property_index(heap, target, name)?;
                let len = heap.sequence_len(goals)?;
                if len < action::GOAL_LEN as u32
                    || heap
                        .sequence_get(goals, len - action::GOAL_LEN as u32)?
                        .as_int()
                        != Some(i64::from(expected))
                {
                    return Err(TypedDataError::Contract(
                        "tween from requires its matching goal",
                    ));
                }
                if present(field) {
                    stage::number(field)?;
                    heap.set_field(goals, len - 2, field)?;
                }
                return Ok(());
            }
            let name = name
                .strip_prefix("goal.")
                .ok_or(TypedDataError::Contract("unknown tween resource field"))?;
            let field_index = stage.resource_property_index(heap, target, name)?;
            let end = stage::number(field)?;
            heap.validate_property_write(
                PropertyId {
                    host: target,
                    field: field_index,
                },
                field,
            )?;
            for v in [
                Value::int(i64::from(field_index)),
                Value::NULL,
                Value::float(end),
            ] {
                heap.array_push(goals, v)?;
            }
        }
        _ => return Err(TypedDataError::Contract("unknown action resource field")),
    }
    Ok(())
}
pub(super) fn finish(heap: &Heap, value: Value, cursor: &mut u32) -> Result<bool, TypedDataError> {
    let kind = action::kind(heap, value)?;
    if kind.is_leaf() {
        if kind == action::Kind::Clock {
            stage::positive(payload(heap, value, action::TIMES)?)?;
        } else {
            stage::nonnegative(payload(heap, value, DURATION)?)?;
        }
        if kind == action::Kind::Tween {
            let easing = payload(heap, value, EASING)?;
            if !easing.as_int().is_some_and(|v| (0..=21).contains(&v))
                && aside_std::curve_key_count(heap, easing).is_err()
            {
                return Err(TypedDataError::Contract("logical tween easing is missing"));
            }
        }
        return Ok(true);
    }
    let children = payload(heap, value, GOALS)?;
    let length = heap.sequence_len(children)?;
    if kind == action::Kind::Repeat && length != 1 {
        return Err(TypedDataError::Contract("repeat requires one child action"));
    }
    if *cursor < length {
        tagged(heap, heap.sequence_get(children, *cursor)?, ACTION)?;
        *cursor += 1;
        return Ok(false);
    }
    Ok(true)
}
