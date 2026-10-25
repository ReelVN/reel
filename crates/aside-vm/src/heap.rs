mod model;
mod properties;
mod reactive;
pub(crate) mod snapshot;
mod storage;
pub(crate) use reactive::DerivedSlot;
mod table;
use crate::HeapBudgetUsage;
use crate::gc::Collector;
use crate::payload::{BYTE_CHUNK_BYTES, Bytes, Payload, Values};
use crate::root_ranges::{BOOK_BYTES, RangeDescriptor};
use crate::slots::{HeapKey, RangeKey, RootKey, Slots, heap_key, index};
use crate::{GcPhase, HeapHandle, HeapId, SessionEpoch, Value, WeakHandle};
pub use properties::{
    PropertyBatch, PropertyBatchProgress, PropertyId, PropertyRead, PropertyWrites,
};
use slotmap::Key;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::rc::Rc;
pub use table::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ObjectKind {
    Cell,
    Array,
    Tuple,
    Table,
    String,
    Bytes,
    TextBuffer,
    ByteBuffer,
    Class,
    Instance,
    Closure,
    BoundBuiltin,
    BoundNative,
    BoundScript,
    Super,
    PropertyRef,
    Generator,
    Thread,
    Future,
    WeakRef,
    Host,
    Set,
    Range,
    Iterator,
    FaultEnvelope,
    FaultStack,
    FaultCause,
    CoreError,
    /// Private reactive dependency sequence; not a script-mutable array.
    Dependencies,
}
impl ObjectKind {
    fn values(self) -> bool {
        !matches!(
            self,
            Self::String
                | Self::Bytes
                | Self::TextBuffer
                | Self::ByteBuffer
                | Self::WeakRef
                | Self::FaultEnvelope
                | Self::FaultStack
                | Self::FaultCause
                | Self::CoreError
        )
    }
    fn record(self) -> bool {
        matches!(
            self,
            Self::Table
                | Self::Class
                | Self::Instance
                | Self::Closure
                | Self::BoundBuiltin
                | Self::BoundNative
                | Self::BoundScript
                | Self::Super
                | Self::PropertyRef
                | Self::Generator
                | Self::Thread
                | Self::Future
                | Self::Host
                | Self::Set
                | Self::Range
                | Self::Iterator
        )
    }
    fn weak_target(self) -> bool {
        !matches!(self, Self::Tuple | Self::String | Self::Bytes)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RootKind {
    TaskFrame,
    Session,
    ModuleSet,
    Delivery,
    NativeContinuation,
    Host,
    Work,
    Arguments,
    Exception,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RootOwner(pub u64);

/// Explicitly owned registration. Release through the heap; copying is not a new root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RootToken {
    heap: HeapId,
    epoch: SessionEpoch,
    key: RootKey,
    _thread: PhantomData<Rc<()>>,
}
impl RootToken {
    pub fn heap_id(self) -> HeapId {
        self.heap
    }
    pub fn epoch(self) -> SessionEpoch {
        self.epoch
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HeapLimits {
    /// Optional host count quotas. Defaults are the slotmap representation ceiling.
    pub max_objects: u32,
    pub max_roots: u32,
    pub max_root_ranges: u32,
    /// Optional host byte quota; u64::MAX leaves allocation to available memory.
    /// Usage and peaks are tracked independently of this admission limit.
    pub max_bytes: u64,
    pub max_mutation_values: u32,
    pub max_mutation_bytes: u32,
}
impl Default for HeapLimits {
    fn default() -> Self {
        Self {
            max_objects: u32::MAX - 1,
            max_roots: u32::MAX - 1,
            max_root_ranges: u32::MAX - 1,
            max_bytes: u64::MAX,
            max_mutation_values: 256,
            max_mutation_bytes: 4096,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeapError {
    InvalidLimits,
    IdentityExhausted,
    CycleExhausted,
    CollectionInProgress,
    ConcurrentModification,
    WrongHeap,
    WrongEpoch,
    InvalidHandle,
    RetiredHost,
    BindingCycle,
    PropertyConflict,
    UnconfirmedLive,
    InvalidRoot,
    WrongKind,
    OutOfBounds,
    ObjectCapacity,
    RootCapacity,
    InvalidRootRange,
    RootRangeCapacity,
    WindowCapacity,
    RootProviderAttached,
    MissingRootProvider,
    WrongRootProvider,
    RootRangeLength {
        expected: u32,
        actual: u32,
    },
    UninitializedSlot,
    UninitializedCell,
    UninitializedField,
    ConstructionFailed,
    AlreadyInitialized,
    ReadOnly,
    TypeMismatch,
    InvalidRangeCursor,
    AllocationFailure,
    HeapUnavailable,
    OperationLimit {
        requested: u64,
        limit: u64,
    },
    ResourceLimit {
        requested: u64,
        used: u64,
        limit: u64,
    },
}
impl std::fmt::Display for HeapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for HeapError {}

#[derive(Clone, Copy, Debug)]
pub struct ObjectInfo {
    pub kind: ObjectKind,
    /// Element count for values, byte count for text/buffers.
    pub len: u32,
    pub content_version: u64,
    pub structure_version: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct HeapStats {
    pub collections: u64,
    pub gc_units: u64,
    pub live_objects: u32,
    pub pending_objects: u32,
    pub roots: u32,
    pub retired_objects: u32,
    pub retired_roots: u32,
    pub root_ranges: u32,
    pub retired_root_ranges: u32,
    pub external_bytes: u64,
    pub reserved_bytes: u64,
    pub payload_bytes: u64,
    pub pending_bytes: u64,
    pub peak_pending_bytes: u64,
    pub charged_bytes: u64,
    pub peak_charged_bytes: u64,
    pub chunk_allocations: u64,
    pub gray_len: u32,
    pub allocation_queue_len: u32,
}

/// Trusted, allocation-free validation at a host property's write boundary.
/// The index identifies an existing own field. Read related properties through
/// the supplied view to validate the complete proposed configuration. This
/// callback must not execute script or wait; errors precede value publication.
pub type HostPropertyValidator = fn(&PropertyRead<'_>, Value, u32, Value) -> Result<(), HeapError>;

pub(crate) struct Object {
    pub kind: ObjectKind,
    pub host_validator: Option<HostPropertyValidator>,
    pub host_model_value: bool,
    pub host_await: Option<crate::bytecode::NativeId>,
    pub host_call: Option<crate::bytecode::FunctionId>,
    pub observable_fields: u32,
    pub observable_writable: u32,
    pub property_owner: Option<HeapKey>,
    pub model: bool,
    pub readonly_collection: bool,
    pub model_visit: u64,
    pub model_metadata: Value,
    pub reactive_subscribers: Value,
    pub reactive_scanned: bool,
    pub derived: bool,
    pub derived_dirty: bool,
    pub evaluation: u64,
    pub property_generation: u64,
    pub payload: Payload,
    pub weak: Option<HeapHandle>,
    pub mark: u64,
    pub scan: u32,
    pub metadata_scanned: bool,
    pub queued: bool,
    pub dying: bool,
    pub content_version: u64,
    pub structure_version: u64,
    pub charged: u64,
}
pub(crate) struct Root {
    pub value: Value,
    pub owner: RootOwner,
    pub kind: RootKind,
}

/// Single-threaded integer-handle graph. All publishing writes pass the barrier.
pub struct Heap {
    pub(crate) code: Option<Rc<crate::fault::CodeLease>>,
    pub(crate) id: HeapId,
    pub(crate) epoch: SessionEpoch,
    pub(crate) objects: crate::slots::ObjectSlots<Object>,
    roots: Slots<RootKey, Root>,
    pub(crate) root_index: Vec<Option<RootKey>>,
    pub(crate) root_scan_len: usize,
    pub(crate) ranges: Slots<RangeKey, RangeDescriptor>,
    pub(crate) range_index: Vec<Option<RangeKey>>,
    pub(crate) range_scan_len: usize,
    pub(crate) next_attachment: u64,
    pub(crate) usage: HeapBudgetUsage,
    pub(crate) allocations: VecDeque<HeapKey>,
    pub(crate) gray: VecDeque<HeapKey>,
    pub(crate) release: VecDeque<HeapKey>,
    pub(crate) gc: Collector,
    property_work: Vec<PropertyId>,
    property_batch: Vec<properties::PreparedProperty>,
    property_batch_index: Vec<usize>,
    property_index_work: std::cell::Cell<u64>,
    property_traversal_work: std::cell::Cell<u64>,
    property_work_used: std::cell::Cell<u64>,
    property_validation_work: std::cell::Cell<u64>,
    property_validation_limit: std::cell::Cell<u64>,
    property_bindings: Vec<properties::PreparedBinding>,
    derived_disconnect_work: Vec<Value>,
    model_work: Vec<Value>,
    model_walk: u64,
    reactive_queue: VecDeque<HeapKey>,
    reactive_capture: Option<Value>,
    evaluation: u64,
    binding_revision: u64,
    pub(crate) mutation_revision: u64,
    property_proposal: Option<Value>,
    pub(crate) limits: HeapLimits,
    reserved: u64,
    payload_bytes: u64,
    pub(crate) pending_bytes: u64,
    pub(crate) peak_pending_bytes: u64,
    chunk_allocations: u64,
    _thread: PhantomData<Rc<()>>,
}

impl Heap {
    pub fn new(epoch: SessionEpoch, limits: HeapLimits) -> Result<Self, HeapError> {
        Self::with_retirement(epoch, limits, u32::MAX)
    }
    pub(crate) fn with_retirement(
        epoch: SessionEpoch,
        limits: HeapLimits,
        retirement: u32,
    ) -> Result<Self, HeapError> {
        if limits.max_objects == 0
            || limits.max_objects == u32::MAX
            || limits.max_roots == 0
            || limits.max_roots == u32::MAX
            || limits.max_root_ranges == u32::MAX
            || limits.max_mutation_values == 0
            || limits.max_mutation_bytes == 0
        {
            return Err(HeapError::InvalidLimits);
        }
        let initial_objects = limits.max_objects.min(256) as usize;
        let initial_roots = limits.max_roots.min(64) as usize;
        let initial_ranges = limits.max_root_ranges.min(64) as usize;
        let root_stride = std::mem::size_of::<crate::slots::Slot<Root>>()
            + 2 * std::mem::align_of::<crate::slots::Slot<Root>>().max(4);
        let range_stride = std::mem::size_of::<crate::slots::Slot<RangeDescriptor>>()
            + 2 * std::mem::align_of::<crate::slots::Slot<RangeDescriptor>>().max(4);
        let estimate = crate::slots::ObjectSlots::<Object>::bytes_for(initial_objects)
            + (initial_roots as u64 + 1) * root_stride as u64
            + initial_objects as u64 * 4 * std::mem::size_of::<HeapKey>() as u64
            + (initial_roots as u64 + 1) * std::mem::size_of::<Option<RootKey>>() as u64
            + (initial_ranges as u64 + 1)
                * (range_stride as u64 + std::mem::size_of::<Option<RangeKey>>() as u64)
            + BOOK_BYTES
            + std::mem::size_of::<Self>() as u64;
        if estimate > limits.max_bytes {
            return Err(HeapError::ResourceLimit {
                requested: estimate,
                used: 0,
                limit: limits.max_bytes,
            });
        }
        // Avoid allocator/layout overflow on 32-bit before any large allocation.
        if estimate > isize::MAX as u64 {
            return Err(HeapError::InvalidLimits);
        }
        let usage = HeapBudgetUsage::new(limits.max_bytes);
        usage.charge(estimate - BOOK_BYTES);
        let mut heap = Self {
            code: None,
            id: HeapId::fresh().ok_or(HeapError::IdentityExhausted)?,
            epoch,
            objects: crate::slots::ObjectSlots::new(
                initial_objects,
                limits.max_objects as usize,
                retirement,
            ),
            roots: Slots::new(initial_roots, limits.max_roots as usize, retirement),
            root_index: vec![None; initial_roots + 1],
            root_scan_len: 0,
            ranges: Slots::new(initial_ranges, limits.max_root_ranges as usize, retirement),
            range_index: vec![None; initial_ranges + 1],
            range_scan_len: 0,
            next_attachment: 0,
            usage,
            allocations: VecDeque::with_capacity(initial_objects),
            gray: VecDeque::with_capacity(initial_objects),
            release: VecDeque::with_capacity(initial_objects),
            gc: Collector::default(),
            property_work: Vec::new(),
            property_batch: Vec::new(),
            property_batch_index: Vec::new(),
            property_index_work: std::cell::Cell::new(0),
            property_traversal_work: std::cell::Cell::new(0),
            property_work_used: std::cell::Cell::new(0),
            property_validation_work: std::cell::Cell::new(0),
            property_validation_limit: std::cell::Cell::new(u64::MAX),
            property_bindings: Vec::new(),
            derived_disconnect_work: Vec::new(),
            model_work: Vec::new(),
            model_walk: 0,
            reactive_queue: VecDeque::with_capacity(initial_objects),
            reactive_capture: None,
            evaluation: 0,
            binding_revision: 1,
            mutation_revision: 0,
            property_proposal: None,
            limits,
            reserved: estimate,
            payload_bytes: 0,
            pending_bytes: 0,
            peak_pending_bytes: 0,
            chunk_allocations: 0,
            _thread: PhantomData,
        };
        let actual = heap.objects.reserved_bytes()
            + heap.roots.reserved_bytes()
            + (heap.root_index.capacity() * std::mem::size_of::<Option<RootKey>>()) as u64
            + heap.ranges.reserved_bytes()
            + (heap.range_index.capacity() * std::mem::size_of::<Option<RangeKey>>()) as u64
            + BOOK_BYTES
            + ((heap.allocations.capacity()
                + heap.gray.capacity()
                + heap.release.capacity()
                + heap.reactive_queue.capacity())
                * std::mem::size_of::<HeapKey>()) as u64
            + std::mem::size_of::<Self>() as u64;
        if actual > estimate {
            heap.usage.ensure(actual - estimate)?;
            heap.usage.charge(actual - estimate);
        } else {
            heap.usage.refund(estimate - actual);
        }
        heap.reserved = actual;
        Ok(heap)
    }
    pub fn id(&self) -> HeapId {
        self.id
    }
    /// Conservative invalidation watermark for cached host projections of stored
    /// fields, collections, private host payloads and object liveness. Unrelated
    /// writes and property-observer changes may also advance it; this is
    /// not an exact edit count. Pair it with `id()` and `epoch()` and discard
    /// cached stamps on reconstruction: the watermark has no persistent or
    /// snapshot identity semantics.
    pub fn mutation_revision(&self) -> u64 {
        self.mutation_revision
    }
    pub fn epoch(&self) -> SessionEpoch {
        self.epoch
    }
    pub fn phase(&self) -> GcPhase {
        self.gc.phase
    }
    pub fn stats(&self) -> HeapStats {
        let pending = self.release.len() as u32;
        HeapStats {
            collections: self.gc.cycle,
            gc_units: self.gc.units,
            live_objects: self.objects.occupied() as u32 - pending,
            pending_objects: pending,
            roots: self.roots.occupied() as u32,
            retired_objects: self.objects.retired() as u32,
            retired_roots: self.roots.retired() as u32,
            root_ranges: self.ranges.occupied() as u32,
            retired_root_ranges: self.ranges.retired() as u32,
            external_bytes: self.usage.charged_bytes() - self.reserved - self.payload_bytes,
            reserved_bytes: self.reserved,
            payload_bytes: self.payload_bytes,
            pending_bytes: self.pending_bytes,
            peak_pending_bytes: self.peak_pending_bytes,
            charged_bytes: self.usage.charged_bytes(),
            peak_charged_bytes: self.usage.peak_charged_bytes(),
            chunk_allocations: self.chunk_allocations,
            gray_len: self.gray.len() as u32,
            allocation_queue_len: self.allocations.len() as u32,
        }
    }
    fn ensure_bytes(&self, bytes: u64) -> Result<(), HeapError> {
        self.usage.ensure(bytes)
    }
    fn charge(&mut self, bytes: u64, chunks: u64) {
        self.gc.allocated_bytes = self.gc.allocated_bytes.saturating_add(bytes);
        self.payload_bytes += bytes;
        self.chunk_allocations += chunks;
        self.usage.charge(bytes);
    }
    pub(crate) fn freed(&mut self, bytes: u64) {
        self.payload_bytes -= bytes;
        self.usage.refund(bytes);
    }
    pub(crate) fn key(&self, value: Value) -> Result<HeapKey, HeapError> {
        let (heap, raw) = value.object_parts().ok_or(HeapError::WrongKind)?;
        if heap != self.id {
            return Err(HeapError::WrongHeap);
        }
        heap_key(raw).ok_or(HeapError::InvalidHandle)
    }
    pub(crate) fn object(&self, key: HeapKey) -> Result<&Object, HeapError> {
        let obj = self
            .objects
            .get(key)
            .filter(|o| !o.dying)
            .ok_or(HeapError::InvalidHandle)?;
        if self.gc.cutoff() && obj.mark != self.gc.cycle {
            return Err(HeapError::UnconfirmedLive);
        }
        Ok(obj)
    }
    fn validate_value(&self, value: Value) -> Result<(), HeapError> {
        if value.object_parts().is_some() {
            self.object(self.key(value)?)?;
        }
        Ok(())
    }
    pub(crate) fn shade(&mut self, value: Value) {
        if self.gc.phase != GcPhase::Mark {
            return;
        }
        let Some((_, raw)) = value.object_parts() else {
            return;
        };
        let key = heap_key(raw).expect("validated full key");
        if let Some(obj) = self.objects.get_mut(key)
            && !obj.dying
            && obj.mark != self.gc.cycle
        {
            obj.mark = self.gc.cycle;
            obj.scan = 0;
            obj.metadata_scanned = false;
            obj.reactive_scanned = false;
            obj.queued = true;
            assert!(self.gray.len() < self.limits.max_objects as usize);
            self.gray.push_back(key);
        }
    }
    fn barrier(&mut self, value: Value) -> Result<(), HeapError> {
        self.validate_value(value)?;
        self.shade(value);
        Ok(())
    }
    pub(crate) fn root_insertion_barrier(&mut self, value: Value) -> Result<(), HeapError> {
        self.barrier(value)
    }
    fn input_values(&self, values: &[Value]) -> Result<(), HeapError> {
        if values.len() as u64 > self.limits.max_mutation_values as u64 {
            return Err(HeapError::OperationLimit {
                requested: values.len() as u64,
                limit: self.limits.max_mutation_values as u64,
            });
        }
        for &v in values {
            self.validate_value(v)?;
        }
        Ok(())
    }
    fn input_bytes(&self, input: &[u8]) -> Result<(), HeapError> {
        if input.len() as u64 > self.limits.max_mutation_bytes as u64 {
            return Err(HeapError::OperationLimit {
                requested: input.len() as u64,
                limit: self.limits.max_mutation_bytes as u64,
            });
        }
        Ok(())
    }
    pub(crate) fn allocate(
        &mut self,
        kind: ObjectKind,
        payload: Payload,
        weak: Option<HeapHandle>,
        charged: u64,
    ) -> Value {
        // All capacity/budget/type validation and initial-edge barriers preceded this.
        let chunks = match &payload {
            Payload::Values(v) | Payload::Table(v) => v.owned_chunks(),
            Payload::Bytes(b) => b.owned_chunks(),
            _ => 0,
        };
        self.charge(charged, chunks);
        self.gc.allocated_objects = self.gc.allocated_objects.saturating_add(1);
        let mark = if self.gc.phase == GcPhase::Idle {
            0
        } else {
            self.gc.cycle
        };
        let key = self.objects.insert(Object {
            kind,
            host_validator: None,
            host_model_value: false,
            host_await: None,
            host_call: None,
            observable_fields: 0,
            observable_writable: 0,
            property_owner: None,
            model: false,
            readonly_collection: false,
            model_visit: 0,
            model_metadata: Value::NULL,
            reactive_subscribers: Value::NULL,
            reactive_scanned: false,
            derived: false,
            derived_dirty: false,
            evaluation: self
                .reactive_capture
                .and_then(|output| {
                    self.host_payload(output, DerivedSlot::Evaluation as u32)
                        .ok()
                })
                .and_then(Value::as_int)
                .unwrap_or(0) as u64,
            property_generation: 0,
            payload,
            weak,
            mark,
            scan: 0,
            metadata_scanned: false,
            queued: self.gc.phase == GcPhase::Mark,
            dying: false,
            content_version: 0,
            structure_version: 0,
            charged,
        });
        self.allocations.push_back(key);
        if self.gc.phase == GcPhase::Mark {
            self.gray.push_back(key);
        }
        Value::object(self.id, key.data().as_ffi())
    }
    fn allocate_values(&mut self, kind: ObjectKind, input: &[Value]) -> Result<Value, HeapError> {
        self.input_values(input)?;
        self.ensure_object_slot()?;
        if kind == ObjectKind::Cell {
            self.shade(input[0]);
            return Ok(self.allocate(
                kind,
                Payload::Cell(crate::cell::CellState {
                    value: Some(input[0]),
                    contract: crate::CellContract::untyped_mutable(),
                }),
                None,
                0,
            ));
        }
        let bytes = Values::allocation_bytes(input.len() as u64);
        self.ensure_bytes(bytes)?;
        let mut values = Values::default();
        for &v in input {
            self.shade(v);
            values.push(v);
        }
        Ok(self.allocate(kind, Payload::Values(values), None, bytes))
    }
    pub fn alloc_cell(&mut self, value: Value) -> Result<Value, HeapError> {
        self.allocate_values(ObjectKind::Cell, &[value])
    }
    pub fn alloc_array(&mut self, values: &[Value]) -> Result<Value, HeapError> {
        self.allocate_values(ObjectKind::Array, values)
    }
    /// Freeze a private, unpublished array builder without copying its edges.
    pub fn seal_tuple(&mut self, builder: Value) -> Result<(), HeapError> {
        let key = self.key(builder)?;
        if self.object(key)?.kind != ObjectKind::Array {
            return Err(HeapError::WrongKind);
        }
        self.objects.get_mut(key).unwrap().kind = ObjectKind::Tuple;
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        Ok(())
    }
    pub fn alloc_tuple(&mut self, values: &[Value]) -> Result<Value, HeapError> {
        self.allocate_values(ObjectKind::Tuple, values)
    }
    /// Storage schema only: fields are strong edges, without language/native execution.
    pub fn alloc_record(&mut self, kind: ObjectKind, fields: &[Value]) -> Result<Value, HeapError> {
        if !kind.record() {
            return Err(HeapError::WrongKind);
        }
        self.allocate_values(kind, fields)
    }
    /// A stable host identity with script-visible properties and private strong edges.
    pub fn alloc_host(
        &mut self,
        fields: Value,
        payload: &[Value],
        validator: Option<HostPropertyValidator>,
    ) -> Result<Value, HeapError> {
        self.host_record(fields, payload, false, validator)
    }
    /// An immutable host data value, shared by identity when copying model defaults.
    /// The host must supply immutable field/payload graphs and retain that contract;
    /// scripts cannot write its properties. Resources and ordinary hosts use alloc_host.
    pub fn alloc_immutable_host(
        &mut self,
        fields: Value,
        payload: &[Value],
    ) -> Result<Value, HeapError> {
        let host = self.host_record(
            fields,
            payload,
            false,
            Some(|_, _, _, _| Err(HeapError::ReadOnly)),
        )?;
        let key = self.key(host)?;
        self.objects.get_mut(key).unwrap().host_model_value = true;
        Ok(host)
    }
    pub(crate) fn is_immutable_host(&self, host: Value) -> Result<bool, HeapError> {
        self.host_fields(host)?;
        Ok(self.object(self.key(host)?)?.host_model_value)
    }
    /// Declare that this host value participates in trailing children composition.
    pub fn alloc_composable_host(
        &mut self,
        fields: Value,
        payload: &[Value],
        validator: Option<HostPropertyValidator>,
    ) -> Result<Value, HeapError> {
        self.host_record(fields, payload, true, validator)
    }
    fn host_record(
        &mut self,
        fields: Value,
        payload: &[Value],
        composable: bool,
        validator: Option<HostPropertyValidator>,
    ) -> Result<Value, HeapError> {
        self.table_len(fields)?;
        let mut values = Vec::with_capacity(payload.len() + 4);
        values.extend([
            fields,
            Value::int(i64::from(composable)),
            Value::NULL,
            Value::NULL,
        ]);
        values.extend_from_slice(payload);
        let host = self.allocate_values(ObjectKind::Host, &values)?;
        let key = self.key(host)?;
        self.objects.get_mut(key).unwrap().host_validator = validator;
        Ok(host)
    }
    pub fn host_fields(&self, host: Value) -> Result<Value, HeapError> {
        if self.info(host)?.kind != ObjectKind::Host {
            return Err(HeapError::WrongKind);
        }
        let fields = self.get_field(host, 0)?;
        if fields.same_bits(Value::NULL) {
            return Err(HeapError::RetiredHost);
        }
        Ok(fields)
    }
    pub(crate) fn host_await(&self, host: Value) -> Result<crate::bytecode::NativeId, HeapError> {
        self.host_fields(host)?;
        self.object(self.key(host)?)?
            .host_await
            .ok_or(HeapError::WrongKind)
    }
    pub(crate) fn host_call(&self, host: Value) -> Result<crate::bytecode::FunctionId, HeapError> {
        self.host_fields(host)?;
        self.object(self.key(host)?)?
            .host_call
            .ok_or(HeapError::WrongKind)
    }
    pub(crate) fn set_host_call(
        &mut self,
        host: Value,
        function: crate::bytecode::FunctionId,
    ) -> Result<(), HeapError> {
        self.host_fields(host)?;
        let key = self.key(host)?;
        self.objects.get_mut(key).unwrap().host_call = Some(function);
        Ok(())
    }
    pub(crate) fn set_host_await(
        &mut self,
        host: Value,
        native: crate::bytecode::NativeId,
    ) -> Result<(), HeapError> {
        self.host_fields(host)?;
        let key = self.key(host)?;
        self.objects.get_mut(key).unwrap().host_await = Some(native);
        Ok(())
    }
    pub fn host_payload_len(&self, host: Value) -> Result<u32, HeapError> {
        let info = self.info(host)?;
        if info.kind != ObjectKind::Host {
            return Err(HeapError::WrongKind);
        }
        info.len.checked_sub(4).ok_or(HeapError::WrongKind)
    }
    pub fn host_payload(&self, host: Value, index: u32) -> Result<Value, HeapError> {
        if index >= self.host_payload_len(host)? {
            return Err(HeapError::OutOfBounds);
        }
        self.get_field(host, index + 4)
    }
    pub fn set_host_payload(
        &mut self,
        host: Value,
        index: u32,
        value: Value,
    ) -> Result<(), HeapError> {
        self.host_fields(host)?;
        if index >= self.host_payload_len(host)? {
            return Err(HeapError::OutOfBounds);
        }
        // Private continuation/scheduling state is a traced edge, not an
        // observable property write. Public host fields live in their own table.
        self.barrier(value)?;
        let key = self.key(host)?;
        let object = self.objects.get_mut(key).unwrap();
        let Payload::Values(values) = &mut object.payload else {
            unreachable!("validated host storage")
        };
        values.set(index + 4, value);
        object.content_version = object.content_version.wrapping_add(1);
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        Ok(())
    }
    pub(crate) fn validate_host_property(
        &self,
        host: Value,
        index: u32,
        value: Value,
    ) -> Result<(), HeapError> {
        self.property_fields(host)?;
        if self.is_model_cell(host)? {
            if index != 0 {
                return Err(HeapError::OutOfBounds);
            }
            let contract = self.cell_info(host)?.contract;
            let accepted = if let (Some(ty), Some(code)) = (contract.type_id, &self.code) {
                code.checks.accepts(code.package(), self, ty, value)?
            } else {
                self.accepts_builtin(contract.ty, value)?
            };
            return if accepted {
                Ok(())
            } else {
                Err(HeapError::TypeMismatch)
            };
        }
        if let Some(validate) = self.object(self.key(host)?)?.host_validator {
            validate(&PropertyRead::current(self), host, index, value)?;
        }
        Ok(())
    }
    pub fn is_composable_host(&self, host: Value) -> Result<bool, HeapError> {
        if host.tag() != crate::value::ValueTag::Object || self.info(host)?.kind != ObjectKind::Host
        {
            return Ok(false);
        }
        self.host_fields(host)?;
        Ok(self
            .get_field(host, 1)?
            .as_int()
            .is_some_and(|flags| flags & 1 != 0))
    }
    pub fn retire_host(&mut self, host: Value) -> Result<(), HeapError> {
        self.disconnect_reactive_source(host)?;
        self.disconnect_host_properties(host)?;
        let slots = self.host_payload_len(host)? + 4;
        for index in 0..slots {
            if index != 1 {
                self.set_field(host, index, Value::NULL)?;
            }
        }
        Ok(())
    }

    /// Opt into per-field script write tracking. Storage is a traced, budgeted
    /// array; a host's own table_store updates do not count as script writes.
    pub fn track_host_writes(&mut self, host: Value) -> Result<(), HeapError> {
        let fields = self.property_fields(host)?;
        let mut versions = self.property_versions(host)?;
        if versions.same_bits(Value::NULL) {
            versions = self.alloc_array(&[])?;
            self.set_property_versions(host, versions)?;
        }
        let count = if self.is_model_table(host)? {
            self.object(self.key(host)?)?.observable_fields
        } else if self.is_model_cell(host)? {
            1
        } else {
            self.table_len(fields)?
        };
        while self.sequence_len(versions)? < count {
            self.property_traversal_work
                .set(self.property_traversal_work.get() + 4);
            self.array_push(versions, Value::int(0))?;
        }
        Ok(())
    }
    pub fn host_write_version(&self, host: Value, field: u32) -> Result<u64, HeapError> {
        self.property_fields(host)?;
        Ok(self
            .sequence_get(self.property_versions(host)?, field)?
            .as_int()
            .ok_or(HeapError::WrongKind)? as u64)
    }
    pub(crate) fn record_host_write(&mut self, host: Value, field: u32) -> Result<(), HeapError> {
        let versions = self.property_versions(host)?;
        if !versions.same_bits(Value::NULL) {
            while self.sequence_len(versions)? <= field {
                self.array_push(versions, Value::int(0))?;
            }
            let version = self.sequence_get(versions, field)?.as_int().unwrap();
            self.set_field(versions, field, Value::int(version.wrapping_add(1)))?;
        }
        Ok(())
    }
    fn allocate_bytes(&mut self, kind: ObjectKind, input: &[u8]) -> Result<Value, HeapError> {
        u32::try_from(input.len()).map_err(|_| HeapError::OutOfBounds)?;
        self.ensure_object_slot()?;
        let chunks = Bytes::chunks_for_len(input.len() as u64);
        self.ensure_bytes(chunks * BYTE_CHUNK_BYTES)?;
        let mut bytes = Bytes::default();
        bytes.append(input);
        Ok(self.allocate(kind, Payload::Bytes(bytes), None, chunks * BYTE_CHUNK_BYTES))
    }
    pub fn alloc_string(&mut self, value: &str) -> Result<Value, HeapError> {
        self.allocate_bytes(ObjectKind::String, value.as_bytes())
    }
    pub(crate) fn alloc_constant_bytes(
        &mut self,
        kind: ObjectKind,
        code: std::rc::Rc<crate::fault::CodeLease>,
        id: crate::bytecode::ConstId,
    ) -> Result<Value, HeapError> {
        self.ensure_object_slot()?;
        Ok(self.allocate(kind, Payload::Bytes(Bytes::constant(code, id)), None, 0))
    }
    pub fn alloc_bytes(&mut self, value: &[u8]) -> Result<Value, HeapError> {
        self.allocate_bytes(ObjectKind::Bytes, value)
    }
    pub fn alloc_text_buffer(&mut self, value: &str) -> Result<Value, HeapError> {
        self.allocate_bytes(ObjectKind::TextBuffer, value.as_bytes())
    }
    pub fn alloc_byte_buffer(&mut self, value: &[u8]) -> Result<Value, HeapError> {
        self.allocate_bytes(ObjectKind::ByteBuffer, value)
    }
    // Only the executor owns this unpublished buffer, copied from valid strings.
    /// Freeze a private byte builder after the caller has validated UTF-8.
    pub fn seal_string(&mut self, builder: Value) -> Result<(), HeapError> {
        let key = self.key(builder)?;
        if self.object(key)?.kind != ObjectKind::ByteBuffer {
            return Err(HeapError::WrongKind);
        }
        self.objects.get_mut(key).unwrap().kind = ObjectKind::String;
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        Ok(())
    }
    /// Freeze an unpublished byte builder without copying its payload.
    pub fn seal_bytes(&mut self, builder: Value) -> Result<(), HeapError> {
        let key = self.key(builder)?;
        if self.object(key)?.kind != ObjectKind::ByteBuffer {
            return Err(HeapError::WrongKind);
        }
        self.objects.get_mut(key).unwrap().kind = ObjectKind::Bytes;
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        Ok(())
    }
    pub fn info(&self, value: Value) -> Result<ObjectInfo, HeapError> {
        let obj = self.object(self.key(value)?)?;
        Ok(ObjectInfo {
            kind: obj.kind,
            len: match &obj.payload {
                Payload::Table(values) => values.len / 2,
                _ => obj.payload.len(),
            },
            content_version: obj.content_version,
            structure_version: obj.structure_version,
        })
    }
    pub fn get_field(&self, value: Value, position: u32) -> Result<Value, HeapError> {
        let obj = self.object(self.key(value)?)?;
        if !obj.kind.values() || matches!(obj.payload, Payload::Table(_)) {
            return Err(HeapError::WrongKind);
        }
        if let Payload::Cell(cell) = &obj.payload {
            if position != 0 {
                return Err(HeapError::OutOfBounds);
            }
            return cell.value.ok_or(HeapError::UninitializedCell);
        }
        obj.payload.edge(position).ok_or(HeapError::OutOfBounds)
    }
    fn editable_values(&self, key: HeapKey) -> Result<&Object, HeapError> {
        let obj = self.object(key)?;
        if !obj.kind.values()
            || matches!(obj.payload, Payload::Table(_))
            || matches!(
                obj.kind,
                ObjectKind::Tuple
                    | ObjectKind::Dependencies
                    | ObjectKind::Closure
                    | ObjectKind::BoundBuiltin
                    | ObjectKind::BoundNative
                    | ObjectKind::BoundScript
                    | ObjectKind::Super
                    | ObjectKind::PropertyRef
            )
        {
            return Err(HeapError::WrongKind);
        }
        Ok(obj)
    }
    /// Cell/array/record slot overwrite. Tuple storage never exposes a write.
    pub fn set_field(
        &mut self,
        target: Value,
        position: u32,
        value: Value,
    ) -> Result<(), HeapError> {
        let key = self.key(target)?;
        if self.object(key)?.kind == ObjectKind::Cell {
            if position != 0 {
                return Err(HeapError::OutOfBounds);
            }
            return self.cell_write(target, value);
        }
        let obj = self.editable_values(key)?;
        if position >= obj.payload.len() {
            return Err(HeapError::OutOfBounds);
        }
        self.adopt_model_value(target, value)?;
        self.barrier(value)?;
        let obj = self.objects.get_mut(key).unwrap();
        match &mut obj.payload {
            Payload::Values(values) => values.set(position, value),
            Payload::Cell(_) => unreachable!(),
            _ => unreachable!(),
        }
        obj.content_version = obj.content_version.wrapping_add(1);
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        self.invalidate_reactive(target, None)?;
        Ok(())
    }
    pub fn array_push(&mut self, target: Value, value: Value) -> Result<(), HeapError> {
        let key = self.key(target)?;
        let obj = self.object(key)?;
        if obj.kind != ObjectKind::Array {
            return Err(HeapError::WrongKind);
        }
        let Payload::Values(values) = &obj.payload else {
            unreachable!()
        };
        values.len.checked_add(1).ok_or(HeapError::OutOfBounds)?;
        let extra = Values::growth_bytes(values.len.into(), 1);
        self.ensure_bytes(extra)?;
        self.adopt_model_value_reserving(target, value, extra)?;
        self.barrier(value)?;
        self.charge(extra, u64::from(extra != 0));
        let obj = self.objects.get_mut(key).unwrap();
        if let Payload::Values(values) = &mut obj.payload {
            values.push(value);
        }
        obj.charged += extra;
        obj.content_version = obj.content_version.wrapping_add(1);
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        obj.structure_version = obj.structure_version.wrapping_add(1);
        self.invalidate_reactive(target, None)?;
        Ok(())
    }
    /// Packed arguments use alternating name/value entries. Validate shape at
    /// every use: bytecode verification does not prove a private ArgsNew origin.
    pub(crate) fn packed_array_len(&self, target: Value) -> Result<u32, HeapError> {
        let obj = self.object(self.key(target)?)?;
        if obj.kind != ObjectKind::Array {
            return Err(HeapError::WrongKind);
        }
        let len = obj.payload.len();
        if len % 2 != 0 {
            return Err(HeapError::TypeMismatch);
        }
        Ok(len)
    }
    /// Publish a complete pair or leave the array untouched. Shape, arithmetic,
    /// allocation and both values are checked before any payload mutation.
    pub(crate) fn packed_pair_push(
        &mut self,
        target: Value,
        name: Value,
        value: Value,
    ) -> Result<(), HeapError> {
        let len = self.packed_array_len(target)?;
        len.checked_add(2).ok_or(HeapError::OutOfBounds)?;
        let extra = Values::growth_bytes(len.into(), 2);
        self.ensure_bytes(extra)?;
        self.validate_value(name)?;
        self.validate_value(value)?;
        let key = self.key(target)?;
        self.shade(name);
        self.shade(value);
        self.charge(extra, u64::from(extra != 0));
        let obj = self.objects.get_mut(key).unwrap();
        let Payload::Values(values) = &mut obj.payload else {
            unreachable!()
        };
        values.push(name);
        values.push(value);
        obj.charged += extra;
        obj.content_version = obj.content_version.wrapping_add(1);
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        obj.structure_version = obj.structure_version.wrapping_add(1);
        Ok(())
    }
    pub fn array_pop(&mut self, target: Value) -> Result<Option<Value>, HeapError> {
        let key = self.key(target)?;
        let obj = self.object(key)?;
        if obj.kind != ObjectKind::Array {
            return Err(HeapError::WrongKind);
        }
        let value = obj
            .payload
            .len()
            .checked_sub(1)
            .and_then(|n| obj.payload.edge(n));
        // A removed edge returned to the caller is conservatively shaded during marking.
        if let Some(v) = value {
            self.barrier(v)?;
        }
        let obj = self.objects.get_mut(key).unwrap();
        let Payload::Values(values) = &mut obj.payload else {
            unreachable!()
        };
        let Some((value, freed)) = values.pop() else {
            return Ok(None);
        };
        obj.charged -= freed;
        obj.content_version = obj.content_version.wrapping_add(1);
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        obj.structure_version = obj.structure_version.wrapping_add(1);
        self.freed(freed);
        self.invalidate_reactive(target, None)?;
        Ok(Some(value))
    }
    pub fn array_swap(&mut self, target: Value, a: u32, b: u32) -> Result<(), HeapError> {
        let key = self.key(target)?;
        if self.object(key)?.kind != ObjectKind::Array {
            return Err(HeapError::WrongKind);
        }
        let left = self.get_field(target, a)?;
        let right = self.get_field(target, b)?;
        // Shade both before moving an unscanned edge behind the persistent cursor.
        self.barrier(left)?;
        self.barrier(right)?;
        let obj = self.objects.get_mut(key).unwrap();
        if let Payload::Values(values) = &mut obj.payload {
            values.set(a, right);
            values.set(b, left);
        }
        obj.content_version = obj.content_version.wrapping_add(1);
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        obj.structure_version = obj.structure_version.wrapping_add(1);
        self.invalidate_reactive(target, None)?;
        Ok(())
    }
    pub(crate) fn append_buffer(
        &mut self,
        target: Value,
        input: &[u8],
        kind: ObjectKind,
    ) -> Result<(), HeapError> {
        self.input_bytes(input)?;
        let key = self.key(target)?;
        let obj = self.object(key)?;
        if obj.kind != kind {
            return Err(HeapError::WrongKind);
        }
        let Payload::Bytes(bytes) = &obj.payload else {
            unreachable!()
        };
        u32::try_from(input.len())
            .ok()
            .and_then(|n| bytes.len.checked_add(n))
            .ok_or(HeapError::OutOfBounds)?;
        let chunks = bytes.extra_chunks(input.len());
        self.ensure_bytes(chunks * BYTE_CHUNK_BYTES)?;
        self.charge(chunks * BYTE_CHUNK_BYTES, chunks);
        let obj = self.objects.get_mut(key).unwrap();
        if let Payload::Bytes(bytes) = &mut obj.payload {
            bytes.append(input);
        }
        obj.charged += chunks * BYTE_CHUNK_BYTES;
        if !input.is_empty() {
            obj.content_version = obj.content_version.wrapping_add(1);
            self.mutation_revision = self.mutation_revision.wrapping_add(1);
        }
        self.invalidate_reactive(target, None)?;
        Ok(())
    }
    pub fn text_append(&mut self, target: Value, input: &str) -> Result<(), HeapError> {
        self.append_buffer(target, input.as_bytes(), ObjectKind::TextBuffer)
    }
    pub fn bytes_append(&mut self, target: Value, input: &[u8]) -> Result<(), HeapError> {
        self.append_buffer(target, input, ObjectKind::ByteBuffer)
    }
    /// Caller-owned read buffer; never returns a raw heap pointer or allocates a copy.
    pub fn read_bytes(
        &self,
        target: Value,
        offset: u32,
        output: &mut [u8],
    ) -> Result<usize, HeapError> {
        self.input_bytes(output)?;
        let obj = self.object(self.key(target)?)?;
        let Payload::Bytes(bytes) = &obj.payload else {
            return Err(HeapError::WrongKind);
        };
        if offset > bytes.len {
            return Err(HeapError::OutOfBounds);
        }
        Ok(bytes.read(offset, output))
    }
    /// A bounded immutable chunk borrowed only for the current executor step.
    pub fn byte_segment(&self, target: Value, offset: u32) -> Result<&[u8], HeapError> {
        let obj = self.object(self.key(target)?)?;
        let Payload::Bytes(bytes) = &obj.payload else {
            return Err(HeapError::WrongKind);
        };
        if offset > bytes.len {
            return Err(HeapError::OutOfBounds);
        }
        let segment = bytes.segment(offset);
        Ok(&segment[..segment.len().min(self.limits.max_mutation_bytes as usize)])
    }
    pub fn handle(&self, value: Value) -> Result<HeapHandle, HeapError> {
        let key = self.key(value)?;
        self.object(key)?;
        Ok(HeapHandle {
            heap: self.id,
            epoch: self.epoch,
            key: key.data().as_ffi(),
            _thread: PhantomData,
        })
    }
    pub fn import_handle(&self, handle: HeapHandle) -> Result<Value, HeapError> {
        self.scope(handle)?;
        let value = Value::object(self.id, handle.key);
        self.validate_value(value)?;
        Ok(value)
    }
    fn scope(&self, handle: HeapHandle) -> Result<(), HeapError> {
        if handle.heap != self.id {
            return Err(HeapError::WrongHeap);
        }
        if handle.epoch != self.epoch {
            return Err(HeapError::WrongEpoch);
        }
        Ok(())
    }
    pub fn downgrade(&self, value: Value) -> Result<WeakHandle, HeapError> {
        if !self.object(self.key(value)?)?.kind.weak_target() {
            return Err(HeapError::WrongKind);
        }
        Ok(WeakHandle(self.handle(value)?))
    }
    pub fn upgrade(&mut self, weak: WeakHandle) -> Result<Option<Value>, HeapError> {
        self.scope(weak.0)?;
        let Some(key) = heap_key(weak.0.key) else {
            return Ok(None);
        };
        let Some(obj) = self.objects.get(key).filter(|o| !o.dying) else {
            return Ok(None);
        };
        if self.gc.cutoff() && obj.mark != self.gc.cycle {
            return Ok(None);
        }
        let value = Value::object(self.id, weak.0.key);
        self.shade(value);
        Ok(Some(value))
    }
    pub fn alloc_weakref(&mut self, target: Value) -> Result<Value, HeapError> {
        let weak = self.downgrade(target)?;
        self.ensure_object_slot()?;
        Ok(self.allocate(ObjectKind::WeakRef, Payload::Empty, Some(weak.0), 0))
    }
    pub fn upgrade_weakref(&mut self, reference: Value) -> Result<Option<Value>, HeapError> {
        let obj = self.object(self.key(reference)?)?;
        if obj.kind != ObjectKind::WeakRef {
            return Err(HeapError::WrongKind);
        }
        let weak = obj.weak;
        match weak {
            Some(h) => self.upgrade(WeakHandle(h)),
            None => Ok(None),
        }
    }
    /// Inspect an internal weak subscription without retaining its target in
    /// the current GC cycle. The value must stay within a transaction that
    /// cannot run collection; use `upgrade_weakref` when it needs to escape.
    pub fn peek_weakref(&self, reference: Value) -> Result<Option<Value>, HeapError> {
        let reference = self.object(self.key(reference)?)?;
        if reference.kind != ObjectKind::WeakRef {
            return Err(HeapError::WrongKind);
        }
        let Some(handle) = reference.weak else {
            return Ok(None);
        };
        let Some(object) = heap_key(handle.key).and_then(|key| self.objects.get(key)) else {
            return Ok(None);
        };
        if object.dying || (self.gc.cutoff() && object.mark != self.gc.cycle) {
            return Ok(None);
        }
        Ok(Some(Value::object(self.id, handle.key)))
    }
    pub(crate) fn weak_confirmed(&self, handle: HeapHandle) -> bool {
        heap_key(handle.key)
            .and_then(|k| self.objects.get(k))
            .is_some_and(|o| !o.dying && o.mark == self.gc.cycle)
    }
    pub fn add_root(
        &mut self,
        owner: RootOwner,
        kind: RootKind,
        value: Value,
    ) -> Result<RootToken, HeapError> {
        self.ensure_root_slot()?;
        self.barrier(value)?;
        let key = self.roots.insert(Root { value, owner, kind });
        self.root_index[index(key)] = Some(key);
        self.root_scan_len = self.root_scan_len.max(index(key) + 1);
        Ok(RootToken {
            heap: self.id,
            epoch: self.epoch,
            key,
            _thread: PhantomData,
        })
    }
    fn root_key(&self, token: RootToken) -> Result<RootKey, HeapError> {
        if token.heap != self.id {
            return Err(HeapError::WrongHeap);
        }
        if token.epoch != self.epoch {
            return Err(HeapError::WrongEpoch);
        }
        self.roots.get(token.key).ok_or(HeapError::InvalidRoot)?;
        Ok(token.key)
    }
    pub fn root_value(&self, token: RootToken) -> Result<Value, HeapError> {
        Ok(self.roots.get(self.root_key(token)?).unwrap().value)
    }
    pub fn root_owner(&self, token: RootToken) -> Result<(RootOwner, RootKind), HeapError> {
        let r = self.roots.get(self.root_key(token)?).unwrap();
        Ok((r.owner, r.kind))
    }
    pub fn set_root(&mut self, token: RootToken, value: Value) -> Result<(), HeapError> {
        let key = self.root_key(token)?;
        self.barrier(value)?;
        self.roots.get_mut(key).unwrap().value = value;
        Ok(())
    }
    pub fn release_root(&mut self, token: RootToken) -> Result<Value, HeapError> {
        let key = self.root_key(token)?;
        self.root_index[index(key)] = None;
        Ok(self.roots.remove(key).unwrap().value)
    }
    /// Bounded owner cancellation; cursor must start at zero and persist until done.
    /// New roots for a cancelled owner must be prevented by the owning host.
    pub fn release_owner_step(&mut self, owner: RootOwner, cursor: &mut u32, slots: u32) -> u32 {
        let mut released = 0;
        let end = (*cursor as usize)
            .saturating_add(slots as usize)
            .min(self.root_scan_len);
        while (*cursor as usize) < end {
            let position = *cursor as usize;
            if let Some(key) = self.root_index[position]
                && self.roots.get(key).is_some_and(|r| r.owner == owner)
            {
                self.roots.remove(key);
                self.root_index[position] = None;
                released += 1;
            }
            *cursor += 1;
        }
        released
    }
    /// Descriptor high-water bound, including sentinel and released holes.
    /// Reserved capacity that has never been used is not a scan position.
    pub fn root_slots(&self) -> u32 {
        self.root_scan_len as u32
    }
    pub(crate) fn scan_root(&mut self, position: usize) {
        if let Some(value) = self.root_index[position]
            .and_then(|k| self.roots.get(k))
            .map(|r| r.value)
        {
            self.shade(value);
        }
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        self.usage.0.heap_alive.set(false);
        self.objects.drop_storage();
        self.roots.drop_storage();
        self.ranges.drop_storage();
        drop(std::mem::take(&mut self.root_index));
        drop(std::mem::take(&mut self.range_index));
        drop(std::mem::take(&mut self.allocations));
        drop(std::mem::take(&mut self.gray));
        drop(std::mem::take(&mut self.release));
        drop(std::mem::take(&mut self.property_work));
        drop(std::mem::take(&mut self.property_batch));
        drop(std::mem::take(&mut self.property_batch_index));
        drop(std::mem::take(&mut self.property_bindings));
        drop(std::mem::take(&mut self.derived_disconnect_work));
        drop(std::mem::take(&mut self.model_work));
        drop(std::mem::take(&mut self.reactive_queue));
        // The Rc book itself remains physically live while an observer/pool owns it.
        self.usage
            .refund(self.reserved - BOOK_BYTES + self.payload_bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GcBudget;

    #[test]
    fn projection_watermark_covers_value_writes_private_payload_and_collection() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let table = heap.alloc_table().unwrap();
        let name = heap.alloc_string("value").unwrap();
        heap.table_store(table, 0, name, Value::int(1)).unwrap();
        let host = heap.alloc_host(table, &[Value::NULL], None).unwrap();
        heap.observe_host_properties(host, 1, 0).unwrap();
        let array = heap.alloc_array(&[Value::int(1)]).unwrap();
        let cell = heap.alloc_cell(Value::int(1)).unwrap();
        let buffer = heap.alloc_byte_buffer(b"a").unwrap();
        let mut changed = |edit: &mut dyn FnMut(&mut Heap) -> Result<(), HeapError>| {
            let before = heap.mutation_revision();
            edit(&mut heap).unwrap();
            assert_ne!(heap.mutation_revision(), before);
        };
        changed(&mut |h| h.table_store(table, 0, name, Value::int(2)));
        changed(&mut |h| h.observe_host_properties(host, 1, 0));
        changed(&mut |h| h.write_host_property(PropertyId { host, field: 0 }, Value::int(3), true));
        changed(&mut |h| h.set_host_payload(host, 0, Value::int(4)));
        changed(&mut |h| h.set_field(array, 0, Value::int(5)));
        changed(&mut |h| h.array_push(array, Value::int(6)));
        changed(&mut |h| h.array_swap(array, 0, 1));
        changed(&mut |h| h.array_pop(array).map(|_| ()));
        changed(&mut |h| h.clear_collection(array));
        changed(&mut |h| h.cell_write(cell, Value::int(7)));
        changed(&mut |h| h.bytes_append(buffer, b"b"));
        changed(&mut |h| h.seal_bytes(buffer));
        changed(&mut |h| h.observe_readonly_collection(array));
        let before = heap.mutation_revision();
        assert_eq!(heap.table_entry(table, 0).unwrap().1.as_int(), Some(3));
        assert_eq!(heap.host_payload(host, 0).unwrap().as_int(), Some(4));
        assert_eq!(
            heap.mutation_revision(),
            before,
            "reads preserve cached projections"
        );
        let handle = heap.handle(host).unwrap();
        collect(&mut heap);
        assert!(heap.import_handle(handle).is_err());
        assert_ne!(heap.mutation_revision(), before);
        let rebuilt = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        assert_ne!(
            heap.id(),
            rebuilt.id(),
            "new heaps never reuse cached stamps"
        );
    }

    fn collect(h: &mut Heap) {
        h.begin_collection().unwrap();
        while h.phase() != GcPhase::Idle {
            h.gc_step(GcBudget::default()).unwrap();
        }
    }
    #[test]
    fn entity_page_admission_and_drop_refund_only_owned_storage() {
        let limits = HeapLimits {
            max_objects: 3,
            ..HeapLimits::default()
        };
        let mut probe = Heap::new(SessionEpoch(1), limits).unwrap();
        let initial = probe.stats().reserved_bytes;
        probe.alloc_cell(Value::NULL).unwrap();
        let populated = probe.stats().reserved_bytes;
        assert!(
            populated - initial < 2_048,
            "small host quotas need no full 32-object page"
        );
        drop(probe);
        for extra in [0, 128, populated - initial] {
            let mut observation = None;
            let allocation = allocation_counter::measure(|| {
                let mut h = Heap::new(
                    SessionEpoch(1),
                    HeapLimits {
                        max_bytes: initial + extra,
                        ..limits
                    },
                )
                .unwrap();
                observation = Some(h.budget_usage());
                let result = h.alloc_cell(Value::int(42));
                if extra == populated - initial {
                    assert_eq!(h.cell_read(result.unwrap()).unwrap().as_int(), Some(42));
                    collect(&mut h);
                    assert_eq!(h.stats().live_objects, 0);
                    h.alloc_cell(Value::int(7)).unwrap();
                } else {
                    assert!(matches!(result, Err(HeapError::ResourceLimit { .. })));
                    assert_eq!(h.stats().live_objects, 0);
                    if extra != 0 {
                        assert!(
                            h.stats().reserved_bytes > initial,
                            "failed page allocation retains its admitted page directory"
                        );
                    }
                }
                assert_eq!(h.stats().charged_bytes, h.stats().reserved_bytes);
                drop(h);
            });
            assert_eq!(observation.unwrap().charged_bytes(), BOOK_BYTES);
            assert_eq!(
                allocation.bytes_current as u64, BOOK_BYTES,
                "only the retained budget observer remains allocated"
            );
        }
    }

    #[test]
    fn retirement_keeps_all_old_object_and_root_handles_invalid() {
        let limits = HeapLimits {
            max_objects: 3,
            max_roots: 3,
            ..HeapLimits::default()
        };
        let mut h = Heap::with_retirement(SessionEpoch(1), limits, 3).unwrap();
        let mut old = Vec::new();
        let mut tokens = Vec::new();
        for _ in 0..6 {
            let a = h.alloc_array(&[]).unwrap();
            let token = h.add_root(RootOwner(0), RootKind::Host, a).unwrap();
            old.push(h.handle(a).unwrap());
            tokens.push(token);
            h.release_root(token).unwrap();
            collect(&mut h);
            for &handle in &old {
                assert_eq!(
                    h.import_handle(handle).unwrap_err(),
                    HeapError::InvalidHandle
                );
            }
            for &token in &tokens {
                assert_eq!(h.root_value(token).unwrap_err(), HeapError::InvalidRoot);
            }
        }
        assert_eq!(h.stats().retired_objects, 3);
        assert_eq!(h.stats().retired_roots, 3);
        assert_eq!(h.alloc_array(&[]).unwrap_err(), HeapError::ObjectCapacity);
        assert_eq!(
            h.add_root(RootOwner(0), RootKind::Host, Value::NULL)
                .unwrap_err(),
            HeapError::RootCapacity
        );
        assert_eq!(h.stats().charged_bytes, h.stats().reserved_bytes);
    }
}
