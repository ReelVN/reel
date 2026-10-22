//! Rooted, resumable graph validation for one unpublished property proposal.
use super::*;
use crate::heap::properties::{SOURCE_WORK, SourceWalkProgress};

pub(in crate::heap) enum PropertyGraphProgress {
    Preparing { work: u64, required_work: u64 },
    Complete { work: u64, disconnect_work: u64 },
}

#[derive(Clone, Copy)]
#[repr(u32)]
enum Slot {
    Disconnect,
    OutputHost,
    OutputField,
    Visited,
    Versions,
    Phase,
    PendingHost,
    PendingField,
    Walk,
    Queue,
    Input,
    Dependencies,
    Edges,
    DisconnectWork,
    Work,
    Count,
}
const ITEM_WORK: u64 = 128;
const HOST_VERSION: u32 = 0;
const REACTIVE_LIST: u32 = 1;
const BOUND_LIST: u32 = 2;
const PROOF_FIELDS: u32 = 4;

impl Heap {
    fn graph_get(&self, cursor: Value, slot: Slot) -> Result<Value, HeapError> {
        self.sequence_get(cursor, slot as u32)
    }
    fn graph_number(&self, cursor: Value, slot: Slot) -> Result<u64, HeapError> {
        Ok(self
            .graph_get(cursor, slot)?
            .as_int()
            .ok_or(HeapError::TypeMismatch)? as u64)
    }
    fn graph_set(&mut self, cursor: Value, slot: Slot, value: u64) -> Result<(), HeapError> {
        self.set_field(cursor, slot as u32, Value::int(value as i64))
    }
    fn begin_property_graph(
        &mut self,
        output: PropertyId,
        source: PropertyId,
        disconnect: bool,
    ) -> Result<Value, HeapError> {
        let cursor = self.alloc_array(&[Value::NULL; Slot::Count as usize])?;
        let visited = self.alloc_dependencies()?;
        self.set_field(cursor, Slot::Visited as u32, visited)?;
        let versions = self.alloc_array(&[])?;
        self.set_field(cursor, Slot::Versions as u32, versions)?;
        self.set_field(cursor, Slot::Disconnect as u32, Value::bool(disconnect))?;
        self.set_field(cursor, Slot::OutputHost as u32, output.host)?;
        self.graph_set(cursor, Slot::OutputField, u64::from(output.field))?;
        self.set_field(cursor, Slot::PendingHost as u32, source.host)?;
        self.graph_set(cursor, Slot::PendingField, u64::from(source.field))?;
        for slot in [
            Slot::Phase,
            Slot::Queue,
            Slot::Input,
            Slot::DisconnectWork,
            Slot::Work,
        ] {
            self.graph_set(cursor, slot, 0)?;
        }
        self.graph_set(cursor, Slot::Edges, 1)?;
        Ok(cursor)
    }
    pub(in crate::heap) fn begin_property_cycle(
        &mut self,
        output: PropertyId,
        source: PropertyId,
    ) -> Result<Value, HeapError> {
        self.begin_property_graph(output, source, false)
    }
    pub(in crate::heap) fn begin_property_disconnect(
        &mut self,
        output: Value,
    ) -> Result<Value, HeapError> {
        let property = PropertyId {
            host: output,
            field: 0,
        };
        self.begin_property_graph(property, property, true)
    }
    fn graph_list(&self, owner: Value, kind: u32) -> Result<Value, HeapError> {
        match kind {
            REACTIVE_LIST => Ok(self.object(self.key(owner)?)?.reactive_subscribers),
            BOUND_LIST => self.subscribers(PropertyId {
                host: owner,
                field: 0,
            }),
            _ => Err(HeapError::WrongKind),
        }
    }
    fn graph_proof(&mut self, cursor: Value, owner: Value, kind: u32) -> Result<(), HeapError> {
        if kind != HOST_VERSION
            && !self.capture_dependency(
                self.graph_get(cursor, Slot::Visited)?,
                owner,
                Some(kind),
            )?
        {
            return Ok(());
        }
        let reference = if kind == HOST_VERSION {
            Value::NULL
        } else {
            self.graph_list(owner, kind)?
        };
        let version = if kind == HOST_VERSION {
            self.info(owner)?.content_version
        } else if reference.same_bits(Value::NULL) {
            0
        } else {
            self.info(reference)?.content_version
        };
        let versions = self.graph_get(cursor, Slot::Versions)?;
        for value in [
            owner,
            Value::int(i64::from(kind)),
            reference,
            Value::int(version as i64),
        ] {
            self.array_push(versions, value)?;
        }
        Ok(())
    }
    /// Each proof is a constant-cost host version or subscription-list pointer
    /// and version check. The outer proposal accounts each one before commit.
    pub(in crate::heap) fn property_graph_versions_len(
        &self,
        cursor: Value,
    ) -> Result<u32, HeapError> {
        Ok(self.sequence_len(self.graph_get(cursor, Slot::Versions)?)? / PROOF_FIELDS)
    }
    pub(in crate::heap) fn property_graph_version_valid(
        &self,
        cursor: Value,
        index: u32,
    ) -> Result<bool, HeapError> {
        let versions = self.graph_get(cursor, Slot::Versions)?;
        let offset = index
            .checked_mul(PROOF_FIELDS)
            .ok_or(HeapError::OutOfBounds)?;
        let owner = self.sequence_get(versions, offset)?;
        let kind = self
            .sequence_get(versions, offset + 1)?
            .as_int()
            .ok_or(HeapError::TypeMismatch)? as u32;
        let reference = self.sequence_get(versions, offset + 2)?;
        let expected = self
            .sequence_get(versions, offset + 3)?
            .as_int()
            .ok_or(HeapError::TypeMismatch)? as u64;
        if kind == HOST_VERSION {
            return Ok(self.info(owner)?.content_version == expected);
        }
        let actual = self.graph_list(owner, kind)?;
        Ok(actual.same_bits(reference)
            && (actual.same_bits(Value::NULL) || self.info(actual)?.content_version == expected))
    }
    pub(in crate::heap) fn property_graph_work(&self, cursor: Value) -> Result<u64, HeapError> {
        self.graph_number(cursor, Slot::Work)
    }
    pub(in crate::heap) fn property_graph_required_work(
        &self,
        cursor: Value,
    ) -> Result<u64, HeapError> {
        let phase = self.graph_number(cursor, Slot::Phase)?;
        if phase == 4 {
            return Ok(0);
        }
        if phase == 0 && self.graph_get(cursor, Slot::Disconnect)?.as_bool() != Some(true) {
            return Ok(SOURCE_WORK);
        }
        if phase == 3 {
            let edges = self.graph_number(cursor, Slot::Edges)?;
            if edges > self.derived_disconnect_work.capacity() as u64 {
                return edges
                    .checked_mul(4)
                    .and_then(|work| work.checked_add(ITEM_WORK))
                    .ok_or(HeapError::OutOfBounds);
            }
        }
        Ok(ITEM_WORK)
    }
    /// Walk one source-resolution item or one dependency at a time. The private
    /// dependency payload owns both the visited index and its ordered strong
    /// edges, so GC and snapshots use their existing representation.
    pub(in crate::heap) fn step_property_graph(
        &mut self,
        cursor: Value,
        writes: &dyn PropertyWrites,
        budget: u64,
    ) -> Result<PropertyGraphProgress, HeapError> {
        let mut work = 0;
        loop {
            let phase = self.graph_number(cursor, Slot::Phase)?;
            if phase == 4 {
                return Ok(PropertyGraphProgress::Complete {
                    work,
                    disconnect_work: self.graph_number(cursor, Slot::DisconnectWork)?,
                });
            }
            let disconnect = self.graph_get(cursor, Slot::Disconnect)?.as_bool() == Some(true);
            let cost = self.property_graph_required_work(cursor)?;
            if budget - work < cost {
                return Ok(PropertyGraphProgress::Preparing {
                    work,
                    required_work: cost,
                });
            }
            work += cost;
            self.graph_set(
                cursor,
                Slot::Work,
                self.graph_number(cursor, Slot::Work)?
                    .checked_add(cost)
                    .ok_or(HeapError::OutOfBounds)?,
            )?;
            match phase {
                0 => {
                    let source = PropertyId {
                        host: self.graph_get(cursor, Slot::PendingHost)?,
                        field: self.graph_number(cursor, Slot::PendingField)? as u32,
                    };
                    if disconnect {
                        self.graph_set(cursor, Slot::Phase, 1)?;
                        continue;
                    }
                    let output = PropertyId {
                        host: self.graph_get(cursor, Slot::OutputHost)?,
                        field: self.graph_number(cursor, Slot::OutputField)? as u32,
                    };
                    if source.host.same_bits(output.host)
                        && (source.field == output.field || source.field == u32::MAX)
                    {
                        return Err(HeapError::BindingCycle);
                    }
                    if source.field == u32::MAX {
                        self.graph_set(cursor, Slot::Phase, 1)?;
                        continue;
                    }
                    let mut walk = self.graph_get(cursor, Slot::Walk)?;
                    if walk.same_bits(Value::NULL) {
                        walk = self.begin_property_source_walk(source, Some(output))?;
                        self.set_field(cursor, Slot::Walk as u32, walk)?;
                        continue;
                    }
                    if let SourceWalkProgress::Resolved { source, .. } =
                        self.step_property_source_walk(walk, writes, SOURCE_WORK)?
                    {
                        self.set_field(cursor, Slot::PendingHost as u32, source.host)?;
                        self.graph_set(cursor, Slot::PendingField, u64::from(source.field))?;
                        self.graph_set(cursor, Slot::Phase, 1)?;
                    }
                }
                1 => {
                    let host = self.graph_get(cursor, Slot::PendingHost)?;
                    if self.object(self.key(host)?)?.derived
                        && self.capture_dependency(
                            self.graph_get(cursor, Slot::Visited)?,
                            host,
                            Some(0),
                        )?
                    {
                        self.graph_proof(cursor, host, HOST_VERSION)?;
                        if disconnect {
                            self.graph_set(
                                cursor,
                                Slot::DisconnectWork,
                                self.graph_number(cursor, Slot::DisconnectWork)?
                                    .checked_add(ITEM_WORK)
                                    .ok_or(HeapError::OutOfBounds)?,
                            )?;
                        }
                    }
                    self.graph_set(cursor, Slot::Phase, 2)?;
                }
                2 => {
                    let visited = self.graph_get(cursor, Slot::Visited)?;
                    let queue = self.graph_number(cursor, Slot::Queue)? as u32;
                    if queue == self.sequence_len(visited)? {
                        self.graph_set(cursor, Slot::Phase, if disconnect { 3 } else { 4 })?;
                        continue;
                    }
                    // Proof-only identities share the same sparse index and do
                    // not add vertices to the derived dependency traversal.
                    if self.sequence_get(visited, queue + 1)?.as_int() != Some(0) {
                        self.graph_set(cursor, Slot::Queue, u64::from(queue + 2))?;
                        continue;
                    }
                    let host = self.sequence_get(visited, queue)?;
                    let mut dependencies = self.graph_get(cursor, Slot::Dependencies)?;
                    if dependencies.same_bits(Value::NULL) {
                        if disconnect {
                            self.graph_proof(cursor, host, REACTIVE_LIST)?;
                            self.graph_proof(cursor, host, BOUND_LIST)?;
                        }
                        dependencies = self.host_payload(host, DerivedSlot::Dependencies as u32)?;
                        self.set_field(cursor, Slot::Dependencies as u32, dependencies)?;
                    }
                    let input = self.graph_number(cursor, Slot::Input)? as u32;
                    if dependencies.same_bits(Value::NULL)
                        || input == self.sequence_len(dependencies)?
                    {
                        self.graph_set(cursor, Slot::Queue, u64::from(queue + 2))?;
                        self.graph_set(cursor, Slot::Input, 0)?;
                        self.set_field(cursor, Slot::Dependencies as u32, Value::NULL)?;
                        continue;
                    }
                    let source = self.sequence_get(dependencies, input)?;
                    let selector = self
                        .sequence_get(dependencies, input + 1)?
                        .as_int()
                        .ok_or(HeapError::TypeMismatch)? as u32;
                    self.graph_set(cursor, Slot::Input, u64::from(input + 2))?;
                    if disconnect {
                        self.graph_proof(cursor, source, REACTIVE_LIST)?;
                        let subscribers = self.object(self.key(source)?)?.reactive_subscribers;
                        let comparisons = if subscribers.same_bits(Value::NULL) {
                            0
                        } else {
                            u64::from(self.sequence_len(subscribers)?) / 2
                        };
                        let extra = ITEM_WORK.saturating_mul(comparisons + 1);
                        self.graph_set(
                            cursor,
                            Slot::DisconnectWork,
                            self.graph_number(cursor, Slot::DisconnectWork)?
                                .checked_add(extra)
                                .ok_or(HeapError::OutOfBounds)?,
                        )?;
                        self.graph_set(
                            cursor,
                            Slot::Edges,
                            self.graph_number(cursor, Slot::Edges)?
                                .checked_add(1)
                                .ok_or(HeapError::OutOfBounds)?,
                        )?;
                    }
                    self.set_field(cursor, Slot::PendingHost as u32, source)?;
                    self.graph_set(cursor, Slot::PendingField, u64::from(selector))?;
                    let walk = self.graph_get(cursor, Slot::Walk)?;
                    if !disconnect && selector != u32::MAX && !walk.same_bits(Value::NULL) {
                        self.reset_property_source_walk(
                            walk,
                            PropertyId {
                                host: source,
                                field: selector,
                            },
                            Some(PropertyId {
                                host: self.graph_get(cursor, Slot::OutputHost)?,
                                field: self.graph_number(cursor, Slot::OutputField)? as u32,
                            }),
                        )?;
                    }
                    self.graph_set(cursor, Slot::Phase, 0)?;
                }
                3 => {
                    let needed = usize::try_from(self.graph_number(cursor, Slot::Edges)?)
                        .map_err(|_| HeapError::OutOfBounds)?;
                    self.reserve_derived_disconnect(needed)?;
                    self.graph_set(cursor, Slot::Phase, 4)?;
                }
                _ => return Err(HeapError::WrongKind),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(heap: &mut Heap) -> PropertyId {
        let fields = heap.alloc_table().unwrap();
        heap.table_store(fields, 0, Value::int(0), Value::int(1))
            .unwrap();
        let host = heap.alloc_host(fields, &[], None).unwrap();
        heap.observe_host_properties(host, 1, 0).unwrap();
        PropertyId { host, field: 0 }
    }
    fn derived(heap: &mut Heap, inputs: &[Value]) -> Value {
        let output = heap.alloc_derived(Value::NULL).unwrap();
        heap.begin_derived(output).unwrap();
        for &input in inputs {
            heap.observe_read(input, Some(0)).unwrap();
        }
        heap.commit_derived(output, Value::int(1)).unwrap();
        output
    }
    fn finish(heap: &mut Heap, scan: Value) -> (u64, u64) {
        let mut budget = 128;
        let mut total = 0;
        for _ in 0..100_000 {
            match heap.step_property_graph(scan, &[], budget).unwrap() {
                PropertyGraphProgress::Preparing {
                    work,
                    required_work,
                } => {
                    assert!(work <= budget);
                    total += work;
                    budget = required_work.max(128);
                }
                PropertyGraphProgress::Complete {
                    work,
                    disconnect_work,
                } => {
                    assert!(work <= budget);
                    total += work;
                    assert_eq!(heap.property_graph_work(scan).unwrap(), total);
                    return (total, disconnect_work);
                }
            }
        }
        panic!("graph scan did not settle");
    }
    #[test]
    fn cycle_checks_visit_shared_vertices_once_and_reject_a_transitive_back_edge() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = host(&mut heap);
        let output = host(&mut heap);
        let leaf = derived(&mut heap, &[source.host]);
        let left = derived(&mut heap, &[leaf]);
        let right = derived(&mut heap, &[leaf]);
        let top = derived(&mut heap, &[left, right]);
        let scan = heap
            .begin_property_cycle(
                output,
                PropertyId {
                    host: top,
                    field: 0,
                },
            )
            .unwrap();
        assert!(matches!(
            heap.step_property_graph(scan, &[], 1).unwrap(),
            PropertyGraphProgress::Preparing { work: 0, .. }
        ));
        finish(&mut heap, scan);
        assert_eq!(heap.property_graph_versions_len(scan).unwrap(), 4);
        for index in 0..4 {
            assert!(heap.property_graph_version_valid(scan, index).unwrap());
        }
        let cycle = heap
            .begin_property_cycle(
                source,
                PropertyId {
                    host: top,
                    field: 0,
                },
            )
            .unwrap();
        let mut rejected = false;
        for _ in 0..1000 {
            match heap.step_property_graph(cycle, &[], 128) {
                Err(HeapError::BindingCycle) => {
                    rejected = true;
                    break;
                }
                Ok(PropertyGraphProgress::Preparing { work, .. }) => assert!(work <= 128),
                _ => panic!("back edge was not rejected"),
            }
        }
        assert!(rejected);
        assert!(
            heap.property_source(source)
                .unwrap()
                .0
                .host
                .same_bits(source.host)
        );
        assert_eq!(heap.host_property(source).unwrap().as_int(), Some(1));
    }
    #[test]
    fn dependency_changes_and_subscriber_growth_invalidate_completed_proofs() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = host(&mut heap);
        let output = host(&mut heap);
        let top = derived(&mut heap, &[source.host]);
        let cycle = heap
            .begin_property_cycle(
                output,
                PropertyId {
                    host: top,
                    field: 0,
                },
            )
            .unwrap();
        finish(&mut heap, cycle);
        heap.begin_derived(top).unwrap();
        heap.observe_read(output.host, Some(0)).unwrap();
        heap.commit_derived(top, Value::int(1)).unwrap();
        assert!(!heap.property_graph_version_valid(cycle, 0).unwrap());
        let disconnect = heap.begin_property_disconnect(top).unwrap();
        let (_, cleanup) = finish(&mut heap, disconnect);
        assert!(cleanup >= ITEM_WORK * 2);
        let proofs = heap.property_graph_versions_len(disconnect).unwrap();
        assert!((0..proofs).all(|i| heap.property_graph_version_valid(disconnect, i).unwrap()));
        let _another = derived(&mut heap, &[output.host]);
        assert!((0..proofs).any(|i| !heap.property_graph_version_valid(disconnect, i).unwrap()));
    }
    #[test]
    fn small_graph_steps_keep_their_roots_during_incremental_collection() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = host(&mut heap);
        let output = host(&mut heap);
        let mut top = source.host;
        for _ in 0..64 {
            top = derived(&mut heap, &[top]);
        }
        let scan = heap
            .begin_property_cycle(
                output,
                PropertyId {
                    host: top,
                    field: 0,
                },
            )
            .unwrap();
        let root = heap.add_root(RootOwner(1), RootKind::Host, scan).unwrap();
        let mut complete = false;
        for _ in 0..10_000 {
            if matches!(
                heap.step_property_graph(scan, &[], 128).unwrap(),
                PropertyGraphProgress::Complete { .. }
            ) {
                complete = true;
                break;
            }
            if heap.phase() == crate::GcPhase::Idle {
                heap.begin_collection().unwrap();
            }
            heap.gc_step(crate::GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
        }
        assert!(complete);
        assert_eq!(heap.property_graph_versions_len(scan).unwrap(), 64);
        heap.release_root(root).unwrap();
    }

    #[test]
    fn graph_cursor_snapshot_remaps_visited_indexes_and_pending_source_resolution() {
        use crate::heap::snapshot as graph;
        use crate::snapshot::{SnapshotMap, SnapshotRegistry, VmSnapshot};
        for disconnect in [false, true] {
            let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
            let limits = crate::FrameSlotsLimits {
                initial_capacity: 8,
                max_capacity: 128,
                max_window_len: 64,
            };
            let slots = crate::FrameSlots::new(&mut heap, limits).unwrap();
            let input = host(&mut heap);
            let target = host(&mut heap);
            let mut output = input.host;
            for _ in 0..12 {
                output = derived(&mut heap, &[output]);
            }
            let cursor = if disconnect {
                heap.begin_property_disconnect(output).unwrap()
            } else {
                heap.begin_property_cycle(
                    target,
                    PropertyId {
                        host: output,
                        field: 0,
                    },
                )
                .unwrap()
            };
            heap.add_root(RootOwner(1), RootKind::Host, cursor).unwrap();
            for _ in 0..9 {
                heap.step_property_graph(cursor, &[], 128).unwrap();
            }
            assert!(heap.property_graph_versions_len(cursor).unwrap() > 0);
            let registry = SnapshotRegistry::default();
            let mut image = VmSnapshot {
                version: VmSnapshot::FORMAT_VERSION,
                code: [0; 32],
                source_heap: heap.id().get(),
                source_epoch: 1,
                records: Vec::new(),
                bytes: 0,
            };
            let mut capture = graph::GraphCapture::new(&heap).unwrap();
            while !capture.step(&heap, &slots, &registry, &mut image).unwrap() {}
            let image = VmSnapshot::decode(&image.encode().unwrap()).unwrap();
            let mut restored = Heap::new(SessionEpoch(2), HeapLimits::default()).unwrap();
            for _ in 0..7 {
                restored.alloc_array(&[]).unwrap();
            }
            let mut restored_slots = crate::FrameSlots::new(&mut restored, limits).unwrap();
            let mut map = SnapshotMap::new(&restored, &image);
            for record in &image.records {
                graph::allocate(
                    &mut restored,
                    &mut restored_slots,
                    &mut map,
                    record,
                    &registry,
                )
                .unwrap();
            }
            for record in &image.records {
                graph::connect(&mut restored, &mut restored_slots, &mut map, record).unwrap();
            }
            for record in &image.records {
                graph::validate(&restored, &map, record).unwrap();
            }
            let cursor2 = map.value(cursor).unwrap();
            assert_ne!(
                cursor.object_parts().unwrap().1,
                cursor2.object_parts().unwrap().1
            );
            // The cumulative counter precedes this pause; only the remaining work
            // is returned by resumed steps, without repeating dependency evaluation.
            let previous = restored.property_graph_work(cursor2).unwrap();
            let mut budget = 128;
            let mut consumed = 0;
            loop {
                match restored.step_property_graph(cursor2, &[], budget).unwrap() {
                    PropertyGraphProgress::Preparing {
                        work,
                        required_work,
                    } => {
                        consumed += work;
                        budget = required_work.max(128);
                    }
                    PropertyGraphProgress::Complete { work, .. } => {
                        consumed += work;
                        break;
                    }
                }
            }
            assert_eq!(
                restored.property_graph_work(cursor2).unwrap(),
                previous + consumed
            );
            let proofs = restored.property_graph_versions_len(cursor2).unwrap();
            if disconnect {
                assert!(proofs > 12);
                assert!(restored.derived_disconnect_work.capacity() >= 13);
            } else {
                assert_eq!(proofs, 12);
            }
        }
    }
    #[test]
    fn cancelling_an_unpublished_graph_releases_its_private_roots() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = host(&mut heap);
        let target = host(&mut heap);
        let mut output = source.host;
        for _ in 0..8 {
            output = derived(&mut heap, &[output]);
        }
        let retained = heap
            .alloc_array(&[source.host, target.host, output])
            .unwrap();
        heap.add_root(RootOwner(1), RootKind::Host, retained)
            .unwrap();
        let collect = |heap: &mut Heap| {
            heap.begin_collection().unwrap();
            while heap.phase() != crate::GcPhase::Idle {
                heap.gc_step(crate::GcBudget {
                    units: 1,
                    ..Default::default()
                })
                .unwrap();
            }
        };
        collect(&mut heap);
        let baseline = heap.stats().live_objects;
        let cursor = heap
            .begin_property_cycle(
                target,
                PropertyId {
                    host: output,
                    field: 0,
                },
            )
            .unwrap();
        let root = heap.add_root(RootOwner(2), RootKind::Host, cursor).unwrap();
        for _ in 0..12 {
            heap.step_property_graph(cursor, &[], 128).unwrap();
        }
        assert!(heap.stats().live_objects > baseline);
        heap.release_root(root).unwrap();
        collect(&mut heap);
        assert_eq!(heap.stats().live_objects, baseline);
        assert_eq!(heap.host_property(target).unwrap().as_int(), Some(1));
    }
    #[test]
    fn disconnect_reservation_covers_the_actual_edge_removal_work() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = host(&mut heap);
        let inputs = (0..16)
            .map(|_| derived(&mut heap, &[source.host]))
            .collect::<Vec<_>>();
        let output = derived(&mut heap, &inputs);
        let cursor = heap.begin_property_disconnect(output).unwrap();
        let (_, reserved) = finish(&mut heap, cursor);
        heap.property_traversal_work.set(0);
        heap.disconnect_unobserved_derived(output).unwrap();
        let actual = heap.property_traversal_work.get();
        assert!(actual > 16 * 16);
        assert!(actual <= reserved, "actual={actual}, reserved={reserved}");
        assert!(
            heap.host_payload(output, DerivedSlot::Dependencies as u32)
                .unwrap()
                .same_bits(Value::NULL)
        );
    }
    #[test]
    fn reactive_notifications_remove_dead_pairs_at_their_known_position() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = host(&mut heap);
        let retained = heap.alloc_array(&[source.host]).unwrap();
        for index in 0..64 {
            let output = derived(&mut heap, &[source.host]);
            if index < 32 {
                heap.array_push(retained, output).unwrap();
            }
        }
        heap.add_root(RootOwner(1), RootKind::Host, retained)
            .unwrap();
        heap.begin_collection().unwrap();
        while heap.phase() != crate::GcPhase::Idle {
            heap.gc_step(crate::GcBudget::default()).unwrap();
        }
        heap.property_traversal_work.set(0);
        heap.invalidate_reactive(source.host, Some(0)).unwrap();
        assert_eq!(heap.property_traversal_work.get(), 64 * 8);
        let subscribers = heap
            .object(heap.key(source.host).unwrap())
            .unwrap()
            .reactive_subscribers;
        assert_eq!(heap.sequence_len(subscribers).unwrap(), 32 * 2);
    }
}
