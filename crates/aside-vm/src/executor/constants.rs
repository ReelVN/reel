//! Lazy literal materialization. The weak cache never participates in tracing;
//! all builders and parent cursors are ordinary rooted, snapshotable values.
use super::*;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(super) struct ConstantWork {
    pub dest: Reg,
    pub id: ConstId,
    pub index: u32,
}

pub(super) enum ConstantStep {
    Blocked,
    Pending,
    Ready(Value),
    Fault(FaultKind),
}

impl Vm {
    fn cached_constant(&self, id: ConstId) -> Option<Value> {
        match &self.program.package().constants[id.index()] {
            Const::Null => Some(Value::NULL),
            Const::Bool(value) => Some(Value::bool(*value)),
            Const::Int(value) => Some(Value::int(*value)),
            Const::FloatBits(bits) => Some(Value::float(f64::from_bits(*bits))),
            _ => {
                let value = self.constant_cache.data[id.index()];
                // Object resolution rejects stale generations, dying objects and objects
                // not confirmed alive after the incremental GC cutoff.
                (value.tag() == ValueTag::Object && self.heap.read_object(value).is_ok())
                    .then_some(value)
            }
        }
    }

    pub(super) fn constant_value(&mut self, id: ConstId) -> Result<Option<Value>, HeapError> {
        if let Some(value) = self.cached_constant(id) {
            return Ok(Some(value));
        }
        let kind = match &self.program.package().constants[id.index()] {
            Const::Utf8(_) | Const::Text { .. } => ObjectKind::String,
            Const::Bytes(_) => ObjectKind::Bytes,
            Const::Tuple(_) => return Ok(None),
            _ => unreachable!("scalar constant"),
        };
        let value = self
            .heap
            .alloc_constant_bytes(kind, self.program.clone(), id)?;
        self.constant_cache.data[id.index()] = value;
        Ok(Some(value))
    }

    pub(super) fn constant_allocation_bytes(&self, id: ConstId) -> u64 {
        if self.cached_constant(id).is_some() {
            return 0;
        }
        self.heap.object_allocation_bytes(
            if matches!(
                self.program.package().constants[id.index()],
                Const::Tuple(_)
            ) {
                2
            } else {
                1
            },
        )
    }

    pub(super) fn constant_step(
        &mut self,
        roots: RootRangeToken,
        slots: [u32; 2],
        work: &mut ConstantWork,
        meter: &mut Meter,
    ) -> Result<ConstantStep, VmError> {
        let ConstantWork { id, index, .. } = *work;
        let Const::Tuple(elements) = &self.program.package().constants[id.index()] else {
            unreachable!("tuple construction")
        };
        let next = elements.get(index as usize).copied();
        let parents = self.value(roots, slots[1])?;
        let depth = self.heap.info(parents)?.len;
        let nested = next.is_some_and(|child| {
            matches!(
                self.program.package().constants[child.index()],
                Const::Tuple(_)
            ) && self.cached_constant(child).is_none()
        });
        let allocation = if nested {
            Values::growth_bytes(depth.into(), 3) + self.heap.object_allocation_bytes(1)
        } else if next.is_some() {
            Values::growth_bytes(index.into(), 1)
                + next.map_or(0, |id| self.constant_allocation_bytes(id))
        } else if next.is_none() && depth != 0 {
            let parent = self.heap.get_field(parents, depth - 3)?;
            Values::growth_bytes(self.heap.info(parent)?.len.into(), 1)
        } else {
            0
        };
        if !meter.take(0, 8, allocation) {
            return Ok(ConstantStep::Blocked);
        }
        Ok(
            match self.constant_step_inner(roots, slots, work, next, nested) {
                Ok(Some(value)) => ConstantStep::Ready(value),
                Ok(None) => ConstantStep::Pending,
                Err(kind) => ConstantStep::Fault(kind),
            },
        )
    }

    fn constant_step_inner(
        &mut self,
        roots: RootRangeToken,
        slots: [u32; 2],
        work: &mut ConstantWork,
        next: Option<ConstId>,
        nested: bool,
    ) -> Result<Option<Value>, FaultKind> {
        let ConstantWork { dest, id, index } = *work;
        let builder = self.value(roots, slots[0]).map_err(vm_fault)?;
        let parents = self.value(roots, slots[1]).map_err(vm_fault)?;
        if let Some(child) = next {
            if nested {
                self.heap.array_push(parents, builder).map_err(heap_fault)?;
                self.heap
                    .array_push(parents, Value::int(i64::from(id.0)))
                    .map_err(heap_fault)?;
                self.heap
                    .array_push(parents, Value::int(i64::from(index + 1)))
                    .map_err(heap_fault)?;
                let nested = self.heap.alloc_array(&[]).map_err(heap_fault)?;
                self.put(roots, slots[0], nested).map_err(vm_fault)?;
                *work = ConstantWork {
                    dest,
                    id: child,
                    index: 0,
                };
            } else {
                let value = self
                    .constant_value(child)
                    .map_err(heap_fault)?
                    .expect("leaf or cached tuple");
                self.heap.array_push(builder, value).map_err(heap_fault)?;
                *work = ConstantWork {
                    dest,
                    id,
                    index: index + 1,
                };
            }
            return Ok(None);
        }
        self.heap.seal_tuple(builder).map_err(heap_fault)?;
        self.constant_cache.data[id.index()] = builder;
        if self.heap.info(parents).map_err(heap_fault)?.len == 0 {
            return Ok(Some(builder));
        } else {
            let index = self
                .heap
                .array_pop(parents)
                .map_err(heap_fault)?
                .expect("constant parent cursor")
                .as_int()
                .unwrap() as u32;
            let id = ConstId(
                self.heap
                    .array_pop(parents)
                    .map_err(heap_fault)?
                    .expect("constant parent cursor")
                    .as_int()
                    .unwrap() as u32,
            );
            let parent = self
                .heap
                .array_pop(parents)
                .map_err(heap_fault)?
                .expect("constant parent cursor");
            self.heap.array_push(parent, builder).map_err(heap_fault)?;
            self.put(roots, slots[0], parent).map_err(vm_fault)?;
            *work = ConstantWork { dest, id, index };
        }
        Ok(None)
    }

    pub(super) fn finish_constant(
        &mut self,
        roots: RootRangeToken,
        slots: [u32; 2],
    ) -> Result<(), VmError> {
        self.clear(roots, slots[0])?;
        self.clear(roots, slots[1])
    }

    /// Only an uncached aggregate needs temporary roots and a construction cursor.
    pub(super) fn start_constant(
        &mut self,
        id: ConstId,
        roots: RootRangeToken,
        slots: [u32; 2],
    ) -> Result<Option<Value>, FaultKind> {
        if let Some(value) = self.constant_value(id).map_err(heap_fault)? {
            return Ok(Some(value));
        }
        let builder = self.heap.alloc_array(&[]).map_err(heap_fault)?;
        self.put(roots, slots[0], builder).map_err(vm_fault)?;
        let parents = self.heap.alloc_array(&[]).map_err(heap_fault)?;
        self.put(roots, slots[1], parents).map_err(vm_fault)?;
        Ok(None)
    }
}
