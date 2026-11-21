//! Intern type shapes once when a package uses invariant array contracts.
use super::*;
use std::collections::HashMap;

pub(super) fn build(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
    usage: &HeapBudgetUsage,
) -> Result<ChargedVec<TypeId>, VmError> {
    if !p.types.iter().any(|ty| matches!(ty, TypeDef::Array(_))) {
        return Ok(ChargedVec::new(usage, 0)?);
    }
    let scratch = p.types.iter().try_fold(0u64, |sum, ty| {
        let owned = match ty {
            TypeDef::Tuple(items) | TypeDef::Union(items) => items.len() * size_of::<TypeId>(),
            TypeDef::Record(fields) => fields.len() * size_of::<RecordFieldType>(),
            TypeDef::Function { parameters, .. } => parameters.len() * size_of::<TypeId>(),
            TypeDef::StringLiteral(text) => text.len(),
            _ => 0,
        };
        sum.checked_add((2 * (size_of::<TypeDef>() + size_of::<TypeId>()) + owned) as u64)
            .ok_or(VmError::Limit)
    })?;
    let _scratch = usage.reserve(scratch)?;
    let mut canonical = ChargedVec::new(usage, p.types.len())?;
    let mut keys = HashMap::new();
    keys.try_reserve(p.types.len())
        .map_err(|_| VmError::Limit)?;
    for (index, ty) in p.types.iter().enumerate() {
        let id = TypeId(index as u32);
        let child = |item: TypeId| canonical.data[item.index()];
        let mut key = ty.clone();
        match &mut key {
            TypeDef::Array(item) => *item = child(*item),
            TypeDef::Tuple(items) | TypeDef::Union(items) => {
                for item in items {
                    *item = child(*item);
                }
                if let TypeDef::Union(items) = &mut key {
                    items.sort_unstable_by_key(|id| id.0);
                    items.dedup();
                    if let [only] = items.as_slice() {
                        canonical.data.push(*only);
                        continue;
                    }
                }
            }
            TypeDef::Record(fields) => {
                for field in &mut *fields {
                    field.ty = child(field.ty);
                }
                fields.sort_unstable_by_key(|field| field.name.0);
            }
            TypeDef::Function {
                parameters,
                return_type,
            } => {
                for item in parameters {
                    *item = child(*item);
                }
                *return_type = child(*return_type);
            }
            _ => {}
        }
        canonical.data.push(*keys.entry(key).or_insert(id));
    }
    Ok(canonical)
}
