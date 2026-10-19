use super::*;
use crate::snapshot::{
    Record, RecordKind, SnapshotError, SnapshotMap, SnapshotRegistry, VmSnapshot,
};
use crate::{BYTE_CHUNK_LEN, FrameSlots, RootRangeToken, VALUE_CHUNK_LEN};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Serialize, Deserialize)]
struct ObjectMeta {
    kind: ObjectKind,
    len: u32,
    layout: Layout,
    validator: Option<(String, u32)>,
    host_model_value: bool,
    host_await: Option<crate::bytecode::NativeId>,
    host_call: Option<crate::bytecode::FunctionId>,
    observable_fields: u32,
    observable_writable: u32,
    property_owner: Option<u64>,
    model: bool,
    readonly_collection: bool,
    model_metadata: Value,
    reactive_subscribers: Value,
    derived: bool,
    derived_dirty: bool,
    evaluation: u64,
    property_generation: u64,
    weak: Option<HeapHandle>,
    content_version: u64,
    structure_version: u64,
}
#[derive(Serialize, Deserialize)]
enum Layout {
    Cell(crate::CellContract, bool),
    Fault(crate::fault::FaultData),
    Values,
    Dependencies,
    Table,
    Bytes,
    Constant(crate::bytecode::ConstId),
    Empty,
}
#[derive(Serialize, Deserialize)]
struct SavedRoot {
    owner: RootOwner,
    kind: RootKind,
    value: Value,
}

impl serde::Serialize for RootToken {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.key.data().as_ffi().serialize(s)
    }
}
impl<'de> serde::Deserialize<'de> for RootToken {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let key = u64::deserialize(d)?;
        if heap_key(key).is_none() {
            return Err(serde::de::Error::custom("invalid snapshot root"));
        }
        Ok(Self {
            heap: HeapId(0),
            epoch: SessionEpoch(0),
            key: slotmap::KeyData::from_ffi(key).into(),
            _thread: PhantomData,
        })
    }
}
impl SnapshotMap {
    pub(crate) fn rebase_root(&self, root: RootToken) -> Result<RootToken, SnapshotError> {
        self.check(root.heap, Some(root.epoch))?;
        self.roots
            .get(&root.key.data().as_ffi())
            .copied()
            .ok_or_else(|| SnapshotError::Invalid("missing root".into()))
    }
}

