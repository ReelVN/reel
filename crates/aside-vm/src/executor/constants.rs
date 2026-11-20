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

/// Constant admission and execution share one weak-cache resolution. A plan
/// stays within its current VM step, which cannot run the collector.
#[derive(Clone, Copy)]
pub(super) enum ConstantLoad {
    Ready(Value),
    Bytes(ConstId, ObjectKind),
    Tuple(ConstId),
}

impl ConstantLoad {
    pub(super) fn allocation_bytes(self, heap: &Heap) -> u64 {
        match self {
            Self::Ready(_) => 0,
            Self::Bytes(..) => heap.object_allocation_bytes(1),
            Self::Tuple(_) => heap.object_allocation_bytes(2),
        }
    }
}

pub(super) fn scalar_constant(constant: &Const) -> Option<Value> {
    Some(match constant {
        Const::Null => Value::NULL,
        Const::Bool(value) => Value::bool(*value),
        Const::Int(value) => Value::int(*value),
        Const::FloatBits(bits) => Value::float(f64::from_bits(*bits)),
        _ => return None,
    })
}

impl Vm {
    pub(super) fn plan_constant(&self, id: ConstId) -> ConstantLoad {
        let constant = &self.program.package().constants[id.index()];
        if let Some(value) = scalar_constant(constant) {
            return ConstantLoad::Ready(value);
        }
        let missing = match constant {
            Const::Utf8(_) | Const::Text { .. } => ConstantLoad::Bytes(id, ObjectKind::String),
            Const::Bytes(_) => ConstantLoad::Bytes(id, ObjectKind::Bytes),
            Const::Tuple(_) => ConstantLoad::Tuple(id),
            _ => unreachable!("immediate constants return above"),
        };
        let value = self.constant_cache.data[id.index()];
        // Resolve the full handle, including liveness after the collection cutoff.
        if value.tag() == ValueTag::Object && self.heap.read_object(value).is_ok() {
            ConstantLoad::Ready(value)
        } else {
            missing
        }
    }

    fn materialize_constant(&mut self, load: ConstantLoad) -> Result<Option<Value>, HeapError> {
        match load {
            ConstantLoad::Ready(value) => Ok(Some(value)),
            ConstantLoad::Bytes(id, kind) => {
                let value = self
                    .heap
                    .alloc_constant_bytes(kind, self.program.clone(), id)?;
                self.constant_cache.data[id.index()] = value;
                Ok(Some(value))
            }
            ConstantLoad::Tuple(_) => Ok(None),
        }
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
        let next = elements
            .get(index as usize)
            .map(|&child| self.plan_constant(child));
        let parents = self.value(roots, slots[1])?;
        let depth = self.heap.info(parents)?.len;
        let allocation = match next {
            Some(ConstantLoad::Tuple(_)) => {
                Values::growth_bytes(depth.into(), 3) + self.heap.object_allocation_bytes(1)
            }
            Some(load) => Values::growth_bytes(index.into(), 1) + load.allocation_bytes(&self.heap),
            None if depth != 0 => {
                let parent = self.heap.get_field(parents, depth - 3)?;
                Values::growth_bytes(self.heap.info(parent)?.len.into(), 1)
            }
            None => 0,
        };
        if !meter.take(0, 8, allocation) {
            return Ok(ConstantStep::Blocked);
        }
        Ok(match self.constant_step_inner(roots, slots, work, next) {
            Ok(Some(value)) => ConstantStep::Ready(value),
            Ok(None) => ConstantStep::Pending,
            Err(kind) => ConstantStep::Fault(kind),
        })
    }

    fn constant_step_inner(
        &mut self,
        roots: RootRangeToken,
        slots: [u32; 2],
        work: &mut ConstantWork,
        next: Option<ConstantLoad>,
    ) -> Result<Option<Value>, FaultKind> {
        let ConstantWork { dest, id, index } = *work;
        let builder = self.value(roots, slots[0]).map_err(vm_fault)?;
        let parents = self.value(roots, slots[1]).map_err(vm_fault)?;
        if let Some(load) = next {
            if let ConstantLoad::Tuple(child) = load {
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
                    .materialize_constant(load)
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
        load: ConstantLoad,
        roots: RootRangeToken,
        slots: [u32; 2],
    ) -> Result<Option<Value>, FaultKind> {
        if let Some(value) = self.materialize_constant(load).map_err(heap_fault)? {
            return Ok(Some(value));
        }
        let builder = self.heap.alloc_array(&[]).map_err(heap_fault)?;
        self.put(roots, slots[0], builder).map_err(vm_fault)?;
        let parents = self.heap.alloc_array(&[]).map_err(heap_fault)?;
        self.put(roots, slots[1], parents).map_err(vm_fault)?;
        Ok(None)
    }

    pub(super) fn execute_constant(
        &mut self,
        script: &mut ScriptFrame,
        dest: Reg,
        id: ConstId,
        load: ConstantLoad,
        next: Pc,
    ) -> Result<bool, FaultKind> {
        if let Some(value) = self.start_constant(load, script.env, [1, 2])? {
            self.write(script.regs, dest, value)?;
            script.pc = next;
        } else {
            script.phase = ScriptPhase::Constant(ConstantWork { dest, id, index: 0 });
        }
        Ok(false)
    }
}
