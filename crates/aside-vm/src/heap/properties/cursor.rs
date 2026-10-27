//! Per-call property proposals use ordinary rooted arrays and a byte index.
//! These containers publish neither host values nor subscription edges.
use super::*;
use crate::heap::model::ModelAdoptionProgress;
use crate::heap::reactive::PropertyGraphProgress;

#[cfg(test)]
#[path = "cursor/publication_tests.rs"]
mod publication_tests;

/// A private proposal retained in the calling native's continuation values.
/// Keeping this value rooted preserves its plan across GC and snapshots.
#[derive(Clone, Copy, Debug)]
pub struct PropertyBatch(Value);
impl PropertyBatch {
    /// Work admitted before creating a private per-call continuation plan.
    pub const INITIAL_WORK: u64 = 32;
    pub fn value(self) -> Value {
        self.0
    }
    pub fn from_value(value: Value) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_index_handles_branch_writes_split_at_each_byte_limit() {
        for bytes in [1, 3, 4, 5, 12, 4096] {
            let mut heap = Heap::new(
                SessionEpoch(1),
                HeapLimits {
                    max_mutation_bytes: bytes,
                    ..Default::default()
                },
            )
            .unwrap();
            let source = host(&mut heap, 0);
            let reference = heap.alloc_property_ref(source, true).unwrap();
            let mut followers = Vec::new();
            for _ in 0..64 {
                let follower = host(&mut heap, 0);
                heap.bind_host_property(follower, reference).unwrap();
                followers.push(follower);
            }
            let plan = heap.begin_property_batch(true).unwrap();
            let writes = [(source, Value::int(7))];
            loop {
                let allowance = heap
                    .property_batch_required_work(plan, &writes)
                    .unwrap()
                    .max(32);
                if let PropertyBatchProgress::Prepared {
                    required_commit_work,
                    ..
                } = heap.step_property_batch(plan, &writes, allowance).unwrap()
                {
                    heap.commit_property_batch(plan, &writes, required_commit_work)
                        .unwrap();
                    break;
                }
            }
            assert_eq!(heap.host_property(source).unwrap().as_int(), Some(7));
            for follower in followers {
                assert_eq!(heap.host_property(follower).unwrap().as_int(), Some(7));
            }
        }
    }

    pub(super) fn host(heap: &mut Heap, value: i64) -> PropertyId {
        let fields = heap.alloc_table().unwrap();
        heap.table_store(fields, 0, Value::int(0), Value::int(value))
            .unwrap();
        let host = heap.alloc_host(fields, &[], None).unwrap();
        heap.observe_host_properties(host, 1, 0).unwrap();
        PropertyId { host, field: 0 }
    }
    #[test]
    fn per_call_plans_interleave_with_unrelated_writes_and_survive_incremental_gc() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let first = host(&mut heap, 0);
        let second = host(&mut heap, 0);
        let unrelated = host(&mut heap, 0);
        let roots = heap
            .alloc_array(&[first.host, second.host, unrelated.host])
            .unwrap();
        let root = heap.add_root(RootOwner(1), RootKind::Host, roots).unwrap();
        let reference = heap.alloc_property_ref(first, true).unwrap();
        for _ in 0..100 {
            let follower = host(&mut heap, 0);
            heap.bind_host_property(follower, reference).unwrap();
            heap.array_push(roots, follower.host).unwrap();
        }
        let a = heap.begin_property_batch(true).unwrap();
        let b = heap.begin_property_batch(true).unwrap();
        heap.array_push(roots, a.value()).unwrap();
        heap.array_push(roots, b.value()).unwrap();
        let first_writes = [(first, Value::int(10))];
        let second_writes = [(second, Value::int(20))];
        let mut a_done = false;
        let mut b_done = false;
        for tick in 0..10_000 {
            heap.write_host_property(unrelated, Value::int(tick), true)
                .unwrap();
            for (plan, writes, done) in [
                (a, &first_writes, &mut a_done),
                (b, &second_writes, &mut b_done),
            ] {
                if *done {
                    continue;
                }
                if let PropertyBatchProgress::Prepared {
                    required_commit_work,
                    ..
                } = heap
                    .step_property_batch(
                        plan,
                        writes,
                        heap.property_batch_required_work(plan, writes)
                            .unwrap()
                            .max(32),
                    )
                    .unwrap()
                {
                    assert_eq!(
                        heap.commit_property_batch(plan, writes, required_commit_work - 1)
                            .unwrap(),
                        PropertyBatchProgress::Prepared {
                            work: 0,
                            required_commit_work
                        }
                    );
                    *done = matches!(
                        heap.commit_property_batch(plan, writes, required_commit_work)
                            .unwrap(),
                        PropertyBatchProgress::Committed { .. }
                    );
                }
            }
            if a_done && b_done {
                break;
            }
            if heap.phase() == GcPhase::Idle {
                heap.begin_collection().unwrap();
            }
            heap.gc_step(crate::GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
        }
        assert!(a_done && b_done, "independent plans must make progress");
        assert_eq!(heap.host_property(first).unwrap().as_int(), Some(10));
        assert_eq!(heap.host_property(second).unwrap().as_int(), Some(20));
        heap.release_root(root).unwrap();
    }
    #[test]
    fn changed_read_host_restarts_before_publication_and_cancellation_leaves_bindings_alone() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let property = host(&mut heap, 1);
        let plan = heap.begin_property_batch(true).unwrap();
        let writes = [(property, Value::int(2))];
        loop {
            if matches!(
                heap.step_property_batch(
                    plan,
                    &writes,
                    heap.property_batch_required_work(plan, &writes)
                        .unwrap()
                        .max(32)
                )
                .unwrap(),
                PropertyBatchProgress::Prepared { .. }
            ) {
                break;
            }
        }
        heap.write_host_property(property, Value::int(9), true)
            .unwrap();
        assert!(matches!(
            heap.commit_property_batch(plan, &writes, u64::MAX).unwrap(),
            PropertyBatchProgress::Preparing { .. }
        ));
        assert_eq!(heap.host_property(property).unwrap().as_int(), Some(9));
        assert!(
            heap.property_binding(property)
                .unwrap()
                .same_bits(Value::NULL)
        );
    }
}

