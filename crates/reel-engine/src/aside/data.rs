//! Code-independent values for system records. No VM handles cross the IO lane.
use super::AdapterError;
use aside_vm::{
    Heap, HeapHandle, HeapReservation, ObjectKind, RootKind, RootOwner, RootToken, Value, ValueTag,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
enum Atom {
    Null,
    Bool(bool),
    Int(i64),
    Float(u64),
    Object(u32),
}
#[derive(Debug, Serialize, Deserialize)]
enum Object {
    String(Vec<u8>),
    Bytes(Vec<u8>),
    Array(Vec<Atom>),
    Tuple(Vec<Atom>),
    Table(Vec<(Atom, Atom)>),
}
impl Object {
    fn bytes(&self) -> usize {
        match self {
            Self::String(v) | Self::Bytes(v) => v.capacity(),
            Self::Array(v) | Self::Tuple(v) => v.capacity() * size_of::<Atom>(),
            Self::Table(v) => v.capacity() * size_of::<(Atom, Atom)>(),
        }
    }
    fn len(&self) -> usize {
        match self {
            Self::String(v) | Self::Bytes(v) => v.len(),
            Self::Array(v) | Self::Tuple(v) => v.len(),
            Self::Table(v) => v.len(),
        }
    }
}

/// A detached data graph. Its format has no dependency on an ASB or session.
#[derive(Debug, Serialize, Deserialize)]
pub struct DataDocument {
    root: Atom,
    objects: Vec<Object>,
    #[serde(skip)]
    payload_bytes: usize,
}
impl DataDocument {
    pub fn retained_bytes(&self) -> u64 {
        (size_of::<Self>() + self.objects.capacity() * size_of::<Object>() + self.payload_bytes)
            as u64
    }
    pub fn write_to(&self, writer: impl std::io::Write) -> Result<(), postcard::Error> {
        postcard::to_io(self, writer).map(|_| ())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (mut document, tail): (Self, _) = postcard::take_from_bytes(bytes)?;
        if !tail.is_empty() {
            return Err("trailing system record data".into());
        }
        let check = |atom: Atom| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            if let Atom::Object(id) = atom {
                if id as usize >= document.objects.len() {
                    return Err("invalid system data reference".into());
                }
            }
            Ok(())
        };
        check(document.root)?;
        for object in &document.objects {
            match object {
                Object::String(bytes) => {
                    std::str::from_utf8(bytes)?;
                }
                Object::Bytes(_) => {}
                Object::Array(values) | Object::Tuple(values) => {
                    for &value in values {
                        check(value)?;
                    }
                }
                Object::Table(entries) => {
                    for &(key, value) in entries {
                        check(key)?;
                        check(value)?;
                    }
                }
            }
        }
        document.payload_bytes = document.objects.iter().map(Object::bytes).sum();
        Ok(document)
    }
}

