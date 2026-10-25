//! Share completed call contracts after defaults and constant references are final.
use super::{Error, budget::Budget};
use aside_vm::bytecode as bc;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::mem::size_of;

fn parameter_key(parameter: &bc::Parameter) -> (bc::SymbolId, bc::TypeId, bool, Option<(u8, u32)>) {
    (
        parameter.name,
        parameter.ty,
        parameter.mutable,
        parameter.default.map(|default| match default {
            bc::ParameterDefault::Constant(id) => (0, id.0),
            bc::ParameterDefault::Function(id) => (1, id.0),
        }),
    )
}

fn hash(signature: &bc::Signature) -> u64 {
    let mut hash = DefaultHasher::new();
    signature.parameters.len().hash(&mut hash);
    for parameter in &signature.parameters {
        parameter_key(parameter).hash(&mut hash);
    }
    signature.rest.as_ref().map(parameter_key).hash(&mut hash);
    signature.return_type.hash(&mut hash);
    signature.effects.0.hash(&mut hash);
    signature
        .property_bag_tail
        .as_ref()
        .map(|tail| {
            (
                tail.target_parameter,
                tail.capability,
                tail.require_writable,
                tail.require_animatable,
            )
        })
        .hash(&mut hash);
    hash.finish()
}

pub(super) fn intern(package: &mut bc::BytecodePackage, b: &mut Budget) -> Result<(), Box<Error>> {
    let count = package.signatures.len();
    if count < 2 {
        return Ok(());
    }
    let mut order = Vec::new();
    for (index, signature) in package.signatures.iter().enumerate() {
        b.work(1 + signature.parameters.len() as u64)?;
        b.push(&mut order, (hash(signature), index))?;
    }
    b.work(count as u64 * (usize::BITS - count.leading_zeros()) as u64)?;
    order.sort_unstable();
    let mut remap = b.filled(count, bc::SignatureId(0))?;
    for (index, id) in remap.iter_mut().enumerate() {
        *id = bc::SignatureId(index as u32);
    }
    let mut group = 0;
    let mut duplicates = 0;
    for position in 1..order.len() {
        let (hash, index) = order[position];
        if hash != order[group].0 {
            group = position;
            continue;
        }
        for &(_, candidate) in &order[group..position] {
            b.work(1)?;
            if remap[candidate].index() != candidate {
                continue;
            }
            b.work(package.signatures[index].parameters.len() as u64)?;
            // Hashes only select candidates. Full equality includes names,
            // default function identities, rest binding and every host constraint.
            if package.signatures[index] == package.signatures[candidate] {
                remap[index] = bc::SignatureId(candidate as u32);
                duplicates += 1;
                break;
            }
        }
    }
    b.release(order);
    if duplicates != 0 {
        b.work(count as u64 + package.functions.len() as u64 + package.natives.len() as u64)?;
        let mut next = 0;
        for index in 0..count {
            let first = remap[index].index();
            remap[index] = if first == index {
                let id = bc::SignatureId(next);
                next += 1;
                id
            } else {
                remap[first]
            };
        }
        let mut retained = Vec::new();
        b.reserve(&mut retained, count - duplicates)?;
        let previous = std::mem::take(&mut package.signatures);
        let previous_capacity = previous.capacity() * size_of::<bc::Signature>();
        for (index, signature) in previous.into_iter().enumerate() {
            if remap[index].index() == retained.len() {
                retained.push(signature);
            } else {
                b.release_bytes(signature.parameters.capacity() * size_of::<bc::Parameter>());
            }
        }
        b.release_bytes(previous_capacity);
        package.signatures = retained;
        for function in &mut package.functions {
            function.signature = remap[function.signature.index()];
        }
        for native in &mut package.natives {
            native.signature = remap[native.signature.index()];
        }
    }
    b.release(remap);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_contract_field_participates_and_native_references_follow_compaction() {
        let parameter = bc::Parameter {
            name: bc::SymbolId(1),
            ty: bc::TypeId(1),
            mutable: false,
            default: Some(bc::ParameterDefault::Constant(bc::ConstId(1))),
        };
        let base = bc::Signature {
            parameters: vec![parameter.clone()],
            rest: Some(parameter),
            return_type: bc::TypeId(1),
            effects: bc::Effects::ALL,
            property_bag_tail: Some(bc::PropertyBagTail {
                target_parameter: 0,
                capability: Some(bc::SymbolId(1)),
                require_writable: false,
                require_animatable: false,
            }),
        };
        let differences: &[fn(&mut bc::Signature)] = &[
            |s| s.parameters.clear(),
            |s| s.parameters[0].name = bc::SymbolId(2),
            |s| s.parameters[0].ty = bc::TypeId(2),
            |s| s.parameters[0].mutable = true,
            |s| s.parameters[0].default = None,
            |s| s.parameters[0].default = Some(bc::ParameterDefault::Constant(bc::ConstId(2))),
            |s| s.parameters[0].default = Some(bc::ParameterDefault::Function(bc::FunctionId(1))),
            |s| s.rest = None,
            |s| s.rest.as_mut().unwrap().name = bc::SymbolId(2),
            |s| s.rest.as_mut().unwrap().ty = bc::TypeId(2),
            |s| s.rest.as_mut().unwrap().mutable = true,
            |s| s.rest.as_mut().unwrap().default = None,
            |s| s.return_type = bc::TypeId(2),
            |s| s.effects = bc::Effects(0),
            |s| s.property_bag_tail = None,
            |s| s.property_bag_tail.as_mut().unwrap().target_parameter = 1,
            |s| s.property_bag_tail.as_mut().unwrap().capability = None,
            |s| s.property_bag_tail.as_mut().unwrap().capability = Some(bc::SymbolId(2)),
            |s| s.property_bag_tail.as_mut().unwrap().require_writable = true,
            |s| s.property_bag_tail.as_mut().unwrap().require_animatable = true,
        ];
        let mut unique = vec![base.clone()];
        for difference in differences {
            let mut signature = base.clone();
            difference(&mut signature);
            unique.push(signature);
        }
        let signatures: Vec<_> = unique.iter().chain(&unique).cloned().collect();
        let natives = (0..signatures.len())
            .map(|index| bc::NativeDef {
                symbol: bc::SymbolId(index as u32),
                signature: bc::SignatureId(index as u32),
                abi_version: 1,
                capabilities: vec![],
            })
            .collect();
        let mut package = bc::BytecodePackage {
            signatures,
            natives,
            ..Default::default()
        };
        let capacity = |signatures: &Vec<bc::Signature>| {
            signatures.capacity() * size_of::<bc::Signature>()
                + signatures
                    .iter()
                    .map(|s| s.parameters.capacity() * size_of::<bc::Parameter>())
                    .sum::<usize>()
        };
        let mut budget = Budget::new(super::super::Limits::default());
        budget.capacity(capacity(&package.signatures)).unwrap();
        intern(&mut package, &mut budget).unwrap();
        assert_eq!(package.signatures, unique);
        for (index, native) in package.natives.iter().enumerate() {
            assert_eq!(native.signature.index(), index % unique.len());
        }
        assert_eq!(budget.stats.capacity_bytes, capacity(&package.signatures));
    }
}