/// Preparation yields without changing author values. A prepared proposal must
/// admit the complete publication cost before it can commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyBatchProgress {
    Preparing {
        work: u64,
        required_work: u64,
    },
    Prepared {
        work: u64,
        required_commit_work: u64,
    },
    Committed {
        work: u64,
    },
}

#[derive(Clone, Copy)]
#[repr(u32)]
enum State {
    Heap,
    Bindings,
    Phase,
    Cursor,
    Subscriber,
    Entries,
    Index,
    Root,
    Script,
    Publication,
    Inputs,
    SourceWalk,
    SourceActive,
    Graphs,
    CommitWork,
    ValidationWork,
    Adoptions,
    Count,
}
const FIELDS: u32 = 7;
const HOST: u32 = 0;
// A field occupies the low 32 bits; private flags share its integer slot.
const FIELD: u32 = 1;
const VALUE: u32 = 2;
const VERSION: u32 = 3;
const REACTIVE: u32 = 4;
const REACTIVE_VERSION: u32 = 5;
const VALIDATION: u32 = 6;
const REBOUND: i64 = 1 << 32;
const WRITABLE: i64 = 1 << 33;
const EMPTY: u32 = u32::MAX;
const ITEM_WORK: u64 = 32;

impl Heap {
    fn cursor_state(&self, plan: Value, state: State) -> Result<Value, HeapError> {
        self.sequence_get(plan, state as u32)
    }
    fn cursor_number(&self, plan: Value, state: State) -> Result<u64, HeapError> {
        Ok(self
            .cursor_state(plan, state)?
            .as_int()
            .ok_or(HeapError::TypeMismatch)? as u64)
    }
    fn cursor_set(&mut self, plan: Value, state: State, value: u64) -> Result<(), HeapError> {
        self.set_field(plan, state as u32, Value::int(value as i64))
    }
    fn cursor_entries(&self, plan: Value) -> Result<Value, HeapError> {
        self.cursor_state(plan, State::Entries)
    }
    fn cursor_len(&self, plan: Value) -> Result<u32, HeapError> {
        Ok(self.sequence_len(self.cursor_entries(plan)?)? / FIELDS)
    }
    fn cursor_property(&self, plan: Value, index: u32) -> Result<PropertyId, HeapError> {
        let entries = self.cursor_entries(plan)?;
        Ok(PropertyId {
            host: self.sequence_get(entries, index * FIELDS + HOST)?,
            field: self
                .sequence_get(entries, index * FIELDS + FIELD)?
                .as_int()
                .ok_or(HeapError::TypeMismatch)? as u32,
        })
    }
    fn cursor_value(&self, plan: Value, index: u32) -> Result<Value, HeapError> {
        self.sequence_get(self.cursor_entries(plan)?, index * FIELDS + VALUE)
    }
    fn cursor_flags(&self, entries: Value, index: u32) -> Result<i64, HeapError> {
        self.sequence_get(entries, index * FIELDS + FIELD)?
            .as_int()
            .ok_or(HeapError::TypeMismatch)
    }
    fn index_get(&self, index: Value, slot: u32) -> Result<u32, HeapError> {
        self.property_index_work
            .set(self.property_index_work.get() + 1);
        let mut bytes = [0; 4];
        let Payload::Bytes(storage) = &self.object(self.key(index)?)?.payload else {
            return Err(HeapError::WrongKind);
        };
        if storage.read(
            slot.checked_mul(4).ok_or(HeapError::OutOfBounds)?,
            &mut bytes,
        ) != 4
        {
            return Err(HeapError::OutOfBounds);
        }
        Ok(u32::from_le_bytes(bytes))
    }
    fn index_set(&mut self, index: Value, slot: u32, value: u32) -> Result<(), HeapError> {
        self.property_index_work
            .set(self.property_index_work.get() + 1);
        let key = self.key(index)?;
        let Payload::Bytes(bytes) = &mut self.objects.get_mut(key).unwrap().payload else {
            return Err(HeapError::WrongKind);
        };
        let offset = slot.checked_mul(4).ok_or(HeapError::OutOfBounds)?;
        if offset.checked_add(4).is_none_or(|end| end > bytes.len) {
            return Err(HeapError::OutOfBounds);
        }
        bytes.overwrite(offset, &value.to_le_bytes());
        Ok(())
    }
    fn property_bit(property: PropertyId, bit: u32) -> bool {
        let key = property
            .host
            .object_parts()
            .expect("validated property host")
            .1;
        if bit < 64 {
            (key >> (63 - bit)) & 1 != 0
        } else {
            (property.field >> (95 - bit)) & 1 != 0
        }
    }
    fn property_difference(first: PropertyId, second: PropertyId) -> u32 {
        let first_key = first
            .host
            .object_parts()
            .expect("validated property host")
            .1;
        let second_key = second
            .host
            .object_parts()
            .expect("validated property host")
            .1;
        if first_key != second_key {
            (first_key ^ second_key).leading_zeros()
        } else {
            64 + (first.field ^ second.field).leading_zeros()
        }
    }
    fn cursor_leaf(&self, plan: Value, property: PropertyId) -> Result<Option<u32>, HeapError> {
        let mut next = self.cursor_number(plan, State::Root)? as u32;
        if next == EMPTY {
            return Ok(None);
        }
        let index = self.cursor_state(plan, State::Index)?;
        // Critical bits strictly increase down the tree: every lookup follows
        // at most the 96 bits of (object identity, stable field identity).
        while next & 1 == 0 {
            let branch = (next / 2) * 3;
            let bit = self.index_get(index, branch)?;
            next = self.index_get(
                index,
                branch + 1 + u32::from(Self::property_bit(property, bit)),
            )?;
        }
        Ok(Some(next / 2))
    }
    pub(super) fn cursor_property_read(
        &self,
        plan: Value,
        property: PropertyId,
    ) -> Result<Value, HeapError> {
        match self.cursor_leaf(plan, property)? {
            Some(index) if self.cursor_property(plan, index)?.same(property) => {
                self.cursor_value(plan, index)
            }
            _ => self.host_property(property),
        }
    }
    pub(super) fn cursor_property_read_work(&self, plan: Value) -> Result<u64, HeapError> {
        let branches = self.info(self.cursor_state(plan, State::Index)?)?.len / 12;
        Ok(u64::from(branches.min(96)) * 2)
    }
    fn cursor_push(
        &mut self,
        plan: Value,
        property: PropertyId,
        value: Value,
        rebound: bool,
        writable: bool,
    ) -> Result<bool, HeapError> {
        let previous = self.cursor_leaf(plan, property)?;
        let entries = self.cursor_entries(plan)?;
        if let Some(previous) = previous
            && self.cursor_property(plan, previous)?.same(property)
        {
            let old = self.cursor_value(plan, previous)?;
            let flags = self.cursor_flags(entries, previous)?;
            if !old.same_bits(value) && flags & REBOUND == 0 {
                return Err(HeapError::PropertyConflict);
            }
            if rebound {
                self.set_field(
                    entries,
                    previous * FIELDS + FIELD,
                    Value::int(flags | REBOUND),
                )?;
            }
            return Ok(true);
        }
        let count = self.cursor_len(plan)?;
        let version = self
            .info(self.property_fields(property.host)?)?
            .content_version;
        let reactive = self.object(self.key(property.host)?)?.reactive_subscribers;
        let reactive_version = if reactive.same_bits(Value::NULL) {
            0
        } else {
            self.info(reactive)?.content_version
        };
        for next in [
            property.host,
            Value::int(
                i64::from(property.field)
                    | if rebound { REBOUND } else { 0 }
                    | if writable { WRITABLE } else { 0 },
            ),
            value,
            Value::int(version as i64),
            reactive,
            Value::int(reactive_version as i64),
            Value::int(0),
        ] {
            self.array_push(entries, next)?;
        }
        let leaf = count
            .checked_mul(2)
            .and_then(|v| v.checked_add(1))
            .filter(|&value| value != EMPTY)
            .ok_or(HeapError::OutOfBounds)?;
        if let Some(previous) = previous {
            let bit = Self::property_difference(property, self.cursor_property(plan, previous)?);
            let index = self.cursor_state(plan, State::Index)?;
            let mut next = self.cursor_number(plan, State::Root)? as u32;
            let mut parent = None;
            while next & 1 == 0 {
                let branch = (next / 2) * 3;
                let critical = self.index_get(index, branch)?;
                if critical >= bit {
                    break;
                }
                let slot = branch + 1 + u32::from(Self::property_bit(property, critical));
                parent = Some(slot);
                next = self.index_get(index, slot)?;
            }
            let offset = self.info(index)?.len;
            let reference = offset / 12 * 2;
            let children = if Self::property_bit(property, bit) {
                [next, leaf]
            } else {
                [leaf, next]
            };
            self.property_index_work
                .set(self.property_index_work.get() + 3);
            let mut branch = [0; 12];
            for (bytes, field) in
                branch
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip([bit, children[0], children[1]])
            {
                bytes.copy_from_slice(&field.to_le_bytes());
            }
            for chunk in branch.chunks(self.limits.max_mutation_bytes as usize) {
                self.bytes_append(index, chunk)?;
            }
            if let Some(parent) = parent {
                self.index_set(index, parent, reference)?;
            } else {
                self.cursor_set(plan, State::Root, u64::from(reference))?;
            }
        } else {
            self.cursor_set(plan, State::Root, u64::from(leaf))?;
        }
        Ok(true)
    }
    fn reset_property_batch(&mut self, plan: Value) -> Result<(), HeapError> {
        let entries = self.alloc_array(&[])?;
        let index = self.alloc_byte_buffer(&[])?;
        let inputs = self.alloc_array(&[])?;
        let graphs = self.alloc_array(&[])?;
        let adoptions = self.alloc_array(&[])?;
        self.cursor_set(plan, State::Heap, u64::from(self.id.get()))?;
        self.cursor_set(plan, State::Bindings, self.binding_revision)?;
        for state in [State::Phase, State::Cursor, State::Subscriber] {
            self.cursor_set(plan, state, 0)?;
        }
        self.set_field(plan, State::Entries as u32, entries)?;
        self.set_field(plan, State::Index as u32, index)?;
        self.set_field(plan, State::Inputs as u32, inputs)?;
        self.set_field(plan, State::SourceWalk as u32, Value::NULL)?;
        self.set_field(plan, State::SourceActive as u32, Value::bool(false))?;
        self.set_field(plan, State::Graphs as u32, graphs)?;
        self.set_field(plan, State::Adoptions as u32, adoptions)?;
        self.cursor_set(plan, State::CommitWork, ITEM_WORK)?;
        self.cursor_set(plan, State::ValidationWork, 64)?;
        self.cursor_set(plan, State::Root, u64::from(EMPTY))
    }
    /// Begin a private, per-call proposal. Retain value() in a native continuation;
    /// cancellation releases it by dropping that ordinary rooted value.
    pub fn begin_property_batch(&mut self, script: bool) -> Result<PropertyBatch, HeapError> {
        self.new_property_batch(script, false)
    }
    /// Begin a bounded authoritative service update, including read-only outputs.
    /// Concrete values may update service fields; replacement bindings retain
    /// ordinary write permissions. Root and advance this plan like a configuration.
    pub fn begin_host_publication(&mut self) -> Result<PropertyBatch, HeapError> {
        self.new_property_batch(false, true)
    }
    fn new_property_batch(
        &mut self,
        script: bool,
        publication: bool,
    ) -> Result<PropertyBatch, HeapError> {
        let plan = self.alloc_array(&[Value::NULL; State::Count as usize])?;
        self.set_field(plan, State::Script as u32, Value::bool(script))?;
        self.set_field(plan, State::Publication as u32, Value::bool(publication))?;
        self.reset_property_batch(plan)?;
        Ok(PropertyBatch(plan))
    }
    /// Prepare bounded graph items without publishing any author value or edge.
    /// Interleaved writes restart the pure plan using the original rooted inputs.
    pub fn step_property_batch(
        &mut self,
        batch: PropertyBatch,
        writes: &dyn PropertyWrites,
        work_budget: u64,
    ) -> Result<PropertyBatchProgress, HeapError> {
        self.property_work_used.set(0);
        let plan = batch.0;
        if self.cursor_number(plan, State::Phase)? == 7 {
            return Ok(PropertyBatchProgress::Committed { work: 0 });
        }
        if self.cursor_number(plan, State::Heap)? != u64::from(self.id.get())
            || self.cursor_number(plan, State::Bindings)? != self.binding_revision
        {
            if work_budget < PropertyBatch::INITIAL_WORK {
                return Ok(PropertyBatchProgress::Preparing {
                    work: 0,
                    required_work: PropertyBatch::INITIAL_WORK,
                });
            }
            self.reset_property_batch(plan)?;
            self.property_work_used.set(PropertyBatch::INITIAL_WORK);
            return Ok(PropertyBatchProgress::Preparing {
                work: PropertyBatch::INITIAL_WORK,
                required_work: ITEM_WORK,
            });
        }
        let mut work = 0;
        loop {
            if self.cursor_number(plan, State::Phase)? == 6 {
                return Ok(PropertyBatchProgress::Prepared {
                    work,
                    required_commit_work: self.property_batch_commit_work(batch, writes)?,
                });
            }
            let required = self.property_batch_step_work(plan)?;
            if work_budget.saturating_sub(work) < required {
                return Ok(PropertyBatchProgress::Preparing {
                    work,
                    required_work: required,
                });
            }
            self.property_index_work.set(0);
            work += ITEM_WORK;
            let result = (|| -> Result<Option<PropertyBatchProgress>, HeapError> {
                let cursor = self.cursor_number(plan, State::Cursor)? as u32;
                match self.cursor_number(plan, State::Phase)? {
                    0 => {
                        if cursor as usize == writes.len() {
                            self.cursor_set(plan, State::Phase, 1)?;
                            self.cursor_set(plan, State::Cursor, 0)?;
                            return Ok(None);
                        }
                        let (target, input) =
                            writes.get(cursor as usize).ok_or(HeapError::OutOfBounds)?;
                        let mut walk = self.cursor_state(plan, State::SourceWalk)?;
                        if self.cursor_state(plan, State::SourceActive)?.as_bool() != Some(true) {
                            if self.is_property_ref(input) && !self.property_access(target)? {
                                return Err(HeapError::ReadOnly);
                            }
                            if walk.same_bits(Value::NULL) {
                                walk = self.begin_property_source_walk(target, None)?;
                                self.set_field(plan, State::SourceWalk as u32, walk)?;
                            } else {
                                self.reset_property_source_walk(walk, target, None)?;
                            }
                            self.set_field(plan, State::SourceActive as u32, Value::bool(true))?;
                            return Ok(None);
                        }
                        let resolved =
                            match self.step_property_source_walk(walk, writes, SOURCE_WORK)? {
                                SourceWalkProgress::Preparing {
                                    work: used,
                                    required_work,
                                } => {
                                    debug_assert!(used <= SOURCE_WORK);
                                    debug_assert_eq!(required_work, SOURCE_WORK);
                                    work = work - ITEM_WORK + used;
                                    None
                                }
                                SourceWalkProgress::Resolved {
                                    work: used,
                                    source,
                                    writable,
                                } => {
                                    work = work - ITEM_WORK + used;
                                    Some((source, writable))
                                }
                            };
                        if let Some((source, writable)) = resolved {
                            if !self.is_property_ref(input)
                                && !writable
                                && self.cursor_state(plan, State::Publication)?.as_bool()
                                    != Some(true)
                            {
                                return Err(HeapError::ReadOnly);
                            }
                            let resolved = self.cursor_state(plan, State::Inputs)?;
                            for value in [
                                source.host,
                                Value::int(i64::from(source.field)),
                                Value::bool(writable),
                            ] {
                                self.array_push(resolved, value)?;
                            }
                            self.cursor_set(plan, State::Cursor, u64::from(cursor + 1))?;
                            self.set_field(plan, State::SourceActive as u32, Value::bool(false))?;
                        }
                    }
                    1 => {
                        if cursor as usize == writes.len() {
                            self.cursor_set(plan, State::Phase, 2)?;
                            self.cursor_set(plan, State::Cursor, 0)?;
                            return Ok(None);
                        }
                        let (_, value) =
                            writes.get(cursor as usize).ok_or(HeapError::OutOfBounds)?;
                        if !self.is_property_ref(value) {
                            let resolved = self.cursor_state(plan, State::Inputs)?;
                            let source = PropertyId {
                                host: self.sequence_get(resolved, cursor * 3)?,
                                field: self
                                    .sequence_get(resolved, cursor * 3 + 1)?
                                    .as_int()
                                    .ok_or(HeapError::TypeMismatch)?
                                    as u32,
                            };
                            if !self.cursor_push(
                                plan,
                                source,
                                value,
                                false,
                                self.property_access(source)?,
                            )? {
                                return Ok(None);
                            }
                        }
                        self.cursor_set(plan, State::Cursor, u64::from(cursor + 1))?;
                    }
                    2 => {
                        if cursor as usize == writes.len() {
                            self.cursor_set(plan, State::Phase, 3)?;
                            self.cursor_set(plan, State::Cursor, 0)?;
                            return Ok(None);
                        }
                        let (target, input) =
                            writes.get(cursor as usize).ok_or(HeapError::OutOfBounds)?;
                        let resolved = self.cursor_state(plan, State::Inputs)?;
                        let source = PropertyId {
                            host: self.sequence_get(resolved, cursor * 3)?,
                            field: self
                                .sequence_get(resolved, cursor * 3 + 1)?
                                .as_int()
                                .ok_or(HeapError::TypeMismatch)?
                                as u32,
                        };
                        let value = self.cursor_property_read(plan, source)?;
                        let writable =
                            self.sequence_get(resolved, cursor * 3 + 2)?.as_bool() == Some(true);
                        if !self.cursor_push(
                            plan,
                            source,
                            value,
                            false,
                            self.property_access(source)?,
                        )? || !self.cursor_push(
                            plan,
                            target,
                            value,
                            self.is_property_ref(input),
                            writable,
                        )? {
                            return Ok(None);
                        }
                        if self.is_property_ref(input) {
                            let graphs = self.cursor_state(plan, State::Graphs)?;
                            if self.object(self.key(source.host)?)?.derived {
                                let graph = self.begin_property_cycle(target, source)?;
                                self.array_push(graphs, graph)?;
                            }
                            let previous = self.property_binding(target)?;
                            let mut binding_work = 128u64;
                            if !previous.same_bits(Value::NULL) {
                                let previous_source = self.property_ref(previous)?.0;
                                let subscribers = self.subscribers(previous_source)?;
                                if !subscribers.same_bits(Value::NULL) {
                                    binding_work +=
                                        u64::from(self.sequence_len(subscribers)? / 2) * 8;
                                }
                                if self.object(self.key(previous_source.host)?)?.derived {
                                    let graph =
                                        self.begin_property_disconnect(previous_source.host)?;
                                    self.array_push(graphs, graph)?;
                                }
                            }
                            let direct = self.property_ref(input)?.0;
                            binding_work +=
                                u64::from(self.object(self.key(target.host)?)?.observable_fields)
                                    * 4;
                            binding_work +=
                                u64::from(self.object(self.key(direct.host)?)?.observable_fields)
                                    * 4;
                            self.cursor_set(
                                plan,
                                State::CommitWork,
                                self.cursor_number(plan, State::CommitWork)? + binding_work,
                            )?;
                        }
                        self.cursor_set(plan, State::Cursor, u64::from(cursor + 1))?;
                    }
                    3 => {
                        if cursor == self.cursor_len(plan)? {
                            self.cursor_set(plan, State::Phase, 4)?;
                            self.cursor_set(plan, State::Cursor, 0)?;
                            return Ok(None);
                        }
                        let source = self.cursor_property(plan, cursor)?;
                        let list = self.subscribers(source)?;
                        let subscriber = self.cursor_number(plan, State::Subscriber)? as u32;
                        if list.same_bits(Value::NULL) || subscriber == self.sequence_len(list)? {
                            self.cursor_set(plan, State::Cursor, u64::from(cursor + 1))?;
                            self.cursor_set(plan, State::Subscriber, 0)?;
                            return Ok(None);
                        }
                        let weak = self.sequence_get(list, subscriber)?;
                        let field =
                            self.sequence_get(list, subscriber + 1)?
                                .as_int()
                                .ok_or(HeapError::TypeMismatch)? as u32;
                        if let Some(host) = self.upgrade_weakref(weak)?
                            && self.property_fields(host).is_ok()
                        {
                            let property = PropertyId { host, field };
                            let permission = self.property_ref(self.property_binding(property)?)?.1;
                            let writable = self.cursor_flags(self.cursor_entries(plan)?, cursor)?
                                & WRITABLE
                                != 0
                                && permission
                                && self.property_access(property)?;
                            if !self.cursor_push(
                                plan,
                                property,
                                self.cursor_value(plan, cursor)?,
                                false,
                                writable,
                            )? {
                                return Ok(None);
                            }
                        }
                        self.cursor_set(plan, State::Subscriber, u64::from(subscriber + 2))?;
                    }
                    4 => {
                        if cursor == self.cursor_len(plan)? {
                            self.cursor_set(plan, State::Phase, 5)?;
                            self.cursor_set(plan, State::Cursor, 0)?;
                            return Ok(None);
                        }
                        let property = self.cursor_property(plan, cursor)?;
                        let value = self.cursor_value(plan, cursor)?;
                        let view = PropertyRead::cursor(self, writes, plan);
                        let grant = self.cursor_number(plan, State::ValidationWork)?;
                        let (result, validation_work) = self
                            .property_validation_budget(grant, || {
                                self.validate_property_proposal(&view, property, value)
                            });
                        work += validation_work;
                        match result {
                            Err(HeapError::OperationLimit { requested, .. }) => {
                                self.cursor_set(
                                    plan,
                                    State::ValidationWork,
                                    grant.saturating_mul(2).max(requested),
                                )?;
                                return Ok(None);
                            }
                            other => other?,
                        }
                        self.cursor_set(plan, State::ValidationWork, 64)?;
                        if validation_work != 0 {
                            self.set_field(
                                self.cursor_entries(plan)?,
                                cursor * FIELDS + VALIDATION,
                                Value::int(grant as i64),
                            )?;
                            self.cursor_set(
                                plan,
                                State::CommitWork,
                                self.cursor_number(plan, State::CommitWork)? + 16 + grant,
                            )?;
                        }
                        if self.object(self.key(property.host)?)?.model
                            && self.model_candidate(value)?
                        {
                            let adoption = self.begin_model_adoption(value)?;
                            self.array_push(self.cursor_state(plan, State::Adoptions)?, adoption)?;
                        }
                        let metadata = self.property_metadata(property)?;
                        if !metadata.same_bits(Value::NULL)
                            && self
                                .sequence_get(metadata, Slot::RequiresWrite as u32)?
                                .as_bool()
                                == Some(true)
                            && self.cursor_flags(self.cursor_entries(plan)?, cursor)? & WRITABLE
                                == 0
                        {
                            return Err(HeapError::ReadOnly);
                        }
                        let reactive = self.object(self.key(property.host)?)?.reactive_subscribers;
                        let notifications = if reactive.same_bits(Value::NULL) {
                            0
                        } else {
                            u64::from(self.sequence_len(reactive)? / 2)
                        };
                        self.cursor_set(
                            plan,
                            State::CommitWork,
                            self.cursor_number(plan, State::CommitWork)?
                                + 48
                                + notifications * 8
                                + self.property_storage_work(
                                    property,
                                    self.cursor_state(plan, State::Script)?.as_bool() == Some(true),
                                )?,
                        )?;
                        self.cursor_set(plan, State::Cursor, u64::from(cursor + 1))?;
                    }
                    5 => {
                        let graphs = self.cursor_state(plan, State::Graphs)?;
                        if cursor == self.sequence_len(graphs)? {
                            self.cursor_set(plan, State::Phase, 8)?;
                            self.cursor_set(plan, State::Cursor, 0)?;
                            self.cursor_set(
                                plan,
                                State::CommitWork,
                                self.cursor_number(plan, State::CommitWork)?
                                    + (writes.len() as u64) * 16,
                            )?;
                            return Ok(None);
                        }
                        let graph = self.sequence_get(graphs, cursor)?;
                        let before = self.property_graph_work(graph)?;
                        let progress = self.step_property_graph(
                            graph,
                            writes,
                            self.property_graph_required_work(graph)?,
                        );
                        work += self.property_graph_work(graph)?.saturating_sub(before);
                        match progress? {
                            PropertyGraphProgress::Preparing {
                                work: used,
                                required_work,
                            } => {
                                debug_assert!(used <= required);
                                debug_assert!(required_work != 0);
                            }
                            PropertyGraphProgress::Complete {
                                work: used,
                                disconnect_work,
                            } => {
                                debug_assert!(used <= required);
                                let proofs = self.property_graph_versions_len(graph)?;
                                self.cursor_set(
                                    plan,
                                    State::CommitWork,
                                    self.cursor_number(plan, State::CommitWork)?
                                        + disconnect_work
                                        + u64::from(proofs) * ITEM_WORK,
                                )?;
                                self.cursor_set(plan, State::Cursor, u64::from(cursor + 1))?;
                            }
                        }
                    }
                    8 => {
                        let adoptions = self.cursor_state(plan, State::Adoptions)?;
                        if cursor == self.sequence_len(adoptions)? {
                            self.cursor_set(plan, State::Phase, 6)?;
                            return Ok(None);
                        }
                        let adoption = self.sequence_get(adoptions, cursor)?;
                        let before = self.model_adoption_work(adoption)?;
                        let progress = self.step_model_adoption(
                            adoption,
                            self.model_adoption_required_work(adoption)?,
                        );
                        work += self.model_adoption_work(adoption)?.saturating_sub(before);
                        match progress? {
                            ModelAdoptionProgress::Preparing {
                                work: used,
                                required_work,
                            } => {
                                debug_assert!(used <= required);
                                debug_assert!(required_work != 0);
                            }
                            ModelAdoptionProgress::Complete {
                                work: used,
                                commit_work,
                            } => {
                                debug_assert!(used <= required);
                                let proofs = self.model_adoption_versions_len(adoption)?;
                                self.cursor_set(
                                    plan,
                                    State::CommitWork,
                                    self.cursor_number(plan, State::CommitWork)?
                                        + commit_work
                                        + u64::from(proofs) * 16,
                                )?;
                                self.cursor_set(plan, State::Cursor, u64::from(cursor + 1))?;
                            }
                        }
                    }
                    6 => {
                        return Ok(Some(PropertyBatchProgress::Prepared {
                            work: 0,
                            required_commit_work: self.property_batch_commit_work(batch, writes)?,
                        }));
                    }
                    7 => return Ok(Some(PropertyBatchProgress::Committed { work: 0 })),
                    _ => return Err(HeapError::TypeMismatch),
                }
                Ok(None)
            })();
            work += self.property_index_work.get();
            self.property_work_used.set(work);
            match result? {
                Some(PropertyBatchProgress::Prepared {
                    required_commit_work,
                    ..
                }) => {
                    return Ok(PropertyBatchProgress::Prepared {
                        work,
                        required_commit_work,
                    });
                }
                Some(PropertyBatchProgress::Committed { .. }) => {
                    return Ok(PropertyBatchProgress::Committed { work });
                }
                _ => {}
            }
        }
    }

