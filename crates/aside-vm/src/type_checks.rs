//! Shared structural contracts. The verified type DAG bounds work and scratch.
use crate::{
    Heap, HeapBudgetUsage, HeapError, Value, bytecode::*, executor::VmError,
    root_ranges::ChargedVec,
};
use std::cell::RefCell;
mod array_identity;

#[derive(Clone, Copy)]
struct Cursor {
    ty: TypeId,
    value: Value,
    next: u32,
    remaining_fields: u32,
}

pub(crate) struct TypeChecks {
    work: ChargedVec<u64>,
    signature_work: ChargedVec<u64>,
    stack: RefCell<ChargedVec<Cursor>>,
    symbols: ChargedVec<SymbolId>,
    array_identities: ChargedVec<TypeId>,
    pub(crate) host_types: crate::host_types::HostTypes,
}

impl TypeChecks {
    pub(super) fn new(
        p: &BytecodePackage<
            impl crate::bytecode::InstructionSequence,
            impl crate::bytecode::SourceMapSequence,
            impl crate::bytecode::DescriptorSequence,
        >,
        usage: &HeapBudgetUsage,
        registrations: &[crate::HostTypeSupport],
    ) -> Result<Self, VmError> {
        let host_types = crate::host_types::HostTypes::new(p, registrations, usage)?;
        let mut work = ChargedVec::new(usage, p.types.len())?;
        let mut depths = ChargedVec::new(usage, p.types.len())?;
        for ty in &p.types {
            let (cost, depth) = match ty {
                TypeDef::Builtin(_) => (1, 0usize),
                TypeDef::Array(_) => (2, 0usize),
                TypeDef::Function { .. } => (2, 0usize),
                TypeDef::Named { .. } => (3 + host_types.work, 0usize),
                TypeDef::StringLiteral(text) => (1 + text.len() as u64, 0),
                TypeDef::Record(fields) => {
                    let mut cost = 3u64;
                    let mut depth = 0;
                    for field in fields {
                        let search = (fields.len() as u64)
                            .checked_mul(3 + p.symbols[field.name.index()].len() as u64)
                            .ok_or(VmError::Limit)?;
                        cost = cost
                            .checked_add(search)
                            .and_then(|cost| cost.checked_add(work.data[field.ty.index()]))
                            .ok_or(VmError::Limit)?;
                        depth = depth.max(depths.data[field.ty.index()] + 1);
                    }
                    (cost, depth)
                }
                TypeDef::Tuple(items) | TypeDef::Union(items) => {
                    let mut cost = 3u64;
                    let mut depth = 0;
                    for id in items {
                        cost = cost
                            .checked_add(work.data[id.index()])
                            .and_then(|n| n.checked_add(2))
                            .ok_or(VmError::Limit)?;
                        depth = depth.max(depths.data[id.index()] + 1);
                    }
                    (cost, depth)
                }
            };
            work.data.push(cost);
            depths.data.push(depth);
        }
        let mut signature_work = ChargedVec::new(usage, p.signatures.len())?;
        for sig in &p.signatures {
            signature_work.data.push(
                sig.parameters
                    .iter()
                    .chain(sig.rest.iter())
                    .map(|p| work.data[p.ty.index()] - 1)
                    .max()
                    .unwrap_or(0),
            );
        }
        let stack = ChargedVec::new(usage, depths.data.iter().copied().max().unwrap_or(0))?;
        let mut symbols = ChargedVec::new(usage, p.symbols.len())?;
        symbols
            .data
            .extend((0..p.symbols.len()).map(|i| SymbolId(i as u32)));
        symbols
            .data
            .sort_unstable_by(|a, b| p.symbols[a.index()].cmp(&p.symbols[b.index()]));
        Ok(Self {
            work,
            signature_work,
            stack: RefCell::new(stack),
            symbols,
            array_identities: array_identity::build(p, usage)?,
            host_types,
        })
    }

    pub(super) fn extra(&self, ty: TypeId) -> u64 {
        self.work.data[ty.index()] - 1
    }
    pub(crate) fn same_type(&self, left: TypeId, right: TypeId) -> bool {
        left == right
            || self
                .array_identities
                .data
                .get(left.index())
                .is_some_and(|left| Some(left) == self.array_identities.data.get(right.index()))
    }
    pub(super) fn argument_extra(&self, sig: SignatureId) -> u64 {
        self.signature_work.data[sig.index()]
    }

    pub(crate) fn symbol_work(&self, bytes: u32) -> u64 {
        u64::from(self.symbols.data.len().checked_ilog2().unwrap_or(0) + 1) * (u64::from(bytes) + 1)
    }