struct Source {
    value: Value,
    version: u64,
    len: u32,
}
pub(super) struct Capture {
    document: DataDocument,
    sources: Vec<Source>,
    ids: HashMap<HeapHandle, u32>,
    cursor: usize,
    validation: usize,
    charge: HeapReservation,
}
impl Capture {
    pub fn new(heap: &Heap, value: Value) -> Result<Self, AdapterError> {
        let mut capture = Self {
            document: DataDocument {
                root: Atom::Null,
                objects: Vec::new(),
                payload_bytes: 0,
            },
            sources: Vec::new(),
            ids: HashMap::new(),
            cursor: 0,
            validation: 0,
            charge: heap.budget_usage().reserve(size_of::<Self>() as u64)?,
        };
        capture.document.root = capture.atom(heap, value)?;
        Ok(capture)
    }
    fn atom(&mut self, heap: &Heap, value: Value) -> Result<Atom, AdapterError> {
        Ok(match value.tag() {
            ValueTag::Null => Atom::Null,
            ValueTag::Bool => Atom::Bool(value.as_bool().unwrap()),
            ValueTag::Int => Atom::Int(value.as_int().unwrap()),
            ValueTag::Float => Atom::Float(value.as_float().unwrap().to_bits()),
            ValueTag::Object => {
                let handle = heap.handle(value)?;
                if let Some(&id) = self.ids.get(&handle) {
                    return Ok(Atom::Object(id));
                }
                let info = heap.info(value)?;
                let len = info.len as usize;
                let unit = match info.kind {
                    ObjectKind::String | ObjectKind::Bytes => 1,
                    ObjectKind::Array | ObjectKind::Tuple => size_of::<Atom>(),
                    ObjectKind::Table => size_of::<(Atom, Atom)>(),
                    _ => {
                        return Err(AdapterError::Input(
                            "persistent data requires plain values; convert instances, buffers and host resources to data first",
                        ));
                    }
                };
                // Include growing descriptor vectors, hash buckets and their old allocations.
                let overhead = 4
                    * (size_of::<Object>()
                        + size_of::<Source>()
                        + size_of::<(HeapHandle, u32)>()
                        + 16);
                self.charge
                    .resize(self.charge.bytes() + (overhead + len * unit) as u64)?;
                fn buffer<T>(len: usize) -> Result<Vec<T>, AdapterError> {
                    let mut v = Vec::new();
                    v.try_reserve_exact(len)
                        .map_err(|_| AdapterError::Allocation)?;
                    Ok(v)
                }
                let object = match info.kind {
                    ObjectKind::String => Object::String(buffer(len)?),
                    ObjectKind::Bytes => Object::Bytes(buffer(len)?),
                    ObjectKind::Array => Object::Array(buffer(len)?),
                    ObjectKind::Tuple => Object::Tuple(buffer(len)?),
                    ObjectKind::Table => Object::Table(buffer(len)?),
                    _ => unreachable!(),
                };
                let id = u32::try_from(self.document.objects.len())
                    .map_err(|_| AdapterError::Allocation)?;
                self.ids
                    .try_reserve(1)
                    .map_err(|_| AdapterError::Allocation)?;
                self.sources
                    .try_reserve(1)
                    .map_err(|_| AdapterError::Allocation)?;
                self.document
                    .objects
                    .try_reserve(1)
                    .map_err(|_| AdapterError::Allocation)?;
                self.document.payload_bytes += object.bytes();
                self.document.objects.push(object);
                self.sources.push(Source {
                    value,
                    version: info.content_version,
                    len: info.len,
                });
                self.ids.insert(handle, id);
                Atom::Object(id)
            }
        })
    }
    pub fn step(
        &mut self,
        heap: &Heap,
        work: u32,
        chunk: usize,
    ) -> Result<(bool, u32), AdapterError> {
        for used in 1..=work {
            if self.cursor == self.sources.len() {
                if self.validation == self.sources.len() {
                    return Ok((true, used));
                }
                let source = &self.sources[self.validation];
                if heap.info(source.value)?.content_version != source.version {
                    return Err(AdapterError::Input(
                        "persistent data changed during capture",
                    ));
                }
                self.validation += 1;
                continue;
            }
            let source = &self.sources[self.cursor];
            if heap.info(source.value)?.content_version != source.version {
                return Err(AdapterError::Input(
                    "persistent data changed during capture",
                ));
            }
            let offset = self.document.objects[self.cursor].len();
            if offset == source.len as usize {
                self.cursor += 1;
                continue;
            }
            let value = source.value;
            match &mut self.document.objects[self.cursor] {
                Object::String(bytes) | Object::Bytes(bytes) => {
                    let mut buffer = [0; 4096];
                    let n =
                        heap.read_bytes(value, offset as u32, &mut buffer[..chunk.min(4096)])?;
                    bytes.extend_from_slice(&buffer[..n]);
                }
                Object::Table(_) => {
                    let (key, value) = heap.table_entry(value, offset as u32)?;
                    let key = self.atom(heap, key)?;
                    let value = self.atom(heap, value)?;
                    let Object::Table(entries) = &mut self.document.objects[self.cursor] else {
                        unreachable!()
                    };
                    entries.push((key, value));
                }
                Object::Array(_) | Object::Tuple(_) => {
                    let value = self.atom(heap, heap.sequence_get(value, offset as u32)?)?;
                    let (Object::Array(values) | Object::Tuple(values)) =
                        &mut self.document.objects[self.cursor]
                    else {
                        unreachable!()
                    };
                    values.push(value);
                }
            }
        }
        Ok((false, work))
    }
    pub fn finish(self) -> (Arc<DataDocument>, HeapReservation) {
        (Arc::new(self.document), self.charge)
    }
}

