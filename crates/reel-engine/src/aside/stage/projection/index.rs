//! Weak heap dependencies of the currently mounted stage projection.
use super::super::*;
use aside_vm::{ChangeCursor, HeapReservation};

pub(in crate::aside::stage) struct Index {
    cursor: Option<ChangeCursor>,
    topology: u64,
    dependencies: Vec<(u64, NodeId)>,
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
        for handle in changes {
            let start = self
                .dependencies
                .partition_point(|(key, _)| *key < handle.key);
            for &(key, id) in &self.dependencies[start..] {
                if key != handle.key {
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