/// A separate tracer avoids serializing unreachable allocations or GC bookkeeping.
pub(crate) struct GraphCapture {
    root: usize,
    range: usize,
    range_offset: Option<u32>,
    objects: Vec<HeapKey>,
    seen: HashSet<HeapKey>,
    object: usize,
    offset: Option<u32>,
}
impl GraphCapture {
    pub fn new(heap: &Heap) -> Result<Self, SnapshotError> {
        if heap.property_proposal.is_some() {
            return Err(SnapshotError::Busy(
                "property proposal must finish before capture",
            ));
        }
        let mut capture = Self {
            root: 0,
            range: 0,
            range_offset: None,
            objects: Vec::new(),
            seen: HashSet::new(),
            object: 0,
            offset: None,
        };
        if let Some(output) = heap.reactive_capture {
            capture.trace(heap, output)?;
        }
        Ok(capture)
    }
    fn trace(&mut self, heap: &Heap, value: Value) -> Result<(), SnapshotError> {
        if value.object_parts().is_some() {
            let key = heap.key(value)?;
            heap.object(key)?;
            if self.seen.insert(key) {
                self.objects.push(key);
            }
        }
        Ok(())
    }
    pub fn step(
        &mut self,
        heap: &Heap,
        slots: &FrameSlots,
        registry: &SnapshotRegistry,
        out: &mut VmSnapshot,
    ) -> Result<bool, SnapshotError> {
        if self.root < heap.root_index.len() {
            let key = heap.root_index[self.root];
            self.root += 1;
            if let Some(key) = key {
                let root = heap.roots.get(key).ok_or(HeapError::InvalidRoot)?;
                self.trace(heap, root.value)?;
                out.push(Record::new(
                    RecordKind::Root,
                    key.data().as_ffi(),
                    0,
                    &SavedRoot {
                        owner: root.owner,
                        kind: root.kind,
                        value: root.value,
                    },
                )?);
            }
            return Ok(false);
        }
        if self.range < heap.range_index.len() {
            let Some(key) = heap.range_index[self.range] else {
                self.range += 1;
                return Ok(false);
            };
            let range = RootRangeToken::new(heap, key);
            let descriptor = heap.range_descriptor(range)?;
            let raw = key.data().as_ffi();
            if let Some(offset) = self.range_offset {
                let end = offset
                    .saturating_add(crate::VALUE_CHUNK_LEN as u32)
                    .min(descriptor.len);
                let mut values = Vec::with_capacity((end - offset) as usize);
                for index in offset..end {
                    let value = match slots.read(crate::RangePosition { range, index }) {
                        Ok(value) => {
                            self.trace(heap, value)?;
                            Some(value)
                        }
                        Err(HeapError::UninitializedSlot) => None,
                        Err(error) => return Err(error.into()),
                    };
                    values.push(value);
                }
                out.push(Record::new(RecordKind::Slots, raw, offset, &values)?);
                if end == descriptor.len {
                    self.range += 1;
                    self.range_offset = None;
                } else {
                    self.range_offset = Some(end);
                }
            } else {
                out.push(Record::new(RecordKind::Range, raw, 0, &descriptor)?);
                if descriptor.len == 0 {
                    self.range += 1;
                } else {
                    self.range_offset = Some(0);
                }
            }
            return Ok(false);
        }
        let Some(&key) = self.objects.get(self.object) else {
            return Ok(true);
        };
        let raw = key.data().as_ffi();
        let object = heap.object(key)?;
        if let Some(offset) = self.offset {
            let len = object.payload.len();
            let end = if let Payload::Bytes(bytes) = &object.payload {
                let data = bytes.segment(offset).to_vec();
                let end = offset + data.len() as u32;
                out.push(Record {
                    kind: RecordKind::Bytes,
                    key: raw,
                    offset,
                    data,
                });
                end
            } else {
                let end = offset
                    .saturating_add(crate::VALUE_CHUNK_LEN as u32)
                    .min(len);
                let mut values = Vec::with_capacity((end - offset) as usize);
                for index in offset..end {
                    let value = object.payload.edge(index).ok_or(HeapError::OutOfBounds)?;
                    self.trace(heap, value)?;
                    values.push(value);
                }
                out.push(Record::new(RecordKind::Values, raw, offset, &values)?);
                end
            };
            if end == len {
                self.object += 1;
                self.offset = None;
            } else {
                self.offset = Some(end);
            }
        } else {
            self.trace(heap, object.model_metadata)?;
            self.trace(heap, object.reactive_subscribers)?;
            let layout = match &object.payload {
                Payload::Cell(cell) => Layout::Cell(cell.contract, cell.value.is_some()),
                Payload::Fault(fault) => Layout::Fault(fault.data),
                Payload::Values(_) => Layout::Values,
                Payload::Dependencies(_) => Layout::Dependencies,
                Payload::Table(_) => Layout::Table,
                Payload::Bytes(bytes) => {
                    bytes.constant_id().map_or(Layout::Bytes, Layout::Constant)
                }
                Payload::Empty => Layout::Empty,
            };
            let validator = if object.host_model_value {
                None
            } else {
                object
                    .host_validator
                    .map(|v| registry.identify(raw, v))
                    .transpose()?
            };
            let has_payload = object.payload.len() != 0 && !matches!(layout, Layout::Constant(_));
            out.push(Record::new(
                RecordKind::Object,
                raw,
                0,
                &ObjectMeta {
                    kind: object.kind,
                    len: object.payload.len(),
                    layout,
                    validator,
                    host_model_value: object.host_model_value,
                    host_await: object.host_await,
                    host_call: object.host_call,
                    observable_fields: object.observable_fields,
                    observable_writable: object.observable_writable,
                    property_owner: object.property_owner.map(|k| k.data().as_ffi()),
                    model: object.model,
                    readonly_collection: object.readonly_collection,
                    model_metadata: object.model_metadata,
                    reactive_subscribers: object.reactive_subscribers,
                    derived: object.derived,
                    derived_dirty: object.derived_dirty,
                    evaluation: object.evaluation,
                    property_generation: object.property_generation,
                    weak: object.weak,
                    content_version: object.content_version,
                    structure_version: object.structure_version,
                },
            )?);
            if !has_payload {
                self.object += 1;
            } else {
                self.offset = Some(0);
            }
        }
        Ok(false)
    }
}