    pub(crate) fn symbol_str(
        &self,
        p: &BytecodePackage<
            impl crate::bytecode::InstructionSequence,
            impl crate::bytecode::SourceMapSequence,
            impl crate::bytecode::DescriptorSequence,
        >,
        name: &str,
    ) -> Option<SymbolId> {
        self.symbols
            .data
            .binary_search_by(|symbol| p.symbols[symbol.index()].as_str().cmp(name))
            .ok()
            .map(|index| self.symbols.data[index])
    }

    /// Resolve dynamic property names without allocating text or scanning all symbols.
    pub(crate) fn symbol(
        &self,
        p: &BytecodePackage<
            impl crate::bytecode::InstructionSequence,
            impl crate::bytecode::SourceMapSequence,
            impl crate::bytecode::DescriptorSequence,
        >,
        heap: &Heap,
        key: Value,
    ) -> Result<Option<SymbolId>, HeapError> {
        use std::cmp::Ordering;
        if !heap.accepts_builtin(BuiltinType::String, key)? {
            return Err(HeapError::TypeMismatch);
        }
        let bytes = heap.read_object(key)?.len() as usize;
        let mut low = 0;
        let mut high = self.symbols.data.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let symbol = self.symbols.data[middle];
            let text = p.symbols[symbol.index()].as_bytes();
            let mut offset = 0;
            let mut ordering = bytes.cmp(&text.len());
            while offset < bytes {
                let segment = heap.byte_segment(key, offset as u32)?;
                let length = segment.len().min(text.len().saturating_sub(offset));
                let comparison = segment[..length].cmp(&text[offset..offset + length]);
                if comparison != Ordering::Equal {
                    ordering = comparison;
                    break;
                }
                if length < segment.len() {
                    ordering = Ordering::Greater;
                    break;
                }
                offset += length;
            }
            match ordering {
                Ordering::Equal => return Ok(Some(symbol)),
                Ordering::Less => high = middle,
                Ordering::Greater => low = middle + 1,
            }
        }
        Ok(None)
    }

    pub(super) fn accepts(
        &self,
        p: &BytecodePackage<
            impl crate::bytecode::InstructionSequence,
            impl crate::bytecode::SourceMapSequence,
            impl crate::bytecode::DescriptorSequence,
        >,
        heap: &Heap,
        ty: TypeId,
        value: Value,
    ) -> Result<bool, HeapError> {
        if let TypeDef::Builtin(kind) = p.types[ty.index()] {
            return heap.accepts_builtin(kind, value);
        }
        let mut stack = self.stack.borrow_mut();
        let result = (|| {
            let mut current = Some((ty, value));
            let mut accepted = false;
            loop {
                if let Some((ty, value)) = current.take() {
                    match &p.types[ty.index()] {
                        TypeDef::Builtin(kind) => accepted = heap.accepts_builtin(*kind, value)?,
                        TypeDef::Array(item) => {
                            accepted = heap
                                .array_item(value)?
                                .is_some_and(|actual| self.same_type(actual, *item));
                        }
                        TypeDef::Function { .. } => {
                            accepted = heap.accepts_builtin(BuiltinType::Closure, value)?
                                || (value.tag() == crate::ValueTag::Object
                                    && heap.read_object(value)?.kind() == crate::ObjectKind::Host
                                    && heap.host_call(value).is_ok());
                        }
                        TypeDef::Named { .. } => {
                            accepted = crate::class::accepts_nominal(heap, value, ty)?
                        }
                        TypeDef::StringLiteral(text) => {
                            accepted = string_equal(heap, value, text)?;
                        }
                        TypeDef::Record(fields) => {
                            if !heap.accepts_builtin(BuiltinType::Table, value)? {
                                accepted = false;
                            } else {
                                let len = heap.table_len(value)?;
                                accepted = len as usize <= fields.len();
                                if accepted && !fields.is_empty() {
                                    stack.data.push(Cursor {
                                        ty,
                                        value,
                                        next: 0,
                                        remaining_fields: len,
                                    });
                                }
                            }
                        }
                        TypeDef::Tuple(items) => {
                            if !heap.accepts_builtin(BuiltinType::Tuple, value)?
                                || heap.sequence_len(value)? as usize != items.len()
                            {
                                accepted = false;
                            } else if items.is_empty() {
                                accepted = true;
                            } else {
                                stack.data.push(Cursor {
                                    ty,
                                    value,
                                    next: 1,
                                    remaining_fields: 0,
                                });
                                current = Some((items[0], heap.sequence_get(value, 0)?));
                                continue;
                            }
                        }
                        TypeDef::Union(items) => {
                            if items.is_empty() {
                                accepted = false;
                            } else {
                                stack.data.push(Cursor {
                                    ty,
                                    value,
                                    next: 1,
                                    remaining_fields: 0,
                                });
                                current = Some((items[0], value));
                                continue;
                            }
                        }
                    }
                }
                loop {
                    let Some(mut parent) = stack.data.pop() else {
                        return Ok(accepted);
                    };
                    let (items, tuple) = match &p.types[parent.ty.index()] {
                        TypeDef::Tuple(items) => (items, true),
                        TypeDef::Union(items) => (items, false),
                        TypeDef::Record(fields) => {
                            while accepted && let Some(field) = fields.get(parent.next as usize) {
                                if let Some(value) = record_field(
                                    heap,
                                    parent.value,
                                    parent.next as usize,
                                    &p.symbols[field.name.index()],
                                )? {
                                    parent.next += 1;
                                    parent.remaining_fields -= 1;
                                    stack.data.push(parent);
                                    current = Some((field.ty, value));
                                    break;
                                }
                                accepted = field.optional;
                                parent.next += 1;
                            }
                            if current.is_some() {
                                break;
                            }
                            accepted &= parent.remaining_fields == 0;
                            continue;
                        }
                        _ => unreachable!(),
                    };
                    if accepted == tuple && (parent.next as usize) < items.len() {
                        let index = parent.next;
                        parent.next += 1;
                        stack.data.push(parent);
                        current = Some((
                            items[index as usize],
                            if tuple {
                                heap.sequence_get(parent.value, index)?
                            } else {
                                parent.value
                            },
                        ));
                        break;
                    }
                }
            }
        })();
        stack.data.clear();
        result
    }
}

