//! Immutable scalar curves. Construction lives in the native continuation and
//! the compiled keys live in the ordinary, traced VM heap.
use crate::{NativeError, StandardLibrary};
use aside_host::aside_vm::{
    Heap, HeapError, ObjectKind, Value,
    bytecode::{BuiltinType, Effects},
    executor::{Continuation, FaultRetention, HostResponse, HostTicket, NativeInput, Vm},
};
use aside_host::{CallContext, NativeBinding, NativeContract, NativeParameter, native};

const TAG: i64 = 0x4355525645;
// A key fits the VM's inline payload: time, value, mode, a/b/c, left/right.
const LEFT: u32 = 6;
const RIGHT: u32 = 7;
const LINEAR: i64 = 0;
const STEP: i64 = 1;
const CUBIC: i64 = 2;

aside_host::native_documentation! {
    /// Construct an immutable scalar curve from time-ordered keys.
    /// Each key has `time` and `value`; `mode` selects `linear` (default), `step`,
    /// or `cubic`. Cubic keys accept `in_tangent` and `out_tangent` slopes.
    /// Times must be nondecreasing; repeated times keep the last key's value.
    /// Sampling outside the key range returns the
    /// nearest endpoint. The curve owns its keys and can be shared by tweens.
    ///
    /// ```aside
    /// use std::math::curve;
    /// fn main() {
    ///     let fade = curve([{time: 0, value: 0}, {time: 1, value: 1}]);
    ///     if fade.sample(0.5) != 0.5 { throw "curve sample"; }
    /// }
    /// ```
    const DOCUMENTATION;
}

pub(super) fn binding() -> NativeBinding<StandardLibrary, NativeError> {
    NativeBinding {
        documentation: DOCUMENTATION,
        contract: NativeContract {
            symbol: "std::math::curve".into(),
            abi_version: 1,
            parameters: vec![NativeParameter {
                name: "keys".into(),
                ty: BuiltinType::Any,
                default: None,
            }],
            rest: None,
            result: BuiltinType::Any,
            effects: Effects(Effects::ALLOCATE),
            required_capabilities: vec![],
        },
        invoke: |host, vm, ticket| construct(host, vm, ticket, 1).map(|_| ()),
        source_signature: |contract| {
            let mut signature = contract.external().signature.expect("curve signature");
            signature.result = aside_host::source::named(crate::source::CURVE);
            signature.parameters[0].ty = crate::source::sequence();
            signature
        },
    }
}

fn finite(value: Value) -> Result<f64, NativeError> {
    let number = super::math::Number::read(value)?.0;
    if !number.is_finite() {
        return Err(NativeError::Input("curve numbers must be finite"));
    }
    Ok(number)
}

fn text_is(heap: &Heap, value: Value, expected: &str) -> bool {
    heap.info(value)
        .is_ok_and(|info| info.kind == ObjectKind::String && info.len == expected.len() as u32)
        && heap
            .byte_segment(value, 0)
            .is_ok_and(|bytes| bytes == expected.as_bytes())
}

struct Key {
    time: f64,
    value: f64,
    mode: i64,
    incoming: f64,
    outgoing: f64,
}

fn read_key(heap: &mut Heap, record: Value) -> Result<Key, NativeError> {
    let count = heap.table_len(record)?;
    if count > 5 {
        return Err(NativeError::Input(
            "curve key fields are time, value, mode, in_tangent and out_tangent",
        ));
    }
    heap.observe_read(record, None)?;
    let mut time = None;
    let mut value = None;
    let mut mode = LINEAR;
    let mut incoming = 0.;
    let mut outgoing = 0.;
    for index in 0..count {
        let (name, field) = heap.table_entry(record, index)?;
        if text_is(heap, name, "time") {
            time = Some(finite(field)?);
        } else if text_is(heap, name, "value") {
            value = Some(finite(field)?);
        } else if text_is(heap, name, "mode") {
            mode = if text_is(heap, field, "linear") {
                LINEAR
            } else if text_is(heap, field, "step") {
                STEP
            } else if text_is(heap, field, "cubic") {
                CUBIC
            } else {
                return Err(NativeError::Input(
                    "curve mode must be linear, step or cubic",
                ));
            };
        } else if text_is(heap, name, "in_tangent") {
            incoming = finite(field)?;
        } else if text_is(heap, name, "out_tangent") {
            outgoing = finite(field)?;
        } else {
            return Err(NativeError::Input(
                "curve key fields are time, value, mode, in_tangent and out_tangent",
            ));
        }
    }
    Ok(Key {
        time: time.ok_or(NativeError::Input("curve key requires time"))?,
        value: value.ok_or(NativeError::Input("curve key requires value"))?,
        mode,
        incoming,
        outgoing,
    })
}