impl Heap {
    pub(crate) fn snapshot_reactive_state(&self) -> (Option<Value>, u64) {
        (self.reactive_capture, self.evaluation)
    }

    pub(crate) fn restore_reactive_state(
        &mut self,
        output: Option<Value>,
        evaluation: u64,
    ) -> Result<(), SnapshotError> {
        if let Some(output) = output {
            let object = self.object(self.key(output)?)?;
            if !object.derived {
                return Err(SnapshotError::Invalid("reactive capture output".into()));
            }
        }
        self.reactive_capture = output;
        self.evaluation = evaluation;
        Ok(())
    }
}

pub(crate) fn allocate(
    heap: &mut Heap,
    slots: &mut FrameSlots,
    map: &mut SnapshotMap,
    record: &Record,
    registry: &SnapshotRegistry,
) -> Result<(), SnapshotError> {
    match record.kind {
        RecordKind::Object => {
            if map.objects.contains_key(&record.key) {
                return Err(SnapshotError::Invalid("duplicate object identity".into()));
            }
            let meta: ObjectMeta = record.read()?;
            heap.ensure_object_slot()?;
            let payload = match meta.layout {
                Layout::Cell(contract, _) if meta.kind == ObjectKind::Cell => {
                    Payload::Cell(crate::cell::CellState {
                        value: None,
                        contract,
                    })
                }
                Layout::Fault(data)
                    if matches!(
                        meta.kind,
                        ObjectKind::FaultEnvelope
                            | ObjectKind::FaultStack
                            | ObjectKind::FaultCause
                            | ObjectKind::CoreError
                    ) =>
                {
                    Payload::Fault(crate::fault::FaultPayload {
                        data,
                        edges: [Value::NULL; 3],
                        release_cursor: 0,
                        code: heap
                            .code
                            .as_ref()
                            .ok_or_else(|| {
                                SnapshotError::Invalid("fault without code lease".into())
                            })?
                            .clone(),
                    })
                }
                Layout::Values if meta.kind.values() => Payload::Values(Values::default()),
                Layout::Dependencies
                    if meta.kind == ObjectKind::Dependencies && meta.len % 2 == 0 =>
                {
                    heap.ensure_bytes(crate::dependencies::BASE_BYTES)?;
                    Payload::Dependencies(Box::default())
                }
                Layout::Table if meta.kind == ObjectKind::Table => {
                    Payload::Table(Values::default())
                }
                Layout::Bytes
                    if matches!(
                        meta.kind,
                        ObjectKind::String
                            | ObjectKind::Bytes
                            | ObjectKind::TextBuffer
                            | ObjectKind::ByteBuffer
                    ) =>
                {
                    Payload::Bytes(Bytes::default())
                }
                Layout::Constant(id) => {
                    use crate::bytecode::Const;
                    let code = heap.code.as_ref().ok_or_else(|| {
                        SnapshotError::Invalid("constant without code lease".into())
                    })?;
                    match (meta.kind, code.package().constants.get(id.index())) {
                        (ObjectKind::String, Some(Const::Utf8(_) | Const::Text { .. }))
                        | (ObjectKind::Bytes, Some(Const::Bytes(_))) => (),
                        _ => return Err(SnapshotError::Invalid("constant kind/index".into())),
                    }
                    Payload::Bytes(Bytes::constant(code.clone(), id))
                }
                Layout::Empty if meta.kind == ObjectKind::WeakRef => Payload::Empty,
                _ => {
                    return Err(SnapshotError::Invalid(format!(
                        "object {} payload kind",
                        record.key
                    )));
                }
            };
            let validator = if meta.host_model_value {
                Some(immutable as HostPropertyValidator)
            } else {
                meta.validator
                    .as_ref()
                    .map(|(name, version)| registry.resolve(name, *version))
                    .transpose()?
            };
            let charged = if matches!(payload, Payload::Dependencies(_)) {
                crate::dependencies::BASE_BYTES
            } else {
                0
            };
            let value = heap.allocate(meta.kind, payload, None, charged);
            let key = heap.key(value)?;
            heap.objects.get_mut(key).unwrap().host_validator = validator;
            map.objects.insert(record.key, value);
        }
        RecordKind::Range => {
            if map.ranges.contains_key(&record.key) {
                return Err(SnapshotError::Invalid("duplicate root range".into()));
            }
            let descriptor: RangeDescriptor = record.read()?;
            slots.reserve_task(heap, descriptor.len)?;
            let range = if descriptor.kind == RootKind::ModuleSet {
                slots.open_module_set(heap, descriptor.owner, descriptor.len)?
            } else {
                slots.open(heap, descriptor.owner, descriptor.kind, descriptor.len)?
            };
            map.ranges.insert(record.key, range);
        }
        _ => (),
    }
    Ok(())
}
fn immutable(_: &Heap, _: Value, _: u32, _: Value) -> Result<(), HeapError> {
    Err(HeapError::ReadOnly)
}