    fn property_batch_commit_work(
        &self,
        batch: PropertyBatch,
        writes: &dyn PropertyWrites,
    ) -> Result<u64, HeapError> {
        let _ = writes;
        self.cursor_number(batch.0, State::CommitWork)
    }
    // These bounds admit every controlled index read/write before the item.
    // Each critical-bit path visits at most 96 branches, with two u32 reads per
    // branch. Settlement charges actual index accesses, plus fixed bookkeeping.
    fn property_batch_step_work(&self, plan: Value) -> Result<u64, HeapError> {
        if self.cursor_number(plan, State::Phase)? == 8 {
            let adoptions = self.cursor_state(plan, State::Adoptions)?;
            let cursor = self.cursor_number(plan, State::Cursor)? as u32;
            return Ok(ITEM_WORK
                + if cursor < self.sequence_len(adoptions)? {
                    self.model_adoption_required_work(self.sequence_get(adoptions, cursor)?)?
                } else {
                    0
                });
        }
        if self.cursor_number(plan, State::Phase)? == 5 {
            let graphs = self.cursor_state(plan, State::Graphs)?;
            let cursor = self.cursor_number(plan, State::Cursor)? as u32;
            return Ok(ITEM_WORK
                + if cursor < self.sequence_len(graphs)? {
                    self.property_graph_required_work(self.sequence_get(graphs, cursor)?)?
                } else {
                    0
                });
        }
        let index = match self.cursor_number(plan, State::Phase)? {
            1 | 3 => 2 * 96 * 2 + 8,
            2 => 5 * 96 * 2 + 8,
            _ => 0,
        };
        Ok(ITEM_WORK
            + index
            + if self.cursor_number(plan, State::Phase)? == 4 {
                self.cursor_number(plan, State::ValidationWork)?
            } else {
                0
            })
    }
    /// The current call's next indivisible step, independent of other tasks.
    pub fn property_batch_required_work(
        &self,
        batch: PropertyBatch,
        writes: &dyn PropertyWrites,
    ) -> Result<u64, HeapError> {
        let plan = batch.0;
        if self.cursor_number(plan, State::Heap)? != u64::from(self.id.get())
            || self.cursor_number(plan, State::Bindings)? != self.binding_revision
        {
            return Ok(PropertyBatch::INITIAL_WORK);
        }
        match self.cursor_number(plan, State::Phase)? {
            6 => self.property_batch_commit_work(batch, writes),
            7 => Ok(0),
            _ => self.property_batch_step_work(plan),
        }
    }
    /// Admit one complete publication. Insufficient work preserves the plan and
    /// every previous value and binding; fallible edge preparation comes first.
    pub fn commit_property_batch(
        &mut self,
        batch: PropertyBatch,
        writes: &dyn PropertyWrites,
        work_budget: u64,
    ) -> Result<PropertyBatchProgress, HeapError> {
        self.property_work_used.set(0);
        let plan = batch.0;
        if self.cursor_number(plan, State::Heap)? != u64::from(self.id.get())
            || self.cursor_number(plan, State::Bindings)? != self.binding_revision
        {
            if work_budget < PropertyBatch::INITIAL_WORK {
                return Ok(PropertyBatchProgress::Preparing {
                    work: 0,
                    required_work: PropertyBatch::INITIAL_WORK,
                });
            }
            self.reset_property_batch(plan)?;
            self.property_work_used.set(PropertyBatch::INITIAL_WORK);
            return Ok(PropertyBatchProgress::Preparing {
                work: PropertyBatch::INITIAL_WORK,
                required_work: ITEM_WORK,
            });
        }
        if self.cursor_number(plan, State::Phase)? != 6 {
            return Err(HeapError::WrongKind);
        }
        let required = self.property_batch_commit_work(batch, writes)?;
        if work_budget < required {
            return Ok(PropertyBatchProgress::Prepared {
                work: 0,
                required_commit_work: required,
            });
        }
        self.property_work_used.set(16);
        let traversal = self.property_traversal_work.get();
        let graphs = self.cursor_state(plan, State::Graphs)?;
        for graph in 0..self.sequence_len(graphs)? {
            let graph = self.sequence_get(graphs, graph)?;
            for proof in 0..self.property_graph_versions_len(graph)? {
                self.property_work_used
                    .set(self.property_work_used.get() + 16);
                if !self.property_graph_version_valid(graph, proof)? {
                    self.reset_property_batch(plan)?;
                    self.property_work_used
                        .set(self.property_work_used.get() + PropertyBatch::INITIAL_WORK);
                    return Ok(PropertyBatchProgress::Preparing {
                        work: self.property_work_used.get(),
                        required_work: ITEM_WORK,
                    });
                }
            }
        }
        for index in 0..self.cursor_len(plan)? {
            self.property_work_used
                .set(self.property_work_used.get() + 16);
            let property = self.cursor_property(plan, index)?;
            let expected = self
                .sequence_get(self.cursor_entries(plan)?, index * FIELDS + VERSION)?
                .as_int()
                .ok_or(HeapError::TypeMismatch)? as u64;
            let entries = self.cursor_entries(plan)?;
            let reactive = self.sequence_get(entries, index * FIELDS + REACTIVE)?;
            let reactive_version = self
                .sequence_get(entries, index * FIELDS + REACTIVE_VERSION)?
                .as_int()
                .ok_or(HeapError::TypeMismatch)? as u64;
            let actual_reactive = self.object(self.key(property.host)?)?.reactive_subscribers;
            if !reactive.same_bits(actual_reactive)
                || (!reactive.same_bits(Value::NULL)
                    && self.info(reactive)?.content_version != reactive_version)
                || self
                    .info(self.property_fields(property.host)?)?
                    .content_version
                    != expected
            {
                self.reset_property_batch(plan)?;
                self.property_work_used
                    .set(self.property_work_used.get() + PropertyBatch::INITIAL_WORK);
                return Ok(PropertyBatchProgress::Preparing {
                    work: self.property_work_used.get(),
                    required_work: ITEM_WORK,
                });
            }
        }
        let adoptions = self.cursor_state(plan, State::Adoptions)?;
        for index in 0..self.sequence_len(adoptions)? {
            let adoption = self.sequence_get(adoptions, index)?;
            for proof in 0..self.model_adoption_versions_len(adoption)? {
                self.property_work_used
                    .set(self.property_work_used.get() + 16);
                if !self.model_adoption_version_valid(adoption, proof)? {
                    self.reset_property_batch(plan)?;
                    self.property_work_used
                        .set(self.property_work_used.get() + PropertyBatch::INITIAL_WORK);
                    return Ok(PropertyBatchProgress::Preparing {
                        work: self.property_work_used.get(),
                        required_work: ITEM_WORK,
                    });
                }
            }
        }
        for index in 0..self.cursor_len(plan)? {
            let entries = self.cursor_entries(plan)?;
            let grant = self
                .sequence_get(entries, index * FIELDS + VALIDATION)?
                .as_int()
                .ok_or(HeapError::TypeMismatch)? as u64;
            if grant == 0 {
                continue;
            }
            self.property_work_used
                .set(self.property_work_used.get() + 16);
            self.property_index_work.set(0);
            let view = PropertyRead::cursor(self, writes, plan);
            let (result, validation_work) = self.property_validation_budget(grant, || {
                self.validate_property_proposal(
                    &view,
                    self.cursor_property(plan, index)?,
                    self.cursor_value(plan, index)?,
                )
            });
            self.property_work_used.set(
                self.property_work_used.get() + validation_work + self.property_index_work.get(),
            );
            if let Err(HeapError::OperationLimit { requested, .. }) = result {
                let next = grant.saturating_mul(2).max(requested);
                self.set_field(
                    entries,
                    index * FIELDS + VALIDATION,
                    Value::int(next as i64),
                )?;
                self.cursor_set(plan, State::CommitWork, required + next - grant)?;
                return Ok(PropertyBatchProgress::Prepared {
                    work: self.property_work_used.get(),
                    required_commit_work: required + next - grant,
                });
            }
            result?;
        }
        let script = self.cursor_state(plan, State::Script)?.as_bool() == Some(true);
        self.property_bindings.clear();
        let prepared = (|| {
            for index in 0..self.cursor_len(plan)? {
                self.property_work_used
                    .set(self.property_work_used.get() + 16);
                let property = self.cursor_property(plan, index)?;
                let value = self.cursor_value(plan, index)?;
                self.prepare_property_storage(property, value, script)?;
            }
            let mut revisions = 0;
            for index in 0..writes.len() {
                self.property_work_used
                    .set(self.property_work_used.get() + 4);
                let (target, value) = writes.get(index).ok_or(HeapError::OutOfBounds)?;
                if self.is_property_ref(value) {
                    let previous = self.property_binding(target)?;
                    revisions += 1 + u64::from(!previous.same_bits(Value::NULL));
                }
            }
            self.binding_revision
                .checked_add(revisions)
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
        })();
        if let Err(error) = prepared {
            self.discard_binding_plan()?;
            self.property_work_used.set(
                self.property_work_used.get() + self.property_traversal_work.get() - traversal,
            );
            return Err(error);
        }
        for index in 0..self.sequence_len(adoptions)? {
            self.publish_model_adoption(self.sequence_get(adoptions, index)?)?;
        }
        for index in 0..self.property_bindings.len() {
            self.property_work_used
                .set(self.property_work_used.get() + 32);
            let prepared = self.property_bindings[index];
            let value = writes.get(prepared.input).ok_or(HeapError::OutOfBounds)?.1;
            self.detach_property(prepared.target)?;
            self.binding_changed()?;
            self.set_field(
                self.property_metadata(prepared.target)?,
                Slot::Binding as u32,
                value,
            )?;
            self.reconnect_derived(prepared.source.host)?;
        }
        for index in 0..self.cursor_len(plan)? {
            self.property_work_used
                .set(self.property_work_used.get() + 16);
            self.commit_property_value(
                self.cursor_property(plan, index)?,
                self.cursor_value(plan, index)?,
                script,
            )?;
        }
        self.property_bindings.clear();
        self.cursor_set(plan, State::Phase, 7)?;
        self.property_work_used.set(
            self.property_work_used.get() + 16 + self.property_traversal_work.get() - traversal,
        );
        debug_assert!(self.property_work_used.get() <= required);
        Ok(PropertyBatchProgress::Committed {
            work: self.property_work_used.get(),
        })
    }
}
