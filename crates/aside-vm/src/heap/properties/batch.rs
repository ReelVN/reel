//! Joint proposals borrow rooted input values; reusable plans retain identities
//! and unpublished binding edges instead of copying a host or parameter table.
use super::*;

/// Stable property assignments whose values were evaluated once by the caller.
pub trait PropertyWrites {
    fn len(&self) -> usize;
    fn get(&self, index: usize) -> Option<(PropertyId, Value)>;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
impl PropertyWrites for [(PropertyId, Value)] {
    fn len(&self) -> usize {
        <[_]>::len(self)
    }
    fn get(&self, index: usize) -> Option<(PropertyId, Value)> {
        <[_]>::get(self, index).copied()
    }
}
impl<const N: usize> PropertyWrites for [(PropertyId, Value); N] {
    fn len(&self) -> usize {
        N
    }
    fn get(&self, index: usize) -> Option<(PropertyId, Value)> {
        self.as_slice().get(index).copied()
    }
}

/// A host's final proposed configuration. Cross-field validators use read,
/// host_property or table_entry; other heap operations retain their usual view.
pub struct PropertyRead<'a> {
    heap: &'a Heap,
    writes: Option<&'a dyn PropertyWrites>,
    prepared: bool,
    cursor: Option<Value>,
    work: Option<(&'a std::cell::Cell<u64>, u64)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(
        heap: &mut Heap,
        values: &[i64],
        writable: bool,
        validator: Option<&'static HostPropertyValidator>,
    ) -> Value {
        let fields = heap.alloc_table().unwrap();
        for (index, &value) in values.iter().enumerate() {
            heap.table_store(
                fields,
                index as u32,
                Value::int(index as i64),
                Value::int(value),
            )
            .unwrap();
        }
        let host = heap.alloc_host(fields, &[], validator).unwrap();
        heap.observe_host_properties(
            host,
            if writable { values.len() as u32 } else { 0 },
            if writable { 0 } else { values.len() as u32 },
        )
        .unwrap();
        host
    }
    fn id(host: Value, field: u32) -> PropertyId {
        PropertyId { host, field }
    }
    fn get(heap: &Heap, host: Value, field: u32) -> i64 {
        heap.host_property(id(host, field))
            .unwrap()
            .as_int()
            .unwrap()
    }

