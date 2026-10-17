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

impl Vm {
    fn cached_constant(&self, id: ConstId) -> Option<Value> {
        match &self.program.package().constants[id.index()] {
            Const::Null => Some(Value::NULL),
            Const::Bool(value) => Some(Value::bool(*value)),
            Const::Int(value) => Some(Value::int(*value)),
            Const::FloatBits(bits) => Some(Value::float(f64::from_bits(*bits))),
            _ => {
                let value = self.constant_cache.data[id.index()];
                // info rejects stale generations, dying objects and objects
                // not confirmed alive after the incremental GC cutoff.
                (value.tag() == ValueTag::Object && self.heap.info(value).is_ok()).then_some(value)
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

    pub(super) fn constant_step(
        &mut self,
        frame: &mut ScriptFrame,
        work: ConstantWork,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let ConstantWork { id, index, .. } = work;
        let Const::Tuple(elements) = &self.program.package().constants[id.index()] else {
            unreachable!("tuple construction")
        };
        let next = elements.get(index as usize).copied();
        let parents = self.value(frame.env, 2)?;
        let depth = self.heap.info(parents)?.len;
        let nested = next.is_some_and(|child| {
            matches!(
                self.program.package().constants[child.index()],
                Const::Tuple(_)
            ) && self.cached_constant(child).is_none()
        });
        let allocation = if nested {
            Values::growth_bytes(depth.into(), 3)
        } else if next.is_some() {
            Values::growth_bytes(index.into(), 1)
        } else if next.is_none() && depth != 0 {
            let parent = self.heap.get_field(parents, depth - 3)?;
            Values::growth_bytes(self.heap.info(parent)?.len.into(), 1)
        } else {
            0
        };
        if !meter.take(0, 8, allocation) {
            return Ok(false);
        }
        let result = self.constant_step_inner(frame, work, next, nested);
        if let Err(kind) = result {
            self.collection_fault(frame, kind)?;
        }
        Ok(true)
    }

    fn constant_step_inner(
        &mut self,
        frame: &mut ScriptFrame,
        work: ConstantWork,
        next: Option<ConstId>,
        nested: bool,
    ) -> Result<(), FaultKind> {
        let ConstantWork { dest, id, index } = work;
        let builder = self.value(frame.env, 1).map_err(vm_fault)?;
        let parents = self.value(frame.env, 2).map_err(vm_fault)?;
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
                self.put(frame.env, 1, nested).map_err(vm_fault)?;
                frame.phase = ScriptPhase::Constant(ConstantWork {
                    dest,
                    id: child,
                    index: 0,
                });
            } else {
                let value = self
                    .constant_value(child)
                    .map_err(heap_fault)?
                    .expect("leaf or cached tuple");
                self.heap.array_push(builder, value).map_err(heap_fault)?;
                frame.phase = ScriptPhase::Constant(ConstantWork {
                    dest,
                    id,
                    index: index + 1,
                });
            }
            return Ok(());
        }
        self.heap.seal_tuple(builder).map_err(heap_fault)?;
        self.constant_cache.data[id.index()] = builder;
        if self.heap.info(parents).map_err(heap_fault)?.len == 0 {
            self.write(*frame, dest, builder)?;
            self.clear(frame.env, 1).map_err(vm_fault)?;
            self.clear(frame.env, 2).map_err(vm_fault)?;
            frame.pc.0 += 1;
            frame.phase = ScriptPhase::Body;
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
            self.put(frame.env, 1, parent).map_err(vm_fault)?;
            frame.phase = ScriptPhase::Constant(ConstantWork { dest, id, index });
        }
        Ok(())
    }
}