pub(crate) fn connect(
    heap: &mut Heap,
    slots: &mut FrameSlots,
    map: &mut SnapshotMap,
    record: &Record,
) -> Result<(), SnapshotError> {
    match record.kind {
        RecordKind::Root => {
            if map.roots.contains_key(&record.key) {
                return Err(SnapshotError::Invalid("duplicate root".into()));
            }
            let root: SavedRoot = record.read()?;
            let token = heap.add_root(root.owner, root.kind, map.value(root.value)?)?;
            map.roots.insert(record.key, token);
        }
        RecordKind::Slots => {
            let range = *map
                .ranges
                .get(&record.key)
                .ok_or(HeapError::InvalidRootRange)?;
            let values: Vec<Option<Value>> = record.read()?;
            if values.is_empty()
                || values.len() > VALUE_CHUNK_LEN
                || record
                    .offset
                    .checked_add(values.len() as u32)
                    .is_none_or(|end| end > heap.range_descriptor(range).map_or(0, |d| d.len))
            {
                return Err(SnapshotError::Invalid("root slot block length".into()));
            }
            for (i, value) in values.into_iter().enumerate() {
                let index = record
                    .offset
                    .checked_add(u32::try_from(i).map_err(|_| SnapshotError::Limit)?)
                    .ok_or(SnapshotError::Limit)?;
                if let Some(value) = value {
                    slots.write(
                        heap,
                        crate::RangePosition { range, index },
                        map.value(value)?,
                    )?;
                }
            }
        }
        RecordKind::Object => {
            let meta: ObjectMeta = record.read()?;
            let value = *map
                .objects
                .get(&record.key)
                .ok_or(HeapError::InvalidHandle)?;
            let key = heap.key(value)?;
            let obj = heap.objects.get_mut(key).unwrap();
            obj.host_model_value = meta.host_model_value;
            obj.host_await = meta.host_await;
            obj.host_call = meta.host_call;
            obj.observable_fields = meta.observable_fields;
            obj.observable_writable = meta.observable_writable;
            obj.property_owner = meta
                .property_owner
                .and_then(|key| map.objects.get(&key))
                .and_then(|v| v.object_parts())
                .and_then(|(_, key)| heap_key(key));
            obj.model = meta.model;
            obj.readonly_collection = meta.readonly_collection;
            obj.model_metadata = map.value(meta.model_metadata)?;
            obj.reactive_subscribers = map.value(meta.reactive_subscribers)?;
            obj.derived = meta.derived;
            obj.derived_dirty = meta.derived_dirty;
            obj.evaluation = meta.evaluation;
            obj.property_generation = meta.property_generation;
            obj.content_version = meta.content_version;
            obj.structure_version = meta.structure_version;
            obj.weak = meta.weak.and_then(|h| map.handle(h).ok());
            if let Payload::Fault(fault) = &mut obj.payload
                && let crate::fault::FaultData::Cause {
                    kind: crate::CauseKind::ReplacedExit(crate::ReplacedExit::Throw(ref mut id)),
                    ..
                } = fault.data
            {
                id.0 = map.handle(id.0)?;
            }
            heap.evaluation = heap.evaluation.max(meta.evaluation);
            if meta.derived && meta.derived_dirty {
                heap.reactive_queue.push_back(key);
            }
        }
        RecordKind::Values | RecordKind::Bytes => {
            let value = *map
                .objects
                .get(&record.key)
                .ok_or(HeapError::InvalidHandle)?;
            let key = heap.key(value)?;
            if record.kind == RecordKind::Bytes {
                let Payload::Bytes(bytes) = &heap.object(key)?.payload else {
                    return Err(HeapError::WrongKind.into());
                };
                if bytes.constant_id().is_some()
                    || bytes.len != record.offset
                    || record.data.len() > BYTE_CHUNK_LEN
                {
                    return Err(SnapshotError::Invalid("byte block offset/length".into()));
                }
                let extra = bytes.extra_chunks(record.data.len()) * BYTE_CHUNK_BYTES;
                heap.ensure_bytes(extra)?;
                heap.charge(extra, extra / BYTE_CHUNK_BYTES);
                let obj = heap.objects.get_mut(key).unwrap();
                let Payload::Bytes(bytes) = &mut obj.payload else {
                    unreachable!()
                };
                bytes.append(&record.data);
                obj.charged += extra;
            } else {
                let values: Vec<Value> = record.read()?;
                if values.len() > VALUE_CHUNK_LEN {
                    return Err(SnapshotError::Invalid("value block length".into()));
                }
                for (i, value) in values.into_iter().enumerate() {
                    let value = map.value(value)?;
                    let offset = record
                        .offset
                        .checked_add(i as u32)
                        .ok_or(SnapshotError::Limit)?;
                    let extra = match &heap.object(key)?.payload {
                        Payload::Values(v) | Payload::Table(v) if v.len == offset => {
                            u64::from(v.needs_chunk()) * VALUE_CHUNK_BYTES
                        }
                        Payload::Dependencies(d) if d.values.len == offset => {
                            d.restore_bytes(value)?
                        }
                        Payload::Cell(cell) if offset == 0 && cell.value.is_none() => 0,
                        Payload::Fault(_) if offset < 3 => 0,
                        _ => return Err(SnapshotError::Invalid("value block offset".into())),
                    };
                    heap.ensure_bytes(extra)?;
                    let chunks = match &heap.object(key)?.payload {
                        Payload::Dependencies(d) => u64::from(d.values.needs_chunk()),
                        _ => u64::from(extra != 0),
                    };
                    heap.charge(extra, chunks);
                    let obj = heap.objects.get_mut(key).unwrap();
                    match &mut obj.payload {
                        Payload::Values(v) | Payload::Table(v) => v.push(value),
                        Payload::Dependencies(d) => d.restore_push(value),
                        Payload::Cell(cell) => cell.value = Some(value),
                        Payload::Fault(fault) => fault.edges[offset as usize] = value,
                        _ => unreachable!(),
                    }
                    obj.charged += extra;
                }
            }
        }
        _ => (),
    }
    Ok(())
}

pub(crate) fn validate(
    heap: &Heap,
    map: &SnapshotMap,
    record: &Record,
) -> Result<(), SnapshotError> {
    if record.kind != RecordKind::Object {
        return Ok(());
    }
    let meta: ObjectMeta = record.read()?;
    let value = *map
        .objects
        .get(&record.key)
        .ok_or(HeapError::InvalidHandle)?;
    let obj = heap.object(heap.key(value)?)?;
    if obj.payload.len() != meta.len || meta.observable_writable > meta.observable_fields {
        return Err(SnapshotError::Invalid(format!(
            "object {} length/schema",
            record.key
        )));
    }
    if let Payload::Table(v) = &obj.payload
        && v.len % 2 != 0
    {
        return Err(SnapshotError::Invalid("odd table payload".into()));
    }
    Ok(())
}