    #[test]
    fn authoritative_batch_publishes_readonly_values_and_keeps_connected_constraints_atomic() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = host(&mut heap, &[0, 10, 5], false, Some(&RANGE));
        let follower = host(&mut heap, &[5], true, None);
        let reference = heap.alloc_property_ref(id(source, 2), false).unwrap();
        heap.bind_host_property(id(follower, 0), reference).unwrap();
        let writes = [
            (id(source, 0), Value::int(10)),
            (id(source, 1), Value::int(20)),
            (id(source, 2), Value::int(15)),
        ];
        for script in [false, true] {
            assert!(matches!(
                heap.write_host_properties(&writes, script),
                Err(HeapError::ReadOnly)
            ));
        }
        heap.publish_host_properties(&writes).unwrap();
        assert_eq!(
            [
                get(&heap, source, 0),
                get(&heap, source, 1),
                get(&heap, source, 2),
                get(&heap, follower, 0)
            ],
            [10, 20, 15, 15]
        );
        let invalid = [
            (id(source, 0), Value::int(11)),
            (id(source, 2), Value::int(30)),
        ];
        assert!(matches!(
            heap.publish_host_properties(&invalid),
            Err(HeapError::TypeMismatch)
        ));
        assert_eq!(
            [
                get(&heap, source, 0),
                get(&heap, source, 1),
                get(&heap, source, 2),
                get(&heap, follower, 0)
            ],
            [10, 20, 15, 15]
        );
        let other = host(&mut heap, &[16], false, None);
        let replacement = heap.alloc_property_ref(id(other, 0), false).unwrap();
        assert!(matches!(
            heap.publish_host_properties(&[(id(source, 2), replacement)]),
            Err(HeapError::ReadOnly)
        ));
    }

    #[test]
    fn authoritative_batch_budget_failure_changes_no_values_and_warm_publication_reuses_storage() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let target = host(&mut heap, &[0, 0], false, None);
        let writes = [
            (id(target, 0), Value::int(1)),
            (id(target, 1), Value::int(2)),
        ];
        let usage = heap.budget_usage();
        let charge = usage.reserve(usage.remaining_bytes()).unwrap();
        assert!(heap.publish_host_properties(&writes).is_err());
        assert_eq!([get(&heap, target, 0), get(&heap, target, 1)], [0, 0]);
        drop(charge);
        heap.publish_host_properties(&writes).unwrap();
        let before = heap.stats();
        for value in 0..120 {
            heap.publish_host_properties(&[
                (id(target, 0), Value::int(value)),
                (id(target, 1), Value::int(value + 1)),
            ])
            .unwrap();
        }
        assert_eq!(heap.stats().chunk_allocations, before.chunk_allocations);
        assert_eq!(get(&heap, target, 1), 120);
    }
    #[test]
    fn prepared_read_admits_and_settles_each_index_query_even_when_the_read_fails() {
        for owned in [false, true] {
            let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
            let target = host(&mut heap, &[0; 4], true, None);
            let writes = std::array::from_fn::<_, 4, _>(|field| {
                (id(target, field as u32), Value::int(field as i64 + 10))
            });
            let cursor = if owned {
                let batch = heap.begin_property_batch(true).unwrap();
                while !matches!(
                    heap.step_property_batch(batch, &writes, 4096).unwrap(),
                    PropertyBatchProgress::Prepared { .. }
                ) {}
                Some(batch.value())
            } else {
                heap.prepare_property_batch(&writes, true, false).unwrap();
                None
            };
            let view = if let Some(cursor) = cursor {
                PropertyRead::cursor(&heap, &writes, cursor)
            } else {
                PropertyRead::prepared(&heap, &writes)
            };
            heap.property_index_work.set(37);
            let (result, work) = heap.property_validation_budget(0, || view.read(id(target, 0)));
            assert!(matches!(result, Err(HeapError::OperationLimit { .. })));
            assert_eq!(work, 0);
            assert_eq!(heap.property_index_work.get(), 37);
            let (result, work) = heap.property_validation_budget(128, || view.read(id(target, 0)));
            assert_eq!(result.unwrap().as_int(), Some(10));
            assert!(work > 0 && work <= 128);
            assert_eq!(heap.property_index_work.get(), 37);
            let (result, work) = heap.property_validation_budget(128, || view.read(id(target, 4)));
            assert!(matches!(result, Err(HeapError::OutOfBounds)));
            assert!(work > 0 && work <= 128);
            assert_eq!(heap.property_index_work.get(), 37);
        }
    }

    #[test]
    fn a_model_follower_defers_object_adoption_before_fast_publication() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = host(&mut heap, &[0], true, None);
        let follower = heap.alloc_table().unwrap();
        heap.table_store(follower, 0, Value::int(0), Value::int(0))
            .unwrap();
        heap.observe_model_object(follower).unwrap();
        let reference = heap.alloc_property_ref(id(source, 0), true).unwrap();
        heap.bind_host_property(id(follower, 0), reference).unwrap();
        let nested = heap.alloc_array(&[Value::int(7)]).unwrap();
        let value = heap.alloc_table().unwrap();
        heap.table_store(value, 0, Value::int(0), nested).unwrap();
        let writes = [(id(source, 0), value)];
        assert!(matches!(
            heap.try_write_host_properties(&writes, true, 4096).unwrap(),
            PropertyBatchProgress::Preparing { .. }
        ));
        assert_eq!(get(&heap, source, 0), 0);
        assert_eq!(get(&heap, follower, 0), 0);
        assert!(!heap.is_model_table(value).unwrap());
        let plan = heap.begin_property_batch(true).unwrap();
        while !matches!(
            heap.step_property_batch(plan, &writes, 4096).unwrap(),
            PropertyBatchProgress::Prepared { .. }
        ) {}
        assert!(matches!(
            heap.commit_property_batch(plan, &writes, u64::MAX).unwrap(),
            PropertyBatchProgress::Committed { .. }
        ));
        assert!(
            heap.host_property(id(follower, 0))
                .unwrap()
                .same_bits(value)
        );
        assert!(heap.is_model_table(value).unwrap());
        assert!(heap.object(heap.key(nested).unwrap()).unwrap().model);
    }
    static RANGE: HostPropertyValidator = HostPropertyValidator::new("test.range", 1, range);

    fn range(view: &PropertyRead<'_>, host: Value, _: u32, _: Value) -> Result<(), HeapError> {
        let fields = view.host_fields(host)?;
        let low = view
            .table_entry(fields, 0)?
            .1
            .as_int()
            .ok_or(HeapError::TypeMismatch)?;
        let high = view
            .table_entry(fields, 1)?
            .1
            .as_int()
            .ok_or(HeapError::TypeMismatch)?;
        let value = view
            .table_entry(fields, 2)?
            .1
            .as_int()
            .ok_or(HeapError::TypeMismatch)?;
        if low < high && low <= value && value <= high {
            Ok(())
        } else {
            Err(HeapError::TypeMismatch)
        }
    }

    #[test]
    fn joint_configuration_is_order_independent_and_invalid_values_publish_nothing() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        for order in [
            [0, 1, 2],
            [2, 0, 1],
            [1, 2, 0],
            [0, 2, 1],
            [1, 0, 2],
            [2, 1, 0],
        ] {
            let target = host(&mut heap, &[0, 1, 0], true, Some(&RANGE));
            let next = [10, 20, 15];
            let writes = order.map(|field| (id(target, field), Value::int(next[field as usize])));
            heap.write_host_properties(&writes, true).unwrap();
            assert_eq!(
                [
                    get(&heap, target, 0),
                    get(&heap, target, 1),
                    get(&heap, target, 2)
                ],
                next
            );
            assert_eq!(
                heap.write_host_properties(
                    &[
                        (id(target, 0), Value::int(100)),
                        (id(target, 2), Value::int(200))
                    ],
                    true
                ),
                Err(HeapError::TypeMismatch)
            );
            assert_eq!(
                [
                    get(&heap, target, 0),
                    get(&heap, target, 1),
                    get(&heap, target, 2)
                ],
                next
            );
        }
    }
    #[test]
    fn failed_rebinding_retains_old_value_and_subscription_and_valid_rebinding_replaces_them() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let sources = host(&mut heap, &[1, 20], true, None);
        let a = heap.alloc_property_ref(id(sources, 0), true).unwrap();
        let b = heap.alloc_property_ref(id(sources, 1), true).unwrap();
        let target = host(&mut heap, &[0, 10, 1], true, Some(&RANGE));
        heap.bind_host_property(id(target, 2), a).unwrap();
        assert_eq!(
            heap.write_host_properties(&[(id(target, 2), b)], true),
            Err(HeapError::TypeMismatch)
        );
        heap.write_host_property(id(sources, 0), Value::int(3), true)
            .unwrap();
        assert_eq!(get(&heap, target, 2), 3);
        heap.write_host_properties(
            &[
                (id(target, 0), Value::int(10)),
                (id(target, 1), Value::int(30)),
                (id(target, 2), b),
            ],
            true,
        )
        .unwrap();
        assert_eq!(get(&heap, target, 2), 20);
        heap.write_host_property(id(sources, 0), Value::int(4), true)
            .unwrap();
        assert_eq!(get(&heap, target, 2), 20);
        heap.write_host_property(id(sources, 1), Value::int(21), true)
            .unwrap();
        assert_eq!(get(&heap, target, 2), 21);
    }
    #[test]
    fn aliased_endpoints_reject_conflicting_values_and_coalesce_identical_values() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = host(&mut heap, &[1], true, None);
        let reference = heap.alloc_property_ref(id(source, 0), true).unwrap();
        let target = host(&mut heap, &[1, 1], true, None);
        heap.bind_host_property(id(target, 0), reference).unwrap();
        heap.bind_host_property(id(target, 1), reference).unwrap();
        assert_eq!(
            heap.write_host_properties(
                &[
                    (id(target, 0), Value::int(2)),
                    (id(target, 1), Value::int(3))
                ],
                true
            ),
            Err(HeapError::PropertyConflict)
        );
        assert_eq!(
            [
                get(&heap, source, 0),
                get(&heap, target, 0),
                get(&heap, target, 1)
            ],
            [1, 1, 1]
        );
        heap.write_host_properties(
            &[
                (id(target, 1), Value::int(4)),
                (id(target, 0), Value::int(4)),
            ],
            true,
        )
        .unwrap();
        assert_eq!(
            [
                get(&heap, source, 0),
                get(&heap, target, 0),
                get(&heap, target, 1)
            ],
            [4, 4, 4]
        );
    }
    #[test]
    fn candidate_bindings_see_other_provided_values_and_reject_new_cycles() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let target = host(&mut heap, &[1, 2], true, None);
        let a = heap.alloc_property_ref(id(target, 0), true).unwrap();
        let b = heap.alloc_property_ref(id(target, 1), true).unwrap();
        assert_eq!(
            heap.write_host_properties(&[(id(target, 0), b), (id(target, 1), a)], true),
            Err(HeapError::BindingCycle)
        );
        assert_eq!([get(&heap, target, 0), get(&heap, target, 1)], [1, 2]);
        heap.write_host_properties(&[(id(target, 0), b), (id(target, 1), Value::int(7))], true)
            .unwrap();
        assert_eq!([get(&heap, target, 0), get(&heap, target, 1)], [7, 7]);
        heap.write_host_property(id(target, 1), Value::int(8), true)
            .unwrap();
        assert_eq!(get(&heap, target, 0), 8);
    }
    #[test]
    fn readonly_sources_cannot_accept_scalars_or_replace_a_required_writeback_binding() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let source = host(&mut heap, &[5], false, None);
        let readonly = heap.alloc_property_ref(id(source, 0), false).unwrap();
        let target = host(&mut heap, &[0, 0], true, None);
        heap.require_writable_property(id(target, 0)).unwrap();
        assert_eq!(
            heap.write_host_properties(
                &[(id(target, 0), readonly), (id(target, 1), Value::int(3))],
                true
            ),
            Err(HeapError::ReadOnly)
        );
        assert_eq!([get(&heap, target, 0), get(&heap, target, 1)], [0, 0]);
        heap.write_host_properties(&[(id(target, 1), readonly)], true)
            .unwrap();
        assert_eq!(
            heap.write_host_properties(&[(id(target, 1), Value::int(9))], true),
            Err(HeapError::ReadOnly)
        );
        assert_eq!(get(&heap, target, 1), 5);
    }
    #[test]
    fn repeated_scalar_configurations_reuse_charged_plan_capacity() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let target = host(&mut heap, &[0, 1, 0], true, Some(&RANGE));
        heap.write_host_properties(
            &[
                (id(target, 0), Value::int(10)),
                (id(target, 1), Value::int(20)),
                (id(target, 2), Value::int(15)),
            ],
            true,
        )
        .unwrap();
        let before = heap.stats();
        for _ in 0..1000 {
            heap.write_host_properties(
                &[
                    (id(target, 2), Value::int(16)),
                    (id(target, 1), Value::int(21)),
                ],
                true,
            )
            .unwrap();
        }
        assert_eq!(heap.stats().charged_bytes, before.charged_bytes);
        assert_eq!(heap.stats().chunk_allocations, before.chunk_allocations);
    }
    #[test]
    fn derived_dependencies_follow_candidate_and_existing_alias_paths_before_publication() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let target = host(&mut heap, &[1, 1], true, None);
        let tail = heap.alloc_property_ref(id(target, 1), true).unwrap();
        heap.bind_host_property(id(target, 0), tail).unwrap();
        let output = heap.alloc_derived(Value::NULL).unwrap();
        heap.begin_derived(output).unwrap();
        heap.observe_read(target, Some(0)).unwrap();
        heap.commit_derived(output, Value::int(2)).unwrap();
        let reference = heap.alloc_property_ref(id(output, 0), false).unwrap();
        assert_eq!(
            heap.write_host_properties(&[(id(target, 1), reference)], true),
            Err(HeapError::BindingCycle)
        );
        assert_eq!([get(&heap, target, 0), get(&heap, target, 1)], [1, 1]);
        let next = host(&mut heap, &[0, 0], true, None);
        let next_tail = heap.alloc_property_ref(id(next, 1), true).unwrap();
        let computed = heap.alloc_derived(Value::NULL).unwrap();
        heap.begin_derived(computed).unwrap();
        heap.observe_read(next, Some(0)).unwrap();
        heap.commit_derived(computed, Value::int(2)).unwrap();
        let computed_ref = heap.alloc_property_ref(id(computed, 0), false).unwrap();
        assert_eq!(
            heap.write_host_properties(
                &[(id(next, 0), next_tail), (id(next, 1), computed_ref)],
                true
            ),
            Err(HeapError::BindingCycle)
        );
        assert_eq!([get(&heap, next, 0), get(&heap, next, 1)], [0, 0]);
    }
    #[test]
    fn replacing_an_old_edge_uses_final_seeds_independently_of_input_order() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        for reverse in [false, true] {
            let sources = host(&mut heap, &[1, 2], true, None);
            let target = host(&mut heap, &[1], true, None);
            let old = heap.alloc_property_ref(id(sources, 0), true).unwrap();
            let new = heap.alloc_property_ref(id(sources, 1), true).unwrap();
            heap.bind_host_property(id(target, 0), old).unwrap();
            let mut values = [
                (id(sources, 0), Value::int(3)),
                (id(target, 0), new),
                (id(sources, 1), Value::int(4)),
            ];
            if reverse {
                values.reverse();
            }
            heap.write_host_properties(&values, true).unwrap();
            assert_eq!(
                [
                    get(&heap, sources, 0),
                    get(&heap, sources, 1),
                    get(&heap, target, 0)
                ],
                [3, 4, 4]
            );
        }
    }
    #[test]
    fn binding_revision_exhaustion_is_rejected_before_detaching_the_old_connection() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let sources = host(&mut heap, &[1, 2], true, None);
        let target = host(&mut heap, &[1], true, None);
        let old = heap.alloc_property_ref(id(sources, 0), true).unwrap();
        let new = heap.alloc_property_ref(id(sources, 1), true).unwrap();
        heap.bind_host_property(id(target, 0), old).unwrap();
        heap.binding_revision = u64::MAX - 1;
        assert_eq!(
            heap.write_host_properties(&[(id(target, 0), new)], true),
            Err(HeapError::IdentityExhausted)
        );
        assert!(heap.property_binding(id(target, 0)).unwrap().same_bits(old));
        assert_eq!(get(&heap, target, 0), 1);
    }
}
impl std::ops::Deref for PropertyRead<'_> {
    type Target = Heap;
    fn deref(&self) -> &Heap {
        self.heap
    }
}
impl<'a> PropertyRead<'a> {
    pub fn current(heap: &'a Heap) -> Self {
        Self {
            heap,
            writes: None,
            prepared: false,
            cursor: None,
            work: None,
        }
    }
    pub(in crate::heap) fn proposed(heap: &'a Heap, writes: &'a dyn PropertyWrites) -> Self {
        Self {
            heap,
            writes: Some(writes),
            prepared: false,
            cursor: None,
            work: None,
        }
    }
    fn prepared(heap: &'a Heap, writes: &'a dyn PropertyWrites) -> Self {
        Self {
            heap,
            writes: Some(writes),
            prepared: true,
            cursor: None,
            work: None,
        }
    }
    pub(super) fn cursor(heap: &'a Heap, writes: &'a dyn PropertyWrites, cursor: Value) -> Self {
        Self {
            heap,
            writes: Some(writes),
            prepared: true,
            cursor: Some(cursor),
            work: None,
        }
    }
    fn input(&self, index: usize) -> Result<(PropertyId, Value), HeapError> {
        self.tick(1)?;
        self.writes
            .and_then(|writes| writes.get(index))
            .ok_or(HeapError::OutOfBounds)
    }
    fn limited(
        heap: &'a Heap,
        writes: &'a dyn PropertyWrites,
        work: &'a std::cell::Cell<u64>,
        limit: u64,
    ) -> Self {
        let mut view = Self::proposed(heap, writes);
        view.work = Some((work, limit));
        view
    }
    fn tick(&self, amount: u64) -> Result<(), HeapError> {
        if let Some((work, limit)) = self.work {
            let next = work.get() + amount;
            if next > limit {
                return Err(HeapError::OperationLimit {
                    requested: next,
                    limit,
                });
            }
            work.set(next);
        }
        Ok(())
    }
    fn replacement(&self, property: PropertyId) -> Result<Option<Value>, HeapError> {
        let Some(writes) = self.writes else {
            return Ok(None);
        };
        let mut result = None;
        for index in 0..writes.len() {
            let (target, value) = self.input(index)?;
            if target.same(property) {
                if result.is_some() {
                    return Err(HeapError::PropertyConflict);
                }
                result = Some(value);
            }
        }
        Ok(result)
    }
    fn edge(&self, property: PropertyId) -> Result<(Value, bool), HeapError> {
        if let Some(value) = self.replacement(property)?
            && self.heap.is_property_ref(value)
        {
            return Ok((value, true));
        }
        Ok((self.heap.property_binding(property)?, false))
    }
    pub(in crate::heap) fn source(
        &self,
        property: PropertyId,
    ) -> Result<(PropertyId, bool), HeapError> {
        self.tick(4)?;
        self.source_avoiding(property, None)
    }
    pub(in crate::heap) fn source_avoiding(
        &self,
        mut property: PropertyId,
        forbidden: Option<PropertyId>,
    ) -> Result<(PropertyId, bool), HeapError> {
        let object = self.heap.object(self.heap.key(property.host)?)?;
        let mut writable = if property.field < object.observable_fields {
            self.heap.property_access(property)?
        } else {
            self.heap.model_position(property)?;
            true
        };
        let mut changed_edges = 0;
        loop {
            self.tick(4)?;
            if forbidden.is_some_and(|target| target.same(property)) {
                return Err(HeapError::BindingCycle);
            }
            let (binding, proposed) = self.edge(property)?;
            if binding.same_bits(Value::NULL) {
                return Ok((property, writable));
            }
            if proposed {
                changed_edges += 1;
                // Existing connections are acyclic; a new cycle must revisit
                // an edge introduced by this finite proposal.
                if changed_edges > self.writes.map_or(0, |writes| writes.len()) {
                    return Err(HeapError::BindingCycle);
                }
            }
            let (source, permission) = self.heap.property_ref(binding)?;
            writable &= permission && self.heap.property_access(source)?;
            property = source;
        }
    }
    /// Resolve candidate bindings and scalar writes, rejecting distinct values
    /// that would write the same final binding endpoint.
    pub fn read(&self, property: PropertyId) -> Result<Value, HeapError> {
        self.tick(2)?;
        if self.prepared {
            let required = if let Some(cursor) = self.cursor {
                self.heap.cursor_property_read_work(cursor)?
            } else {
                self.heap.property_batch.len() as u64 + 1
            };
            self.heap.property_validation_admission(required)?;
            let before = self.heap.property_index_work.get();
            let result = if let Some(cursor) = self.cursor {
                self.heap.cursor_property_read(cursor, property)
            } else {
                self.heap.batch_position(property).map_or_else(
                    || self.heap.host_property(property),
                    |index| Ok(self.heap.property_batch[index].value),
                )
            };
            // The validator owns this lookup, including a failed read. Restore
            // the enclosing operation's counter before settling its exact cost.
            let used = self.heap.property_index_work.get() - before;
            self.heap.property_index_work.set(before);
            debug_assert!(used <= required);
            self.heap.charge_property_validation_work(used)?;
            return result;
        }
        let Some(writes) = self.writes else {
            return self.heap.host_property(property);
        };
        let (source, _) = self.source(property)?;
        let mut result = None;
        for index in 0..writes.len() {
            let (target, value) = self.input(index)?;
            if !self.heap.is_property_ref(value) && self.source(target)?.0.same(source) {
                if result.is_some_and(|previous: Value| !previous.same_bits(value)) {
                    return Err(HeapError::PropertyConflict);
                }
                result = Some(value);
            }
        }
        result.map_or_else(|| self.heap.host_property(source), Ok)
    }
    pub fn host_property(&self, property: PropertyId) -> Result<Value, HeapError> {
        self.read(property)
    }
    /// Read a host field table through the same complete proposal.
    pub fn table_entry(&self, table: Value, index: u32) -> Result<(Value, Value), HeapError> {
        let (key, value) = self.heap.table_entry(table, index)?;
        let Some(owner) = self.heap.object(self.heap.key(table)?)?.property_owner else {
            return Ok((key, value));
        };
        let host = Value::object(self.heap.id, owner.data().as_ffi());
        let field = self.heap.model_field(host, index)?;
        Ok((key, self.read(PropertyId { host, field })?))
    }
}