pub(super) struct Restore {
    document: Arc<DataDocument>,
    root: RootToken,
    cursor: usize,
    offset: usize,
    sealing: bool,
    _charge: HeapReservation,
}
impl Restore {
    pub fn new(
        heap: &mut Heap,
        owner: RootOwner,
        document: Arc<DataDocument>,
    ) -> Result<Self, AdapterError> {
        let charge = heap.budget_usage().reserve(document.retained_bytes())?;
        let values = heap.alloc_array(&[])?;
        let root = heap.add_root(owner, RootKind::Host, values)?;
        Ok(Self {
            document,
            root,
            cursor: 0,
            offset: 0,
            sealing: false,
            _charge: charge,
        })
    }
    fn value(heap: &Heap, values: Value, atom: Atom) -> Result<Value, AdapterError> {
        Ok(match atom {
            Atom::Null => Value::NULL,
            Atom::Bool(v) => Value::bool(v),
            Atom::Int(v) => Value::int(v),
            Atom::Float(v) => Value::float(f64::from_bits(v)),
            Atom::Object(id) => heap.sequence_get(values, id)?,
        })
    }
    pub fn step(&mut self, heap: &mut Heap, work: u32, chunk: usize) -> Result<bool, AdapterError> {
        let values = heap.root_value(self.root)?;
        for _ in 0..work {
            let len = heap.sequence_len(values)? as usize;
            if len < self.document.objects.len() {
                let value = match self.document.objects[len] {
                    Object::String(_) | Object::Bytes(_) => heap.alloc_byte_buffer(&[])?,
                    Object::Array(_) | Object::Tuple(_) => heap.alloc_array(&[])?,
                    Object::Table(_) => heap.alloc_table()?,
                };
                heap.array_push(values, value)?;
                continue;
            }
            if self.cursor == len {
                if self.sealing {
                    return Ok(true);
                }
                self.sealing = true;
                self.cursor = 0;
                continue;
            }
            let value = heap.sequence_get(values, self.cursor as u32)?;
            let object = &self.document.objects[self.cursor];
            if self.sealing {
                match object {
                    Object::String(_) => heap.seal_string(value)?,
                    Object::Bytes(_) => heap.seal_bytes(value)?,
                    Object::Tuple(_) => heap.seal_tuple(value)?,
                    _ => {}
                }
                self.cursor += 1;
                continue;
            }
            if self.offset == object.len() {
                self.cursor += 1;
                self.offset = 0;
                continue;
            }
            match object {
                Object::String(bytes) | Object::Bytes(bytes) => {
                    let end = bytes.len().min(self.offset + chunk.min(4096));
                    heap.bytes_append(value, &bytes[self.offset..end])?;
                    self.offset = end;
                }
                Object::Array(entries) | Object::Tuple(entries) => {
                    heap.array_push(value, Self::value(heap, values, entries[self.offset])?)?;
                    self.offset += 1;
                }
                Object::Table(entries) => {
                    let (key, entry) = entries[self.offset];
                    heap.table_store(
                        value,
                        self.offset as u32,
                        Self::value(heap, values, key)?,
                        Self::value(heap, values, entry)?,
                    )?;
                    self.offset += 1;
                }
            }
        }
        Ok(false)
    }
    pub fn value_root(&self, heap: &mut Heap, owner: RootOwner) -> Result<RootToken, AdapterError> {
        let value = Self::value(heap, heap.root_value(self.root)?, self.document.root)?;
        Ok(heap.add_root(owner, RootKind::Host, value)?)
    }
    pub fn release(self, heap: &mut Heap) -> Result<(), AdapterError> {
        heap.release_root(self.root)?;
        Ok(())
    }
}
