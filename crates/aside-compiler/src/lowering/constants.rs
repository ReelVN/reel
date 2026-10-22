//! Intern text and scalar values without merging identity-bearing object constants.
use super::{Error, budget::Budget, build};
use aside_vm::bytecode as bc;

pub(super) fn intern(package: &mut bc::BytecodePackage, b: &mut Budget) -> Result<(), Box<Error>> {
    let mut names = Vec::new();
    let mut scalars = Vec::new();
    for (index, constant) in package.constants.iter().enumerate() {
        b.work(1)?;
        if let bc::Const::Utf8(value) = constant {
            b.push(&mut names, (value.as_str(), Some(index)))?;
        } else {
            let key = match constant {
                bc::Const::Null => Some((0u8, 0u64)),
                bc::Const::Bool(value) => Some((1, u64::from(*value))),
                bc::Const::Int(value) => Some((2, *value as u64)),
                // Preserve -0 and NaN payloads; constant identity uses exact bits.
                bc::Const::FloatBits(value) => Some((3, *value)),
                _ => None,
            };
            if let Some(key) = key {
                b.push(&mut scalars, (key, index))?;
            }
        }
    }
    build::sort_names(&mut names, b)?;
    let mut representative = b.filled(package.constants.len(), 0usize)?;
    for (index, value) in representative.iter_mut().enumerate() {
        *value = index;
    }
    let mut start = 0;
    while start < names.len() {
        let mut end = start + 1;
        let mut first = names[start].1.unwrap();
        while end < names.len() && b.compare(names[start].0, names[end].0)?.is_eq() {
            first = first.min(names[end].1.unwrap());
            end += 1;
        }
        for &(_, index) in &names[start..end] {
            representative[index.unwrap()] = first;
        }
        start = end;
    }
    b.release(names);
    b.work(scalars.len() as u64 * (usize::BITS - scalars.len().leading_zeros()) as u64)?;
    scalars.sort_unstable();
    let mut previous = None;
    for (key, index) in &scalars {
        let first = match previous {
            Some((old, first)) if old == *key => first,
            _ => {
                previous = Some((*key, *index));
                *index
            }
        };
        representative[*index] = first;
    }
    b.release(scalars);

    let mut remap = b.filled(package.constants.len(), bc::ConstId(0))?;
    let mut next = 0;
    for (index, &first) in representative.iter().enumerate() {
        b.work(1)?;
        remap[index] = if first == index {
            let id = bc::ConstId(next);
            next += 1;
            id
        } else {
            // The earliest occurrence keeps backward tuple references valid.
            remap[first]
        };
    }
    let mut index = 0;
    package.constants.retain(|value| {
        let keep = representative[index] == index;
        if !keep && let bc::Const::Utf8(value) = value {
            b.release_bytes(value.capacity());
        }
        index += 1;
        keep
    });
    b.release(representative);
    for constant in &mut package.constants {
        if let bc::Const::Tuple(items) = constant {
            b.work(items.len() as u64)?;
            for item in items {
                *item = remap[item.index()];
            }
        }
    }
    for signature in &mut package.signatures {
        b.work(signature.parameters.len() as u64)?;
        for parameter in &mut signature.parameters {
            if let Some(bc::ParameterDefault::Constant(id)) = &mut parameter.default {
                *id = remap[id.index()];
            }
        }
    }
    for function in &mut package.functions {
        b.work(function.code.len() as u64 + function.descriptors.len() as u64)?;
        for instruction in &mut function.code {
            let kinds = bc::instruction_spec(instruction.opcode)
                .expect("emitted opcode")
                .operands;
            for (kind, operand) in kinds.into_iter().zip(&mut instruction.operands) {
                if kind == bc::OperandKind::Const {
                    *operand = remap[*operand as usize].0;
                }
            }
        }
        for descriptor in &mut function.descriptors {
            if let bc::Descriptor::Pattern(pattern) = descriptor {
                b.work(pattern.nodes.len() as u64)?;
                for node in &mut pattern.nodes {
                    if let bc::PatternNode::Literal(id) = node {
                        *id = remap[id.index()];
                    }
                }
            }
        }
    }
    b.release(remap);
    Ok(())
}
