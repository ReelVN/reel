//! Code-independent values for system records. No VM handles cross the IO lane.
use super::AdapterError;
use aside_vm::{
    Heap, HeapHandle, HeapReservation, InstanceField, ObjectKind, RootKind, RootOwner, RootToken,
    Value, ValueTag, bytecode::SymbolId,
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
    pub fn as_bytes(&self) -> Option<&[u8]> {
        if let Atom::Object(id) = self.root
            && let Object::Bytes(bytes) = &self.objects[id as usize]
        {
            Some(bytes)
        } else {
            None
        }
    }
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let payload_bytes = bytes.capacity();
        Self {
            root: Atom::Object(0),
            objects: vec![Object::Bytes(bytes)],
            payload_bytes,
        }
    }
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
            if let Atom::Object(id) = atom
                && id as usize >= document.objects.len()
            {
                return Err("invalid system data reference".into());
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
    object: u32,
}
pub(super) struct Capture {
    document: DataDocument,
    sources: Vec<Source>,
    ids: HashMap<HeapHandle, u32>,
    names: HashMap<SymbolId, u32>,
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
            names: HashMap::new(),
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
                let len = if info.kind == ObjectKind::Instance {
                    heap.instance_field_count(value)?
                } else {
                    info.len
                } as usize;
                let unit = match info.kind {
                    ObjectKind::String | ObjectKind::Bytes => 1,
                    ObjectKind::Array | ObjectKind::Tuple => size_of::<Atom>(),
                    ObjectKind::Table | ObjectKind::Instance => size_of::<(Atom, Atom)>(),
                    _ => {
                        return Err(AdapterError::Input(
                            "persistent data requires data values; convert buffers, executable values and host resources first",
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
                    ObjectKind::Table | ObjectKind::Instance => Object::Table(buffer(len)?),
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
                    len: len as u32,
                    object: id,
                });
                self.ids.insert(handle, id);
                Atom::Object(id)
            }
        })
    }

    fn field_name(
        &mut self,
        field: &InstanceField<'_>,
        chunk: usize,
    ) -> Result<(Atom, bool), AdapterError> {
        let id = if let Some(&id) = self.names.get(&field.symbol) {
            id
        } else {
            let overhead = 4 * (size_of::<Object>() + size_of::<(SymbolId, u32)>() + 16);
            self.charge
                .resize(self.charge.bytes() + (overhead + field.name.len()) as u64)?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(field.name.len())
                .map_err(|_| AdapterError::Allocation)?;
            self.names
                .try_reserve(1)
                .map_err(|_| AdapterError::Allocation)?;
            self.document
                .objects
                .try_reserve(1)
                .map_err(|_| AdapterError::Allocation)?;
            let id =
                u32::try_from(self.document.objects.len()).map_err(|_| AdapterError::Allocation)?;
            self.document.payload_bytes += bytes.capacity();
            self.document.objects.push(Object::String(bytes));
            self.names.insert(field.symbol, id);
            id
        };
        let Object::String(bytes) = &mut self.document.objects[id as usize] else {
            unreachable!()
        };
        let end = field.name.len().min(bytes.len() + chunk.min(4096));
        bytes.extend_from_slice(&field.name.as_bytes()[bytes.len()..end]);
        Ok((Atom::Object(id), end == field.name.len()))
    }

    fn watch_cell(&mut self, heap: &Heap, cell: Value) -> Result<(), AdapterError> {
        self.charge
            .resize(self.charge.bytes() + (4 * size_of::<Source>()) as u64)?;
        self.sources
            .try_reserve(1)
            .map_err(|_| AdapterError::Allocation)?;
        // The field is already copied. Retain only its mutation check until capture finishes.
        self.sources.push(Source {
            value: cell,
            version: heap.info(cell)?.content_version,
            len: 0,
            object: 0,
        });
        Ok(())
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
            let object = source.object as usize;
            if source.len == 0 || self.document.objects[object].len() == source.len as usize {
                self.cursor += 1;
                continue;
            }
            let offset = self.document.objects[object].len();
            let value = source.value;
            match &mut self.document.objects[object] {
                Object::String(bytes) | Object::Bytes(bytes) => {
                    let mut buffer = [0; 4096];
                    let n =
                        heap.read_bytes(value, offset as u32, &mut buffer[..chunk.min(4096)])?;
                    bytes.extend_from_slice(&buffer[..n]);
                }
                Object::Table(_) => {
                    let (key, value) = if heap.info(value)?.kind == ObjectKind::Instance {
                        let field = heap.instance_field(value, offset as u32)?;
                        let (key, complete) = self.field_name(&field, chunk)?;
                        if !complete {
                            continue;
                        }
                        self.watch_cell(heap, field.cell)?;
                        (key, heap.cell_read(field.cell)?)
                    } else {
                        let (key, value) = heap.table_entry(value, offset as u32)?;
                        (self.atom(heap, key)?, value)
                    };
                    let value = self.atom(heap, value)?;
                    let Object::Table(entries) = &mut self.document.objects[object] else {
                        unreachable!()
                    };
                    entries.push((key, value));
                }
                Object::Array(_) | Object::Tuple(_) => {
                    let value = self.atom(heap, heap.sequence_get(value, offset as u32)?)?;
                    let (Object::Array(values) | Object::Tuple(values)) =
                        &mut self.document.objects[object]
                    else {
                        unreachable!()
                    };
                    values.push(value);
                }
            }
        }
        Ok((false, work))
    }
    pub fn finish(self) -> Result<(Arc<DataDocument>, HeapReservation), AdapterError> {
        let Self {
            document,
            sources,
            ids,
            names,
            mut charge,
            ..
        } = self;
        drop((sources, ids, names));
        charge.resize(document.retained_bytes() + (2 * size_of::<usize>()) as u64)?;
        Ok((Arc::new(document), charge))
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

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Read a persistent record by nonempty key, independently of story save slots.
        /// Returns an awaitable operation; awaiting yields the stored plain value graph
        /// or `default` (null when omitted) when the record is absent. Storage failures throw.
        /// The operation belongs to the session, without a node-owner parameter; a host-claimed
        /// storage request is not cancelled merely because its requesting UI was removed.
        "reel::persistent::read";
        /// Store a detached plain value graph under a nonempty persistent key.
        /// Supported values are null, bool, int, float, strings, Bytes, arrays, tuples and
        /// tables, including shared references and cycles. Completed instances store their
        /// data fields as plain tables, including inherited fields. Static fields, methods
        /// and class identity stay in code. Reads return ordinary records for your constructor.
        /// Convert closures, buffers and native resources to data first. Capture is incremental;
        /// mutating captured source objects before it finishes is rejected.
        /// Returns a session-owned awaitable operation; awaiting yields null after the host
        /// commits the write, or throws on failure. A claimed write survives removal of its UI.
        "reel::persistent::write";
        /// Delete the persistent record identified by a nonempty key.
        /// Returns a session-owned awaitable operation; awaiting yields null on success
        /// or throws on storage failure. It does not delete a story save slot.
        /// Host-claimed storage work is independent of the requesting UI's lifetime.
        "reel::persistent::delete";
    }
}