#[derive(Clone, Copy)]
pub(in crate::heap) struct PreparedProperty {
    property: PropertyId,
    value: Value,
    rebound: bool,
}

#[derive(Clone, Copy)]
pub(in crate::heap) struct PreparedBinding {
    pub(super) target: PropertyId,
    pub(super) source: PropertyId,
    pub(super) input: usize,
    list: Value,
    weak: Value,
    position: u32,
}

impl Heap {
    /// Work observed for the latest synchronous bounded property operation.
    /// Read immediately after it returns; resumable state belongs to each plan.
    pub fn property_configuration_work(&self) -> u64 {
        self.property_work_used.get()
    }
    /// Start a host admission attempt, including argument and owner validation.
    pub fn begin_property_configuration_work(&self) {
        self.property_work_used.set(0);
    }
    fn batch_key(property: PropertyId) -> (u64, u32) {
        let (_, key) = property
            .host
            .object_parts()
            .expect("validated property host");
        (key, property.field)
    }
    fn batch_position(&self, property: PropertyId) -> Option<usize> {
        self.property_index_work
            .set(self.property_index_work.get() + 1);
        self.property_batch_index
            .get(&Self::batch_key(property))
            .copied()
    }
    fn reserve_batch_index(&mut self) -> Result<(), HeapError> {
        if self.property_batch_index.len() < self.property_batch_index.capacity() {
            return Ok(());
        }
        let old = self.property_batch_index.allocation_size();
        // hashbrown 0.17 grows its power-of-two bucket array by doubling. Admit
        // both arrays during relocation; include control bytes and alignment.
        let buckets = (self.property_batch_index.capacity() + 1)
            .next_power_of_two()
            .saturating_mul(2)
            .max(4);
        let peak = buckets
            .saturating_mul(size_of::<((u64, u32), usize)>() + 1)
            .saturating_add(64);
        let reservation = self.usage.reserve(peak as u64)?;
        self.property_batch_index
            .try_reserve(1)
            .map_err(|_| HeapError::AllocationFailure)?;
        let allocated = self.property_batch_index.allocation_size();
        debug_assert!(allocated <= peak);
        drop(reservation);
        let additional = (allocated - old) as u64;
        self.usage.charge(additional);
        self.reserved += additional;
        self.property_index_work
            .set(self.property_index_work.get() + buckets as u64);
        Ok(())
    }
    fn batch_push(
        &mut self,
        property: PropertyId,
        value: Value,
        rebound: bool,
    ) -> Result<(), HeapError> {
        if let Some(index) = self.batch_position(property) {
            let previous = &mut self.property_batch[index];
            // A candidate connection replaces its old incoming edge. Traversing
            // that stale subscriber edge must not overwrite the seeded value.
            if !previous.value.same_bits(value) && !previous.rebound {
                return Err(HeapError::PropertyConflict);
            }
            previous.rebound |= rebound;
            return Ok(());
        }
        self.reserve_batch_index()?;
        if self.property_batch.len() == self.property_batch.capacity() {
            let old = self.property_batch.capacity();
            let new = (old * 2).max(8);
            self.ensure_bytes(((new - old) * std::mem::size_of::<PreparedProperty>()) as u64)?;
            self.property_batch
                .try_reserve_exact(new - old)
                .map_err(|_| HeapError::AllocationFailure)?;
            let bytes = ((self.property_batch.capacity() - old)
                * std::mem::size_of::<PreparedProperty>()) as u64;
            self.usage.charge(bytes);
            self.reserved += bytes;
        }
        self.property_batch.push(PreparedProperty {
            property,
            value,
            rebound,
        });
        self.property_batch_index
            .insert(Self::batch_key(property), self.property_batch.len() - 1);
        self.property_index_work
            .set(self.property_index_work.get() + 1);
        Ok(())
    }
    fn clear_property_batch(&mut self) {
        self.property_batch_index.clear();
        self.property_batch.clear();
    }
    fn reserve_binding_plan(&mut self) -> Result<(), HeapError> {
        if self.property_bindings.len() == self.property_bindings.capacity() {
            let old = self.property_bindings.capacity();
            let new = (old * 2).max(4);
            self.ensure_bytes(((new - old) * std::mem::size_of::<PreparedBinding>()) as u64)?;
            self.property_bindings
                .try_reserve_exact(new - old)
                .map_err(|_| HeapError::AllocationFailure)?;
            let bytes = ((self.property_bindings.capacity() - old)
                * std::mem::size_of::<PreparedBinding>()) as u64;
            self.usage.charge(bytes);
            self.reserved += bytes;
        }
        Ok(())
    }
    pub(super) fn discard_binding_plan(&mut self) -> Result<(), HeapError> {
        while let Some(prepared) = self.property_bindings.pop() {
            debug_assert!(
                self.sequence_get(prepared.list, prepared.position)?
                    .same_bits(prepared.weak)
            );
            self.remove_subscriber(prepared.list, prepared.position)?;
            self.property_traversal_work
                .set(self.property_traversal_work.get() + 16);
        }
        Ok(())
    }
    pub(super) fn prepare_batch_binding(
        &mut self,
        target: PropertyId,
        reference: Value,
        input: usize,
    ) -> Result<(), HeapError> {
        let (source, _) = self.property_ref(reference)?;
        self.reserve_binding_plan()?;
        self.ensure_property_metadata(target)?;
        let source_meta = self.ensure_property_metadata(source)?;
        let mut list = self.sequence_get(source_meta, Slot::Subscribers as u32)?;
        if list.same_bits(Value::NULL) {
            list = self.alloc_array(&[])?;
            self.set_field(source_meta, Slot::Subscribers as u32, list)?;
        }
        let weak = self.alloc_weakref(target.host)?;
        let position = self.sequence_len(list)?;
        self.packed_pair_push(list, weak, Value::int(i64::from(target.field)))?;
        self.property_bindings.push(PreparedBinding {
            target,
            source,
            input,
            list,
            weak,
            position,
        });
        Ok(())
    }
    /// Jointly validate and publish a configuration through the shared property
    /// graph. Inputs stay borrowed; plans retain their charged capacity for reuse.
    pub fn write_host_properties(
        &mut self,
        writes: &dyn PropertyWrites,
        script: bool,
    ) -> Result<(), HeapError> {
        self.apply_host_properties(writes, script, false)
    }

