//! Shared structural contracts. The verified type DAG bounds work and scratch.
use crate::{
    Heap, HeapBudgetUsage, HeapError, Value, bytecode::*, executor::VmError,
    root_ranges::ChargedVec,
};
use std::cell::RefCell;

#[derive(Clone, Copy)]
struct Cursor {
    ty: TypeId,
    value: Value,
    next: usize,
}

pub(crate) struct TypeChecks {
    work: ChargedVec<u64>,
    signature_work: ChargedVec<u64>,
    stack: RefCell<ChargedVec<Cursor>>,
    symbols: ChargedVec<SymbolId>,
    pub(crate) host_types: crate::host_types::HostTypes,
}

impl TypeChecks {
    pub(super) fn new(
        p: &BytecodePackage<
            impl crate::bytecode::InstructionSequence,
            impl crate::bytecode::SourceMapSequence,
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
                TypeDef::Function { .. } => (2, 0usize),
                TypeDef::Named { .. } => (3 + host_types.work, 0usize),
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
            host_types,
        })
    }

    pub(super) fn extra(&self, ty: TypeId) -> u64 {
        self.work.data[ty.index()] - 1
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
        >,
        heap: &Heap,
        key: Value,
    ) -> Result<Option<SymbolId>, HeapError> {
        use std::cmp::Ordering;
        if !heap.accepts_builtin(BuiltinType::String, key)? {
            return Err(HeapError::TypeMismatch);
        }
        let bytes = heap.info(key)?.len as usize;
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
                        TypeDef::Function { .. } => {
                            accepted = heap.accepts_builtin(BuiltinType::Closure, value)?
                                || (value.tag() == crate::ValueTag::Object
                                    && heap.info(value)?.kind == crate::ObjectKind::Host
                                    && heap.host_call(value).is_ok());
                        }
                        TypeDef::Named { .. } => {
                            accepted = crate::class::accepts_nominal(heap, value, ty)?
                        }
                        TypeDef::Tuple(items) => {
                            if !heap.accepts_builtin(BuiltinType::Tuple, value)?
                                || heap.sequence_len(value)? as usize != items.len()
                            {
                                accepted = false;
                            } else if items.is_empty() {
                                accepted = true;
                            } else {
                                stack.data.push(Cursor { ty, value, next: 1 });
                                current = Some((items[0], heap.sequence_get(value, 0)?));
                                continue;
                            }
                        }
                        TypeDef::Union(items) => {
                            if items.is_empty() {
                                accepted = false;
                            } else {
                                stack.data.push(Cursor { ty, value, next: 1 });
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
                        _ => unreachable!(),
                    };
                    if accepted == tuple && parent.next < items.len() {
                        let index = parent.next;
                        parent.next += 1;
                        stack.data.push(parent);
                        current = Some((
                            items[index],
                            if tuple {
                                heap.sequence_get(parent.value, index as u32)?
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