fn number(heap: &Heap, keys: Value, index: u32) -> Result<f64, HeapError> {
    Ok(heap.sequence_get(keys, index)?.as_float().unwrap())
}

pub(super) fn construct(
    _host: &mut StandardLibrary,
    vm: &mut Vm,
    ticket: HostTicket,
    work: u32,
) -> Result<u32, NativeError> {
    // The source stays rooted by the native arguments; only numeric output and
    // the version/cursor cross construction slices.
    let source = vm.host_arguments(ticket)?[0];
    let info = vm.heap().info(source)?;
    if matches!(ticket.input, NativeInput::Entry)
        && (!matches!(info.kind, ObjectKind::Array | ObjectKind::Tuple) || info.len == 0)
    {
        return Err(NativeError::Input(
            "curve keys must be a nonempty array or tuple",
        ));
    }
    let (keys, version, mut outgoing, mut pending) = match ticket.input {
        NativeInput::Entry => {
            vm.heap_mut().observe_read(source, None)?;
            (
                vm.heap_mut().alloc_array(&[])?,
                info.content_version,
                0.,
                Value::NULL,
            )
        }
        NativeInput::Continue { .. } => {
            let state = vm.host_continuation(ticket)?;
            (
                state[0],
                state[1].as_int().unwrap() as u64,
                state[2].as_float().unwrap(),
                state[3],
            )
        }
        _ => return Err(aside_host::Error::Arguments.into()),
    };
    if info.content_version != version {
        return Err(NativeError::ConcurrentModification);
    }
    let mut index = vm.heap().sequence_len(keys)?;
    let mut used = 0;
    while index < info.len && used < work.max(1) {
        let record = vm.heap().sequence_get(source, index)?;
        let key = read_key(vm.heap_mut(), record)?;
        if index > 0 {
            let previous = vm.heap().sequence_get(keys, index - 1)?;
            let before = number(vm.heap(), previous, 0)?;
            if key.time < before {
                return Err(NativeError::Input("curve key times must be nondecreasing"));
            }
            if key.time > before {
                let value = number(vm.heap(), previous, 1)?;
                if vm.heap().sequence_get(previous, 2)?.as_int() == Some(CUBIC) {
                    let duration = key.time - before;
                    let left = outgoing * duration;
                    let right = key.incoming * duration;
                    let difference = value - key.value;
                    for (offset, coefficient) in [
                        (3, 2. * difference + left + right),
                        (4, -3. * difference - 2. * left - right),
                        (5, left),
                    ] {
                        vm.heap_mut()
                            .set_field(previous, offset, Value::float(coefficient))?;
                    }
                }
            }
        }
        let compiled = vm.heap_mut().alloc_array(&[
            Value::float(key.time),
            Value::float(key.value),
            Value::int(key.mode),
            Value::float(0.),
            Value::float(0.),
            Value::float(0.),
            Value::NULL,
            Value::NULL,
        ])?;
        vm.heap_mut().array_push(keys, compiled)?;
        outgoing = key.outgoing;
        index += 1;
        used += 1;
    }
    if index == info.len && pending.same_bits(Value::NULL) {
        pending = vm
            .heap_mut()
            .alloc_array(&[Value::int(0), Value::int(i64::from(info.len))])?;
    }
    while !pending.same_bits(Value::NULL)
        && vm.heap().sequence_len(pending)? > 0
        && used < work.max(1)
    {
        let end = vm.heap_mut().array_pop(pending)?.unwrap().as_int().unwrap() as u32;
        let start = vm.heap_mut().array_pop(pending)?.unwrap().as_int().unwrap() as u32;
        let middle = start + (end - start) / 2;
        let node = vm.heap().sequence_get(keys, middle)?;
        for (low, high, side) in [(start, middle, LEFT), (middle + 1, end, RIGHT)] {
            if low < high {
                let child = vm.heap().sequence_get(keys, low + (high - low) / 2)?;
                vm.heap_mut().set_field(node, side, child)?;
                vm.heap_mut()
                    .array_push(pending, Value::int(i64::from(low)))?;
                vm.heap_mut()
                    .array_push(pending, Value::int(i64::from(high)))?;
            }
        }
        used += 1;
    }
    if !pending.same_bits(Value::NULL) && vm.heap().sequence_len(pending)? == 0 {
        let root = vm.heap().sequence_get(keys, info.len / 2)?;
        let first = vm.heap().sequence_get(keys, 0)?;
        let fields = vm.heap_mut().alloc_table()?;
        let curve = vm
            .heap_mut()
            .alloc_immutable_host(fields, &[Value::int(TAG), root, first])?;
        let ty = vm.heap().host_type(crate::source::CURVE)?;
        vm.heap_mut().set_host_type(curve, ty)?;
        vm.respond(ticket, HostResponse::Ready(curve))?;
    } else {
        vm.respond(
            ticket,
            HostResponse::Continue(Continuation {
                version: 1,
                stage: 0,
                values: &[
                    keys,
                    Value::int(version as i64),
                    Value::float(outgoing),
                    pending,
                ],
                faults: FaultRetention::Preserve,
            }),
        )?;
    }
    Ok(used)
}

