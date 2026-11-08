//! Weak heap dependencies of the currently mounted stage projection.
use super::super::*;
use aside_vm::{ChangeCursor, HeapReservation};

pub(in crate::aside::stage) struct Index {
    cursor: Option<ChangeCursor>,
    topology: u64,
    dependencies: Vec<(u64, NodeId)>,
    changed: Vec<u64>,
    changed_charge: HeapReservation,
    pub pending: Vec<NodeId>,
    virtuals: Vec<NodeId>,
    dependency_charge: HeapReservation,
    pending_charge: HeapReservation,
    virtual_charge: HeapReservation,
    pub invalid: bool,
}
impl Index {
    pub fn new(heap: &Heap) -> Result<Self, AdapterError> {
        Ok(Self {
            cursor: None,
            topology: 0,
            dependencies: Vec::new(),
            changed: Vec::new(),
            changed_charge: heap.budget_usage().reserve(0)?,
            pending: Vec::new(),
            virtuals: Vec::new(),
            dependency_charge: heap.budget_usage().reserve(0)?,
            pending_charge: heap.budget_usage().reserve(0)?,
            virtual_charge: heap.budget_usage().reserve(0)?,
            invalid: true,
        })
    }
    /// Return false when topology, session or bounded history requires a rebuild.
    pub fn collect(&mut self, heap: &Heap, stage: &Stage) -> Result<bool, AdapterError> {
        self.pending.clear();
        if self.invalid || self.topology != stage.topology_revision() {
            return Ok(false);
        }
        let Some(changes) = self.cursor.and_then(|cursor| heap.changes_since(cursor)) else {
            return Ok(false);
        };
        // The bounded write history can contain many writes to the same shared
        // effect or override. Visit each dependency range only once per batch.
        self.changed.clear();
        for handle in changes {
            if self
                .dependencies
                .binary_search_by_key(&handle.key, |(key, _)| *key)
                .is_err()
            {
                continue;
            }
            reserve(heap, &mut self.changed, &mut self.changed_charge)?;
            self.changed.push(handle.key);
        }
        self.changed.sort_unstable();
        self.changed.dedup();
        for &changed in &self.changed {
            let start = self.dependencies.partition_point(|(key, _)| *key < changed);
            for &(key, id) in &self.dependencies[start..] {
                if key != changed {
                    break;
                }
                if let Err(at) = self.pending.binary_search(&id) {
                    reserve(heap, &mut self.pending, &mut self.pending_charge)?;
                    self.pending.insert(at, id);
                }
            }
        }
        if !self.pending.is_empty() {
            for &id in &self.virtuals {
                if let Err(at) = self.pending.binary_search(&id) {
                    reserve(heap, &mut self.pending, &mut self.pending_charge)?;
                    self.pending.insert(at, id);
                }
            }
        }
        // Keep the pending allocation bounded by unique nodes while different
        // changed sources can share every dependent node.
        Ok(true)
    }
    pub fn is_current(&self, heap: &Heap, stage: &Stage) -> Option<bool> {
        if self.invalid || self.topology != stage.topology_revision() {
            return None;
        }
        let mut changes = heap.changes_since(self.cursor?)?;
        Some(!changes.any(|handle| {
            self.dependencies
                .binary_search_by_key(&handle.key, |(key, _)| *key)
                .is_ok()
        }))
    }
    pub fn accept(&mut self, heap: &Heap) {
        self.cursor = Some(heap.change_cursor());
    }

