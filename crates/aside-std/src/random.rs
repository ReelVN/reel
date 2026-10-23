use crate::{NativeError, StandardLibrary};
use aside_host::aside_vm::{Heap, HeapError, Value, bytecode::Effects};
use aside_host::source::named;
use aside_host::{CallContext, native};

const TAG: i64 = 0x4153494445524e47;
enum Slot {
    Tag,
    State,
}

pub(super) fn readonly(
    _: &aside_host::aside_vm::PropertyRead<'_>,
    _: Value,
    _: u32,
    _: Value,
) -> Result<(), HeapError> {
    Err(HeapError::ReadOnly)
}

/// Create an independent random stream. Supplying an integer seed makes its
/// sequence repeatable; omitting the seed chooses a new sequence. Saving and
/// restoring the VM preserves each stream's position. The embedding host supplies
/// omitted seeds through its standard-library environment.
///
/// ```aside
/// fn main() {
///     let first = random(42);
///     let second = random(42);
///     if first.int(0, 100) != second.int(0, 100) { throw "seed"; }
/// }
/// ```
#[native(symbol = "std::random", effects = Effects(Effects::ALLOCATE | Effects::HOST), doc_result = named(crate::source::RANDOM))]
pub(super) fn random(
    #[aside(context)] context: &mut CallContext<'_, StandardLibrary>,
    #[aside(default = null)] seed: Option<i64>,
) -> Result<Value, NativeError> {
    let state = seed.unwrap_or_else(|| context.host.environment.random_seed() as i64);
    let vm = &mut context.vm;
    let fields = vm.heap_mut().alloc_table()?;
    let object = vm.heap_mut().alloc_host(
        fields,
        &[Value::int(TAG), Value::int(state)],
        Some(readonly),
    )?;
    let ty = vm.heap().host_type(crate::source::RANDOM)?;
    vm.heap_mut().set_host_type(object, ty)?;
    Ok(object)
}

fn generator(heap: &Heap, receiver: Value) -> Result<fastrand::Rng, NativeError> {
    if heap.host_payload(receiver, Slot::Tag as u32)?.as_int() != Some(TAG) {
        return Err(NativeError::Input("expected a random stream"));
    }
    let state = heap
        .host_payload(receiver, Slot::State as u32)?
        .as_int()
        .ok_or(HeapError::TypeMismatch)?;
    Ok(fastrand::Rng::with_seed(state as u64))
}

fn commit(heap: &mut Heap, receiver: Value, generator: fastrand::Rng) -> Result<(), NativeError> {
    heap.set_host_payload(
        receiver,
        Slot::State as u32,
        Value::int(generator.get_seed() as i64),
    )?;
    Ok(())
}

/// Draw an integer in `[min, max)`. Requires `min < max` and advances this stream.
#[native(symbol = "std::random::int", effects = Effects(Effects::WRITE))]
pub(super) fn int(
    #[aside(context)] context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = named(crate::source::RANDOM))] receiver: Value,
    min: i64,
    max: i64,
) -> Result<i64, NativeError> {
    if min >= max {
        return Err(NativeError::Input("random.int requires min < max"));
    }
    let mut generator = generator(context.vm.heap(), receiver)?;
    let value = generator.i64(min..max);
    commit(context.vm.heap_mut(), receiver, generator)?;
    Ok(value)
}

/// Draw a floating-point number in `[0, 1)` and advance this stream.
#[native(symbol = "std::random::float", effects = Effects(Effects::WRITE))]
pub(super) fn float(
    #[aside(context)] context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = named(crate::source::RANDOM))] receiver: Value,
) -> Result<f64, NativeError> {
    let mut generator = generator(context.vm.heap(), receiver)?;
    let value = generator.f64();
    commit(context.vm.heap_mut(), receiver, generator)?;
    Ok(value)
}

/// Select one element from a nonempty array or tuple and advance this stream.
/// The returned value shares the selected element's identity.
#[native(symbol = "std::random::pick", effects = Effects(Effects::WRITE))]
pub(super) fn pick(
    #[aside(context)] context: &mut CallContext<'_, StandardLibrary>,
    #[aside(doc_type = named(crate::source::RANDOM))] receiver: Value,
    #[aside(doc_type = crate::source::sequence())] items: Value,
) -> Result<Value, NativeError> {
    let count = context.vm.heap().sequence_len(items)?;
    if count == 0 {
        return Err(NativeError::Input(
            "random.pick requires a nonempty array or tuple",
        ));
    }
    let mut generator = generator(context.vm.heap(), receiver)?;
    let value = context
        .vm
        .heap()
        .sequence_get(items, generator.u32(0..count))?;
    commit(context.vm.heap_mut(), receiver, generator)?;
    Ok(value)
}