pub(super) fn string_equal(heap: &Heap, value: Value, expected: &str) -> Result<bool, HeapError> {
    if !string_matches_length(heap, value, expected.len())? {
        return Ok(false);
    }
    string_equal_bytes(heap, value, expected)
}

pub(super) fn string_matches_length(
    heap: &Heap,
    value: Value,
    len: usize,
) -> Result<bool, HeapError> {
    if value.tag() != crate::ValueTag::Object {
        return Ok(false);
    }
    let object = heap.read_object(value)?;
    Ok(object.kind() == crate::ObjectKind::String && object.len() as usize == len)
}

/// Compare the payload after checking both the string kind and length.
pub(super) fn string_equal_bytes(
    heap: &Heap,
    value: Value,
    expected: &str,
) -> Result<bool, HeapError> {
    let mut remaining = expected.as_bytes();
    let mut offset = 0;
    while !remaining.is_empty() {
        let segment = heap.byte_segment(value, offset)?;
        if !remaining.starts_with(segment) {
            return Ok(false);
        }
        remaining = &remaining[segment.len()..];
        offset += segment.len() as u32;
    }
    Ok(true)
}

pub(super) fn record_field(
    heap: &Heap,
    table: Value,
    preferred: usize,
    name: &str,
) -> Result<Option<Value>, HeapError> {
    let table = heap.table_read(table)?;
    let preferred = preferred as u32;
    let first = (preferred < table.len()).then_some(preferred);
    for index in first
        .into_iter()
        .chain((0..table.len()).filter(|&index| index != preferred))
    {
        let (key, value) = table.entry(index)?;
        if string_equal(heap, key, name)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HeapLimits, SessionEpoch};
    #[test]
    fn structural_checks_reuse_charged_stack_without_allocations() {
        let mut p = fixtures::helper_native();
        let mut ty = TypeId(1); // int
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let mut value = Value::int(42);
        for _ in 0..64 {
            let next = TypeId(p.types.len() as u32);
            p.types.push(TypeDef::Tuple(vec![ty]));
            ty = next;
            value = heap.alloc_tuple(&[value]).unwrap();
        }
        let checked = crate::bytecode::verify(p, crate::bytecode::Limits::default()).unwrap();
        let checks = TypeChecks::new(checked.package(), &heap.budget_usage(), &[]).unwrap();
        let measured = allocation_counter::measure(|| {
            for _ in 0..10_000 {
                assert!(checks.accepts(checked.package(), &heap, ty, value).unwrap());
                assert!(
                    !checks
                        .accepts(checked.package(), &heap, ty, Value::NULL)
                        .unwrap()
                );
            }
        });
        assert_eq!(measured.count_total, 0);
        assert!(checks.stack.borrow().data.is_empty());
        println!(
            "10000 successful depth-64 tuple checks and 10000 rejections: allocations={}",
            measured.count_total
        );
    }
}