    fn watch(&mut self, heap: &mut Heap, source: Value, id: NodeId) -> Result<(), AdapterError> {
        let key = heap.watch_object(source)?.key;
        reserve(heap, &mut self.dependencies, &mut self.dependency_charge)?;
        self.dependencies.push((key, id));
        Ok(())
    }
    pub fn rebuild(
        &mut self,
        heap: &mut Heap,
        scene: &Stage,
        handles: &BTreeMap<NodeId, NodeBinding>,
    ) -> Result<(), AdapterError> {
        self.invalid = true;
        self.dependencies.clear();
        self.virtuals.clear();
        self.pending.clear();
        if let Some(root) = scene.root() {
            reserve(heap, &mut self.pending, &mut self.pending_charge)?;
            self.pending.push(root);
        }
        let mut next = 0;
        while next < self.pending.len() {
            let id = self.pending[next];
            next += 1;
            let node = scene.node(id).unwrap();
            for &child in &node.children {
                reserve(heap, &mut self.pending, &mut self.pending_charge)?;
                self.pending.push(child);
            }
            if matches!(node.kind, NodeKind::VirtualList(_)) {
                reserve(heap, &mut self.virtuals, &mut self.virtual_charge)?;
                self.virtuals.push(id);
            }
            let binding = &handles[&id];
            let host = heap.import_handle(binding.handle)?;
            self.watch(heap, heap.host_fields(host)?, id)?;
            if binding.shape == snapshot::Shape::ScrollBar {
                let target = scrollbar::target(heap, heap.host_fields(host)?)?;
                if let Ok(fields) = heap.host_fields(target) {
                    self.watch(heap, target, id)?;
                    self.watch(heap, fields, id)?;
                }
            }
            for (handle, _) in [binding.mask_source, binding.effect_source]
                .into_iter()
                .flatten()
            {
                let value = heap.import_handle(handle)?;
                if crate::aside::effects::is_effect(heap, value) {
                    self.watch(heap, heap.host_fields(value)?, id)?;
                }
            }
            if binding.overrides.is_some() {
                overrides::visit_dependencies(heap, host, |heap, fields| {
                    self.watch(heap, fields, id)
                })?;
            }
        }
        self.dependencies.sort_unstable();
        self.dependencies.dedup();
        self.changed.clear();
        if !self.dependencies.is_empty() {
            // Admit the first change batch with the mounted projection, before input.
            reserve(heap, &mut self.changed, &mut self.changed_charge)?;
        }
        self.pending.clear();
        self.topology = scene.topology_revision();
        self.accept(heap);
        self.invalid = false;
        Ok(())
    }
}

fn reserve<T>(
    heap: &Heap,
    values: &mut Vec<T>,
    charge: &mut HeapReservation,
) -> Result<(), AdapterError> {
    if values.len() < values.capacity() {
        return Ok(());
    }
    let capacity = values
        .len()
        .max(4)
        .checked_mul(2)
        .ok_or(AdapterError::Allocation)?;
    let mut next_charge = heap
        .budget_usage()
        .reserve((capacity * size_of::<T>()) as u64)?;
    let mut next = Vec::new();
    next.try_reserve_exact(capacity)
        .map_err(|_| AdapterError::Allocation)?;
    next_charge.resize((next.capacity() * size_of::<T>()) as u64)?;
    next.append(values);
    *values = next;
    *charge = next_charge;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_sources_keep_pending_storage_bounded_by_unique_nodes() {
        let limits = aside_vm::HeapLimits::default();
        let mut heap = Heap::new(aside_vm::SessionEpoch(1), limits).unwrap();
        let stage = Stage::default();
        let mut index = Index::new(&heap).unwrap();
        let mut sources = Vec::new();
        for _ in 0..256 {
            let source = heap.alloc_array(&[Value::int(0)]).unwrap();
            for node in 0..64 {
                index.watch(&mut heap, source, NodeId(node)).unwrap();
            }
            sources.push(source);
        }
        index.dependencies.sort_unstable();
        index.topology = stage.topology_revision();
        index.invalid = false;
        index.accept(&heap);
        // Warm both bounded buffers using repeated writes to one shared source.
        for write in 0..256 {
            heap.set_field(sources[0], 0, Value::int(write)).unwrap();
        }
        assert!(index.collect(&heap, &stage).unwrap());
        index.accept(&heap);
        let capacity = index.pending.capacity();
        let charge = heap.budget_usage().charged_bytes();
        let _remaining = heap
            .budget_usage()
            .reserve(limits.max_bytes - charge - 256)
            .unwrap();
        let charged = heap.budget_usage().charged_bytes();
        let started = std::time::Instant::now();
        for frame in 1..=100 {
            for &source in &sources {
                heap.set_field(source, 0, Value::int(frame)).unwrap();
            }
            assert!(index.collect(&heap, &stage).unwrap());
            assert_eq!(index.pending.len(), 64);
            assert_eq!(index.pending.capacity(), capacity);
            assert_eq!(heap.budget_usage().charged_bytes(), charged);
            index.accept(&heap);
        }
        println!(
            "overlapping_projection sources=256 nodes=64 batches=100 pending_capacity={capacity} extra_charged=0 elapsed_us={}",
            started.elapsed().as_micros()
        );
    }
}
