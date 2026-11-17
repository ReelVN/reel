//! Per-occurrence progress. Structure is rebuilt by the ordinary action builder.
use super::*;
const TAG: i64 = -1003;
const ACTION_SLOT: u32 = 1;
const PHASE: u32 = 2;
const ELAPSED: u32 = 3;
const ITERATION: u32 = 4;
const REMAINING: u32 = 5;
const STARTS: u32 = 6;

pub(super) fn source(heap: &Heap, value: Value) -> bool {
    let Ok(length) = heap.sequence_len(value) else {
        return false;
    };
    if length != graph::Slot::ChildCount as u32 + 1 {
        return false;
    }
    let Ok(activity) = graph::get(heap, value, graph::Slot::Activity) else {
        return false;
    };
    if !is(heap, activity, ACTIVITY) {
        return false;
    }
    let Ok(index) = graph::get(heap, value, graph::Slot::Index) else {
        return false;
    };
    let Some(index) = index.as_int() else {
        return false;
    };
    index >= 0 && graph::node(heap, activity, index).is_ok_and(|node| node.same_bits(value))
}
pub(super) fn restored(heap: &Heap, value: Value) -> bool {
    heap.sequence_get(value, 0)
        .is_ok_and(|tag| tag.as_int() == Some(TAG))
}
pub(super) fn count(heap: &Heap, value: Value) -> Result<u32, AdapterError> {
    if graph::phase(heap, value)? == graph::Phase::Starting {
        return Err(AdapterError::Input(
            "activity is between committed execution phases",
        ));
    }
    Ok(5 + if graph::phase(heap, value)? == graph::Phase::Active {
        heap.sequence_len(graph::get(heap, value, graph::Slot::Tracks)?)? / graph::Track::LEN as u32
    } else {
        0
    })
}
pub(super) fn field(
    activities: &Activities,
    heap: &Heap,
    stage: &StageBinding,
    value: Value,
    index: u32,
) -> Result<(Cow<'static, str>, Value), TypedDataError> {
    Ok(match index {
        0 => (
            "action".into(),
            graph::get(heap, value, graph::Slot::Action)?,
        ),
        1 => ("phase".into(), graph::get(heap, value, graph::Slot::Phase)?),
        2 => {
            let activity = graph::get(heap, value, graph::Slot::Activity)?;
            let now = if state(heap, activity)? == State::Paused {
                root_get(heap, activity, RootSlot::PausedAt)?
                    .as_float()
                    .unwrap()
            } else {
                activities.time
            };
            let start = graph::get(heap, value, graph::Slot::StartedAt)?
                .as_float()
                .unwrap_or(now);
            ("elapsed".into(), Value::float((now - start).max(0.)))
        }
        3 => (
            "iteration".into(),
            graph::get(heap, value, graph::Slot::Iteration)?,
        ),
        4 => (
            "remaining".into(),
            graph::get(heap, value, graph::Slot::Remaining)?,
        ),
        _ => {
            let tracks = graph::get(heap, value, graph::Slot::Tracks)?;
            let offset = (index - 5) * graph::Track::LEN as u32;
            let target = payload(heap, graph::get(heap, value, graph::Slot::Action)?, TARGET)?;
            let field = graph::track(heap, tracks, offset, graph::Track::Field)?
                .as_int()
                .unwrap() as u32;
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
            (
                format!("start.{name}").into(),
                graph::track(heap, tracks, offset, graph::Track::Start)?,
            )
        }
    })
}
pub(super) fn allocate(heap: &mut Heap) -> Result<Value, TypedDataError> {
    let starts = heap.alloc_table()?;
    Ok(heap.alloc_array(&[
        Value::int(TAG),
        Value::NULL,
        Value::NULL,
        Value::NULL,
        Value::NULL,
        Value::NULL,
        starts,
    ])?)
}
pub(super) fn connect(
    heap: &mut Heap,
    value: Value,
    name: &str,
    field: Value,
) -> Result<(), TypedDataError> {
    let slot = match name {
        "action" => ACTION_SLOT,
        "phase" => {
            if !matches!(field.as_int(), Some(0 | 2 | 3)) {
                return Err(TypedDataError::Contract(
                    "activity is between committed execution phases",
                ));
            }
            PHASE
        }
        "elapsed" => {
            stage::nonnegative(field)?;
            ELAPSED
        }
        "iteration" | "remaining" => {
            if field.tag() != ValueTag::Null && !field.as_int().is_some_and(|n| n >= 0) {
                return Err(TypedDataError::Contract(
                    "invalid action iteration or remaining count",
                ));
            }
            if name == "iteration" {
                ITERATION
            } else {
                REMAINING
            }
        }
        _ => {
            let name = name
                .strip_prefix("start.")
                .ok_or(TypedDataError::Contract("unknown action progress field"))?;
            stage::number(field)?;
            let key = heap.alloc_string(name)?;
            let starts = heap.sequence_get(value, STARTS)?;
            heap.table_store(starts, heap.table_len(starts)?, key, field)?;
            return Ok(());
        }
    };
    heap.set_field(value, slot, field)?;
    Ok(())
}
pub(super) fn apply(
    heap: &mut Heap,
    stage: &StageBinding,
    pending: &mut Pending,
    time: f64,
    activity: Value,
) -> Result<bool, TypedDataError> {
    let nodes = root_get(heap, activity, RootSlot::Graph)?;
    let length = heap.sequence_len(nodes)?;
    if pending.node == length && pending.progress.tag() == ValueTag::Null {
        return Ok(true);
    }
    if heap.sequence_len(pending.progress)? != length {
        return Err(TypedDataError::Contract(
            "saved progress does not match action occurrence count",
        ));
    }
    if pending.node == length {
        pending.progress = Value::NULL;
        return Ok(true);
    }
    let current = heap.sequence_get(nodes, pending.node)?;
    let saved = heap.sequence_get(pending.progress, pending.node)?;
    if !restored(heap, saved) {
        return Err(TypedDataError::Contract(
            "invalid action occurrence progress",
        ));
    }
    if !pending.node_started {
        let phase = heap.sequence_get(saved, PHASE)?.as_int();
        if !matches!(phase, Some(0 | 2 | 3)) {
            return Err(TypedDataError::Contract(
                "saved action occurrence phase is missing",
            ));
        }
        for slot in [ITERATION, REMAINING] {
            let value = heap.sequence_get(saved, slot)?;
            if value.tag() != ValueTag::Null && !value.as_int().is_some_and(|n| n >= 0) {
                return Err(TypedDataError::Contract(
                    "invalid action iteration or remaining count",
                ));
            }
            if phase != Some(0) && value.tag() == ValueTag::Null {
                return Err(TypedDataError::Contract(
                    "active action progress count is missing",
                ));
            }
        }
        let parent = graph::int(heap, current, graph::Slot::Parent)?;
        if phase == Some(2)
            && parent >= 0
            && graph::phase(heap, graph::node(heap, activity, parent)?)? != graph::Phase::Active
        {
            return Err(TypedDataError::Contract(
                "active action requires an active parent occurrence",
            ));
        }
        let child_count = graph::int(heap, current, graph::Slot::ChildCount)?;
        if heap
            .sequence_get(saved, REMAINING)?
            .as_int()
            .is_some_and(|remaining| remaining > child_count)
        {
            return Err(TypedDataError::Contract(
                "action remaining count exceeds its child occurrences",
            ));
        }
        if !heap.sequence_get(saved, ACTION_SLOT)?.same_bits(graph::get(
            heap,
            current,
            graph::Slot::Action,
        )?) {
            return Err(TypedDataError::Contract(
                "saved progress does not match action description",
            ));
        }
        for (destination, source) in [
            (graph::Slot::Phase, PHASE),
            (graph::Slot::Iteration, ITERATION),
            (graph::Slot::Remaining, REMAINING),
        ] {
            graph::set(
                heap,
                current,
                destination,
                heap.sequence_get(saved, source)?,
            )?;
        }
        let elapsed = stage::nonnegative(heap.sequence_get(saved, ELAPSED)?)?;
        graph::set(
            heap,
            current,
            graph::Slot::StartedAt,
            Value::float(time - elapsed),
        )?;
        pending.node_started = true;
        return Ok(false);
    }
    let tracks = graph::get(heap, current, graph::Slot::Tracks)?;
    if graph::phase(heap, current)? == graph::Phase::Active
        && pending.track < heap.sequence_len(tracks)?
    {
        let offset = pending.track;
        let starts = heap.sequence_get(saved, STARTS)?;
        if heap.table_len(starts)? != heap.sequence_len(tracks)? / graph::Track::LEN as u32 {
            return Err(TypedDataError::Contract("saved tween track count changed"));
        }
        let (name, start) = heap.table_entry(starts, offset / graph::Track::LEN as u32)?;
        let target = payload(
            heap,
            graph::get(heap, current, graph::Slot::Action)?,
            TARGET,
        )?;
        let field = graph::track(heap, tracks, offset, graph::Track::Field)?
            .as_int()
            .unwrap() as u32;
        let expected = stage
            .resource_property_name(
                heap,
                PropertyId {
                    host: target,
                    field,
                },
            )?
            .ok_or(TypedDataError::Contract("invalid restored tween property"))?;
        if crate::aside::text::read_string(heap, name, 256)? != expected {
            return Err(TypedDataError::Contract(
                "saved tween property order changed",
            ));
        }
        heap.set_field(tracks, offset + graph::Track::Start as u32, start)?;
        heap.set_field(
            tracks,
            offset + graph::Track::Version as u32,
            Value::int(heap.host_write_version(target, field)? as i64),
        )?;
        let (source, writable) = heap.property_source(PropertyId {
            host: target,
            field,
        })?;
        if !writable {
            return Err(HeapError::ReadOnly.into());
        }
        if present(heap.property_claim(source)?) {
            return Err(TypedDataError::Contract(
                "restored activities share a property claim",
            ));
        }
        heap.set_field(
            tracks,
            offset + graph::Track::SourceHost as u32,
            source.host,
        )?;
        heap.set_field(
            tracks,
            offset + graph::Track::SourceField as u32,
            Value::int(i64::from(source.field)),
        )?;
        heap.set_property_claim(source, current)?;
        pending.track += graph::Track::LEN as u32;
        return Ok(false);
    }
    pending.node += 1;
    pending.track = 0;
    pending.node_started = false;
    Ok(false)
}
