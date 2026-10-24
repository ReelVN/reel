//! Share immutable operand metadata within each function, preserving every instruction.
use super::{Error, budget::Budget};
use aside_vm::bytecode as bc;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::mem::size_of;

#[derive(Hash)]
enum Key<'a> {
    Global(bc::ModuleId, bc::GlobalId),
    Call(&'a bc::CallDesc),
    Sequence(bc::RegWindow),
    Table(&'a [[bc::Reg; 2]]),
    TableSources(&'a [bc::TableSource]),
    Super(bc::Reg, bc::SymbolId),
    Property(bc::Reg, bool),
    Spawn(&'a bc::CallDesc, bc::Reg),
}

fn key(descriptor: &bc::Descriptor) -> Option<Key<'_>> {
    Some(match descriptor {
        bc::Descriptor::Global { module, global } => Key::Global(*module, *global),
        bc::Descriptor::Call(call) => Key::Call(call),
        bc::Descriptor::Sequence(window) => Key::Sequence(*window),
        bc::Descriptor::Table(fields) => Key::Table(fields),
        bc::Descriptor::TableSources(fields) => Key::TableSources(fields),
        bc::Descriptor::Super { base, member } => Key::Super(*base, *member),
        bc::Descriptor::Property { key, writable } => Key::Property(*key, *writable),
        bc::Descriptor::Spawn { call, owner } => Key::Spawn(call, *owner),
        _ => return None,
    })
}

fn nested_len(descriptor: &bc::Descriptor) -> usize {
    match descriptor {
        bc::Descriptor::Call(call) | bc::Descriptor::Spawn { call, .. } => call.names.len(),
        bc::Descriptor::Table(fields) => fields.len(),
        bc::Descriptor::TableSources(fields) => fields.len(),
        _ => 0,
    }
}

fn nested_capacity(descriptor: &bc::Descriptor) -> usize {
    match descriptor {
        bc::Descriptor::Call(call) | bc::Descriptor::Spawn { call, .. } => {
            call.names.capacity() * size_of::<bc::NamedArg>()
        }
        bc::Descriptor::Table(fields) => fields.capacity() * size_of::<[bc::Reg; 2]>(),
        bc::Descriptor::TableSources(fields) => fields.capacity() * size_of::<bc::TableSource>(),
        _ => 0,
    }
}

pub(super) fn intern(package: &mut bc::BytecodePackage, b: &mut Budget) -> Result<(), Box<Error>> {
    let mut order = Vec::new();
    let mut remap = Vec::new();
    for function in &mut package.functions {
        let count = function.descriptors.len();
        if count < 2 {
            continue;
        }
        order.clear();
        for (index, descriptor) in function.descriptors.iter().enumerate() {
            b.work(1 + nested_len(descriptor) as u64)?;
            if let Some(key) = key(descriptor) {
                let mut hash = DefaultHasher::new();
                key.hash(&mut hash);
                b.push(&mut order, (hash.finish(), index))?;
            }
        }
        if order.len() < 2 {
            continue;
        }
        b.work(order.len() as u64 * (usize::BITS - order.len().leading_zeros()) as u64)?;
        order.sort_unstable();
        remap.clear();
        b.reserve(&mut remap, count)?;
        b.work(count as u64)?;
        remap.extend((0..count).map(|index| bc::DescriptorId(index as u32)));
        let mut group = 0;
        let mut duplicates = 0;
        for position in 1..order.len() {
            let (hash, index) = order[position];
            if hash != order[group].0 {
                group = position;
                continue;
            }
            // Hashes select candidates; equality preserves distinct call contracts,
            // argument order, registers and container construction inputs.
            for &(_, candidate) in &order[group..position] {
                b.work(1)?;
                if remap[candidate].index() != candidate {
                    continue;
                }
                b.work(nested_len(&function.descriptors[index]) as u64)?;
                if function.descriptors[index] == function.descriptors[candidate] {
                    remap[index] = bc::DescriptorId(candidate as u32);
                    duplicates += 1;
                    break;
                }
            }
        }
        if duplicates == 0 {
            continue;
        }
        b.work(count as u64 + function.code.len() as u64)?;
        let mut next = 0;
        for index in 0..count {
            let first = remap[index].index();
            remap[index] = if first == index {
                let id = bc::DescriptorId(next);
                next += 1;
                id
            } else {
                remap[first]
            };
        }
        let mut retained = Vec::new();
        b.reserve(&mut retained, count - duplicates)?;
        let previous = std::mem::take(&mut function.descriptors);
        let previous_capacity = previous.capacity() * size_of::<bc::Descriptor>();
        for (index, descriptor) in previous.into_iter().enumerate() {
            if remap[index].index() == retained.len() {
                retained.push(descriptor);
            } else {
                b.release_bytes(nested_capacity(&descriptor));
            }
        }
        b.release_bytes(previous_capacity);
        function.descriptors = retained;
        for instruction in &mut function.code {
            let spec = bc::instruction_spec(instruction.opcode).expect("emitted opcode");
            for (kind, operand) in spec.operands.into_iter().zip(&mut instruction.operands) {
                if matches!(kind, bc::OperandKind::Descriptor(_)) {
                    *operand = remap[*operand as usize].0;
                }
            }
        }
    }
    b.release(order);
    b.release(remap);
    Ok(())
}