    /// Jointly publish authoritative service values, including read-only fields.
    /// Connected constraints and all storage are prepared before any value changes.
    /// Binding inputs retain their ordinary write permissions. Preparation reuses
    /// the same charged scratch storage as `write_host_properties`.
    pub fn publish_host_properties(
        &mut self,
        writes: &dyn PropertyWrites,
    ) -> Result<(), HeapError> {
        self.apply_host_properties(writes, false, true)
    }

    fn apply_host_properties(
        &mut self,
        writes: &dyn PropertyWrites,
        script: bool,
        publication: bool,
    ) -> Result<(), HeapError> {
        self.property_work_used.set(0);
        self.clear_property_batch();
        self.property_bindings.clear();
        if let Err(error) = self.prepare_property_batch(writes, script, publication) {
            self.discard_binding_plan()?;
            self.clear_property_batch();
            return Err(error);
        }
        self.commit_prepared_batch(writes, script)
    }
    /// Attempt a small configuration using reusable charged scratch storage.
    /// Larger graphs and every derived dependency continue through a per-call
    /// PropertyBatch. A deferred attempt has published no values or edges.
    pub fn try_write_host_properties(
        &mut self,
        writes: &dyn PropertyWrites,
        script: bool,
        work_budget: u64,
    ) -> Result<super::cursor::PropertyBatchProgress, HeapError> {
        self.property_work_used.set(0);
        let traversal = self.property_traversal_work.get();
        use super::cursor::{PropertyBatch, PropertyBatchProgress};
        const MAX_WRITES: usize = 8;
        const MAX_PROPERTIES: usize = 32;
        if writes.len() > MAX_WRITES || work_budget < 2048 {
            return Ok(PropertyBatchProgress::Preparing {
                work: 0,
                required_work: PropertyBatch::INITIAL_WORK,
            });
        }
        self.clear_property_batch();
        self.property_bindings.clear();
        self.property_index_work.set(0);
        let work = std::cell::Cell::new(0u64);
        let limit = work_budget.saturating_sub(1024).min(2048);
        let proposal = (|| -> Result<bool, HeapError> {
            for index in 0..writes.len() {
                work.set(work.get() + 4);
                let (target, input) = writes.get(index).ok_or(HeapError::OutOfBounds)?;
                let (source, writable) =
                    PropertyRead::limited(self, writes, &work, limit).source(target)?;
                if self.object(self.key(source.host)?)?.derived {
                    return Ok(false);
                }
                let rebound = self.is_property_ref(input);
                if rebound {
                    if !self.property_access(target)? {
                        return Err(HeapError::ReadOnly);
                    }
                    let old = self.property_binding(target)?;
                    if !old.same_bits(Value::NULL)
                        && self
                            .object(self.key(self.property_ref(old)?.0.host)?)?
                            .derived
                    {
                        return Ok(false);
                    }
                } else if !writable {
                    return Err(HeapError::ReadOnly);
                }
                let value = PropertyRead::limited(self, writes, &work, limit).read(target)?;
                if self.object(self.key(source.host)?)?.model && self.model_candidate(value)? {
                    return Ok(false);
                }
                if !rebound {
                    self.batch_push(source, value, false)?;
                }
                self.batch_push(target, value, rebound)?;
            }
            let mut cursor = 0;
            while cursor < self.property_batch.len() {
                let source = self.property_batch[cursor];
                cursor += 1;
                work.set(work.get() + 16);
                let list = self.subscribers(source.property)?;
                let mut index = 0;
                while !list.same_bits(Value::NULL) && index < self.sequence_len(list)? {
                    if work.get() + self.property_index_work.get() + 64 > limit {
                        return Ok(false);
                    }
                    work.set(work.get() + 16);
                    let weak = self.sequence_get(list, index)?;
                    let field = self
                        .sequence_get(list, index + 1)?
                        .as_int()
                        .ok_or(HeapError::TypeMismatch)? as u32;
                    if let Some(host) = self.subscriber_target(weak)?
                        && self.property_fields(host).is_ok()
                    {
                        self.batch_push(PropertyId { host, field }, source.value, false)?;
                    }
                    if self.property_batch.len() > MAX_PROPERTIES {
                        return Ok(false);
                    }
                    index += 2;
                }
            }
            for &prepared in &self.property_batch {
                if self.object(self.key(prepared.property.host)?)?.model
                    && self.model_candidate(prepared.value)?
                {
                    return Ok(false);
                }
                let metadata = self.property_metadata(prepared.property)?;
                let requires_write = !metadata.same_bits(Value::NULL)
                    && self
                        .sequence_get(metadata, Slot::RequiresWrite as u32)?
                        .as_bool()
                        == Some(true);
                let worst = if requires_write {
                    limit + MAX_PROPERTIES as u64 * (MAX_WRITES as u64 + 4) + 256
                } else {
                    256
                };
                if work.get() + self.property_index_work.get() + worst > work_budget {
                    return Ok(false);
                }
                work.set(work.get() + 16);
                let (result, validation_work) = self.property_validation_budget(256, || {
                    self.validate_prepared_property(writes, prepared, Some(&work))
                });
                work.set(work.get() + validation_work);
                result?;
            }
            Ok(true)
        })();
        let used = work.get() + self.property_index_work.get();
        self.property_work_used.set(used);
        let ready = match proposal {
            Ok(ready) => ready,
            Err(HeapError::OperationLimit { .. }) => false,
            Err(error) => {
                self.clear_property_batch();
                return Err(error);
            }
        };
        if !ready {
            self.clear_property_batch();
            return Ok(PropertyBatchProgress::Preparing {
                work: used,
                required_work: PropertyBatch::INITIAL_WORK,
            });
        }
        let mut publication = 32 + (writes.len() as u64) * 16;
        for prepared in &self.property_batch {
            let reactive = self
                .object(self.key(prepared.property.host)?)?
                .reactive_subscribers;
            publication += 32
                + self.property_storage_work(prepared.property, script)?
                + if reactive.same_bits(Value::NULL) {
                    0
                } else {
                    u64::from(self.sequence_len(reactive)? / 2) * 8
                };
        }
        for index in 0..writes.len() {
            let (target, input) = writes.get(index).ok_or(HeapError::OutOfBounds)?;
            if self.is_property_ref(input) {
                let direct = self.property_ref(input)?.0;
                publication += 128
                    + u64::from(self.property_storage_len(target.host)?) * 4
                    + u64::from(self.property_storage_len(direct.host)?) * 4;
                let old = self.property_binding(target)?;
                if !old.same_bits(Value::NULL) {
                    let list = self.subscribers(self.property_ref(old)?.0)?;
                    if !list.same_bits(Value::NULL) {
                        publication += u64::from(self.sequence_len(list)? / 2) * 8;
                    }
                }
            }
        }
        if work_budget.saturating_sub(used) < publication {
            self.clear_property_batch();
            return Ok(PropertyBatchProgress::Preparing {
                work: used,
                required_work: PropertyBatch::INITIAL_WORK,
            });
        }
        if let Err(error) = self.prepare_prepared_batch(writes, script, true) {
            self.discard_binding_plan()?;
            self.clear_property_batch();
            self.property_work_used.set(
                self.property_work_used.get() + self.property_traversal_work.get() - traversal,
            );
            return Err(error);
        }
        self.commit_prepared_batch(writes, script)?;
        self.property_work_used
            .set(self.property_work_used.get() + self.property_traversal_work.get() - traversal);
        Ok(PropertyBatchProgress::Committed {
            work: self.property_work_used.get(),
        })
    }
    fn commit_prepared_batch(
        &mut self,
        writes: &dyn PropertyWrites,
        script: bool,
    ) -> Result<(), HeapError> {
        self.property_work_used
            .set(self.property_work_used.get() + 16);
        for index in 0..self.property_bindings.len() {
            self.property_work_used
                .set(self.property_work_used.get() + 32);
            let prepared = self.property_bindings[index];
            let reference = writes.get(prepared.input).ok_or(HeapError::OutOfBounds)?.1;
            self.detach_property(prepared.target)?;
            self.binding_changed()?;
            let metadata = self.property_metadata(prepared.target)?;
            self.set_field(metadata, Slot::Binding as u32, reference)?;
            self.reconnect_derived(prepared.source.host)?;
        }
        for index in 0..self.property_batch.len() {
            self.property_work_used
                .set(self.property_work_used.get() + 16);
            let prepared = self.property_batch[index];
            self.commit_property_value(prepared.property, prepared.value, script)?;
        }
        self.clear_property_batch();
        self.property_bindings.clear();
        Ok(())
    }
    fn prepare_property_batch(
        &mut self,
        writes: &dyn PropertyWrites,
        script: bool,
        publication: bool,
    ) -> Result<(), HeapError> {
        for index in 0..writes.len() {
            let (target, value) = writes.get(index).ok_or(HeapError::OutOfBounds)?;
            let rebound = self.is_property_ref(value);
            self.property_fields(target.host)?;
            let (source, writable) = PropertyRead::proposed(self, writes).source(target)?;
            if self.is_property_ref(value) {
                if !self.property_access(target)? {
                    return Err(HeapError::ReadOnly);
                }
                self.check_derived_configuration_cycle(target, source, Some(writes))?;
            } else if !writable && !publication {
                return Err(HeapError::ReadOnly);
            }
            let value = PropertyRead::proposed(self, writes).read(target)?;
            if !rebound {
                self.batch_push(source, value, false)?;
            }
            self.batch_push(target, value, rebound)?;
        }
        let mut cursor = 0;
        while cursor < self.property_batch.len() {
            let source = self.property_batch[cursor];
            cursor += 1;
            let list = self.subscribers(source.property)?;
            let mut index = 0;
            while !list.same_bits(Value::NULL) && index < self.sequence_len(list)? {
                let weak = self.sequence_get(list, index)?;
                let field = self
                    .sequence_get(list, index + 1)?
                    .as_int()
                    .ok_or(HeapError::TypeMismatch)? as u32;
                if let Some(host) = self.subscriber_target(weak)?
                    && self.property_fields(host).is_ok()
                {
                    self.batch_push(PropertyId { host, field }, source.value, false)?;
                }
                index += 2;
            }
        }
        self.validate_prepared_batch(writes)?;
        self.prepare_prepared_batch(writes, script, false)
    }
    fn validate_prepared_batch(&self, writes: &dyn PropertyWrites) -> Result<(), HeapError> {
        for &prepared in &self.property_batch {
            self.validate_prepared_property(writes, prepared, None)?;
        }
        Ok(())
    }
    fn validate_prepared_property(
        &self,
        writes: &dyn PropertyWrites,
        prepared: PreparedProperty,
        work: Option<&std::cell::Cell<u64>>,
    ) -> Result<(), HeapError> {
        let property = prepared.property;
        let mut view = PropertyRead::prepared(self, writes);
        view.work = work.map(|work| (work, u64::MAX));
        let value = prepared.value;
        self.validate_property_proposal(&view, property, value)?;
        let metadata = self.property_metadata(property)?;
        if !metadata.same_bits(Value::NULL)
            && self
                .sequence_get(metadata, Slot::RequiresWrite as u32)?
                .as_bool()
                == Some(true)
            && !view.source(property)?.1
        {
            return Err(HeapError::ReadOnly);
        }
        Ok(())
    }
    pub(super) fn validate_property_proposal(
        &self,
        view: &PropertyRead<'_>,
        property: PropertyId,
        value: Value,
    ) -> Result<(), HeapError> {
        if self.is_model_cell(property.host)? {
            self.validate_host_property(property.host, property.field, value)
        } else if let Some(validate) = self.object(self.key(property.host)?)?.host_validator {
            validate.validate(view, property.host, property.field, value)
        } else {
            Ok(())
        }
    }
    fn property_validation_admission(&self, amount: u64) -> Result<u64, HeapError> {
        let requested = self.property_validation_work.get().saturating_add(amount);
        let limit = self.property_validation_limit.get();
        if requested > limit {
            return Err(HeapError::OperationLimit { requested, limit });
        }
        Ok(requested)
    }
    /// Charge content inspected by a variable-sized native property validator.
    /// Ordinary property assignment retains its existing unlimited synchronous
    /// validation. Bounded configuration prepares retry with a larger grant.
    pub fn charge_property_validation_work(&self, amount: u64) -> Result<(), HeapError> {
        let requested = self.property_validation_admission(amount)?;
        self.property_validation_work.set(requested);
        Ok(())
    }
    pub(super) fn property_validation_budget<T>(
        &self,
        limit: u64,
        validate: impl FnOnce() -> Result<T, HeapError>,
    ) -> (Result<T, HeapError>, u64) {
        self.property_validation_work.set(0);
        self.property_validation_limit.set(limit);
        let result = validate();
        let work = self.property_validation_work.get();
        self.property_validation_limit.set(u64::MAX);
        self.property_validation_work.set(0);
        (result, work)
    }
    pub(super) fn prepare_property_publication(
        &mut self,
        property: PropertyId,
        value: Value,
        script: bool,
    ) -> Result<(), HeapError> {
        self.prepare_property_storage(property, value, script)?;
        self.adopt_model_value(property.host, value)
    }
    pub(super) fn prepare_property_storage(
        &mut self,
        property: PropertyId,
        value: Value,
        script: bool,
    ) -> Result<(), HeapError> {
        self.barrier(value)?;
        if script
            && !self
                .property_versions(property.host)?
                .same_bits(Value::NULL)
        {
            self.track_host_writes(property.host)?;
        }
        Ok(())
    }
    pub(super) fn property_storage_work(
        &self,
        property: PropertyId,
        script: bool,
    ) -> Result<u64, HeapError> {
        let versions = self.property_versions(property.host)?;
        if !script || versions.same_bits(Value::NULL) {
            return Ok(0);
        }
        let fields = if self.is_model_table(property.host)? {
            self.table_len(property.host)?
        } else if self.is_model_cell(property.host)? {
            1
        } else {
            self.table_len(self.property_fields(property.host)?)?
        };
        Ok(u64::from(fields.saturating_sub(self.sequence_len(versions)?)) * 4)
    }
    fn prepare_prepared_batch(
        &mut self,
        writes: &dyn PropertyWrites,
        script: bool,
        graphs_prepared: bool,
    ) -> Result<(), HeapError> {
        for index in 0..self.property_batch.len() {
            self.property_work_used
                .set(self.property_work_used.get() + 16);
            let prepared = self.property_batch[index];
            let property = prepared.property;
            let value = prepared.value;
            self.prepare_property_publication(property, value, script)?;
        }
        let mut count = 0u64;
        for index in 0..writes.len() {
            self.property_work_used
                .set(self.property_work_used.get() + 4);
            let (target, value) = writes.get(index).ok_or(HeapError::OutOfBounds)?;
            if self.is_property_ref(value) {
                let previous = self.property_binding(target)?;
                count += 1 + u64::from(!previous.same_bits(Value::NULL));
                if !graphs_prepared && !previous.same_bits(Value::NULL) {
                    self.prepare_derived_disconnect(self.property_ref(previous)?.0.host)?;
                }
            }
        }
        self.binding_revision
            .checked_add(count)
            .ok_or(HeapError::IdentityExhausted)?;
        for index in 0..writes.len() {
            self.property_work_used
                .set(self.property_work_used.get() + 4);
            let (target, value) = writes.get(index).ok_or(HeapError::OutOfBounds)?;
            if self.is_property_ref(value) {
                self.property_work_used
                    .set(self.property_work_used.get() + 32);
                self.prepare_batch_binding(target, value, index)?;
            }
        }
        Ok(())
    }
}