/// Sample this curve at `time`. Endpoints clamp; inner segments use the left
/// key's interpolation mode. Sampling performs a logarithmic lookup.
#[native(symbol = "std::math::curve::sample", effects = Effects(0))]
pub(super) fn sample(
    #[aside(context)] context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = aside_host::source::named(crate::source::CURVE))] receiver: Value,
    #[aside(doc_type = aside_host::source::number())] time: Value,
) -> Result<f64, NativeError> {
    let time = finite(time)?;
    Ok(sample_curve(context.vm.heap(), receiver, time)?)
}

/// Samples an immutable standard-library curve without allocating or invoking
/// script. The caller retains the curve through the ordinary VM roots.
pub fn sample_curve(heap: &Heap, receiver: Value, time: f64) -> Result<f64, HeapError> {
    if !time.is_finite() {
        return Err(HeapError::TypeMismatch);
    }
    if heap.host_payload(receiver, 0)?.as_int() != Some(TAG) {
        return Err(HeapError::TypeMismatch);
    }
    let mut node = heap.host_payload(receiver, 1)?;
    let mut before = Value::NULL;
    let mut after = Value::NULL;
    // Upper bound picks the last key at a repeated time.
    while !node.same_bits(Value::NULL) {
        if number(heap, node, 0)? <= time {
            before = node;
            node = heap.sequence_get(node, RIGHT)?;
        } else {
            after = node;
            node = heap.sequence_get(node, LEFT)?;
        }
    }
    if before.same_bits(Value::NULL) {
        return number(heap, heap.host_payload(receiver, 2)?, 1);
    }
    let value = number(heap, before, 1)?;
    if after.same_bits(Value::NULL) {
        return Ok(value);
    }
    let start = number(heap, before, 0)?;
    if time == start {
        return Ok(value);
    }
    let mode = heap.sequence_get(before, 2)?.as_int().unwrap();
    if mode == STEP {
        return Ok(value);
    }
    let duration = number(heap, after, 0)? - start;
    let progress = (time - start) / duration;
    if mode == LINEAR {
        let end = number(heap, after, 1)?;
        return Ok(value * (1. - progress) + end * progress);
    }
    let a = number(heap, before, 3)?;
    let b = number(heap, before, 4)?;
    let c = number(heap, before, 5)?;
    Ok(((a * progress + b) * progress + c) * progress + value)
}
