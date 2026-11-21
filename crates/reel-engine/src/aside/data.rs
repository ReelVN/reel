//! Code-independent values for system records. No VM handles cross the IO lane.
use super::AdapterError;
use aside_vm::typed_data::DataType;
use aside_vm::{
    Heap, HeapHandle, HeapReservation, InstanceField, ObjectKind, RootKind, RootOwner, RootToken,
    Value, ValueTag, bytecode::SymbolId,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
mod bindings;
mod typed;
pub use typed::{
    LogicalReference, LogicalReferences, NoReferences, TypedCapture, TypedDataError, TypedDocument,
    TypedRestore,
};

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
    TypedArray(Box<TypedArray>),
}
#[derive(Debug, Serialize, Deserialize)]
struct TypedArray {
    element: DataType,
    values: Vec<Atom>,
}
impl Object {
    fn bytes(&self) -> usize {
        match self {
            Self::String(v) | Self::Bytes(v) => v.capacity(),
            Self::Array(v) | Self::Tuple(v) => v.capacity() * size_of::<Atom>(),
            Self::TypedArray(array) => {
                size_of::<TypedArray>()
                    + array.element.retained_bytes()
                    + array.values.capacity() * size_of::<Atom>()
            }
            Self::Table(v) => v.capacity() * size_of::<(Atom, Atom)>(),
        }
    }
    fn len(&self) -> usize {
        match self {
            Self::String(v) | Self::Bytes(v) => v.len(),
            Self::Array(v) | Self::Tuple(v) => v.len(),
            Self::TypedArray(array) => array.values.len(),
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
        document.validate()?;
        Ok(document)
    }
    fn validate(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let check = |atom: Atom| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            if let Atom::Object(id) = atom
                && id as usize >= self.objects.len()
            {
                return Err("invalid system data reference".into());
            }
            Ok(())
        };
        check(self.root)?;
        for object in &self.objects {
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
                Object::TypedArray(array) => {
                    for &value in &array.values {
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
        self.payload_bytes = self.objects.iter().map(Object::bytes).sum();
        Ok(())
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
    resource_names: HashMap<std::borrow::Cow<'static, str>, u32>,
    bindings: Vec<usize>,
    binding_cursor: usize,
    cursor: usize,
    validation: usize,
    charge: HeapReservation,
    typed: Option<typed::CaptureState>,
}
impl Capture {
    pub fn new(heap: &Heap, value: Value) -> Result<Self, AdapterError> {
        Self::new_inner(heap, value, None)
    }
    fn new_inner(
        heap: &Heap,
        value: Value,
        context: Option<&typed::Context<'_>>,
    ) -> Result<Self, AdapterError> {
        let mut capture = Self {
            document: DataDocument {
                root: Atom::Null,
                objects: Vec::new(),
                payload_bytes: 0,
            },
            sources: Vec::new(),
            ids: HashMap::new(),
            names: HashMap::new(),
            resource_names: HashMap::new(),
            bindings: Vec::new(),
            binding_cursor: 0,
            cursor: 0,
            validation: 0,
            charge: heap.budget_usage().reserve(size_of::<Self>() as u64)?,
            typed: context.map(|_| typed::CaptureState::default()),
        };
        capture.document.root = capture.atom(heap, value, context)?;
        Ok(capture)
    }
    fn atom(
        &mut self,
        heap: &Heap,
        value: Value,
        context: Option<&typed::Context<'_>>,
    ) -> Result<Atom, AdapterError> {
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
                let reference = if info.kind == ObjectKind::PropertyRef && context.is_some() {
                    Some(LogicalReference {
                        kind: "aside.property".into(),
                        version: 1,
                        fields: 1,
                        payload: Vec::new(),
                    })
                } else {
                    context
                        .and_then(|c| c.references.capture(heap, value))
                        .transpose()
                        .map_err(|e| {
                            if let Some(context) = context {
                                context.error.replace(Some(e));
                            }
                            AdapterError::Input("logical reference capture failed")
                        })?
                };
                let len = if let Some(reference) = &reference {
                    reference.fields
                } else if info.kind == ObjectKind::Instance {
                    heap.instance_field_count(value)?
                } else {
                    info.len
                } as usize;
                let unit = if reference.is_some() {
                    size_of::<(Atom, Atom)>()
                } else {
                    match info.kind {
                        ObjectKind::String | ObjectKind::Bytes => 1,
                        ObjectKind::Array | ObjectKind::Tuple => size_of::<Atom>(),
                        ObjectKind::Table | ObjectKind::Instance => size_of::<(Atom, Atom)>(),
                        _ => {
                            return Err(AdapterError::Input(
                                "persistent data requires data values; convert buffers, executable values and host resources first",
                            ));
                        }
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
                let object = if reference.is_some() {
                    Object::Table(buffer(len)?)
                } else {
                    match info.kind {
                        ObjectKind::String => Object::String(buffer(len)?),
                        ObjectKind::Bytes => Object::Bytes(buffer(len)?),
                        ObjectKind::Array => {
                            let element = heap
                                .array_data_type(value, context.map(|ctx| ctx.registry))
                                .map_err(|error| {
                                    if let Some(context) = context {
                                        context.error.replace(Some(error.into()));
                                    }
                                    AdapterError::Input("array element type is not persistent")
                                })?;
                            if let Some(element) = element {
                                self.charge.resize(
                                    self.charge.bytes()
                                        + (size_of::<TypedArray>() + element.retained_bytes())
                                            as u64,
                                )?;
                                Object::TypedArray(Box::new(TypedArray {
                                    element,
                                    values: buffer(len)?,
                                }))
                            } else {
                                Object::Array(buffer(len)?)
                            }
                        }
                        ObjectKind::Tuple => Object::Tuple(buffer(len)?),
                        ObjectKind::Table | ObjectKind::Instance => Object::Table(buffer(len)?),
                        _ => unreachable!(),
                    }
                };
                let id = u32::try_from(self.document.objects.len())
                    .map_err(|_| AdapterError::Allocation)?;
                let source = Source {
                    value,
                    version: info.content_version,
                    len: len as u32,
                    object: id,
                };
                if let (Some(state), Some(context)) = (&mut self.typed, context) {
                    state
                        .record(
                            heap,
                            context,
                            &source,
                            info.kind,
                            reference,
                            &mut self.charge,
                        )
                        .map_err(|e| {
                            context.error.replace(Some(e.into()));
                            AdapterError::Input("typed State capture contract failed")
                        })?;
                }
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
                self.sources.push(source);
                self.ids.insert(handle, id);
                if info.kind == ObjectKind::Host {
                    self.watch_cell(heap, heap.host_fields(value)?)?;
                }
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
    fn resource_name(
        &mut self,
        name: impl Into<std::borrow::Cow<'static, str>>,
    ) -> Result<Atom, AdapterError> {
        let name = name.into();
        if let Some(&id) = self.resource_names.get(name.as_ref()) {
            return Ok(Atom::Object(id));
        }
        self.charge
            .resize(self.charge.bytes() + (name.len() + 4 * (size_of::<Object>() + 32)) as u64)?;
        let id =
            u32::try_from(self.document.objects.len()).map_err(|_| AdapterError::Allocation)?;
        self.resource_names
            .try_reserve(1)
            .map_err(|_| AdapterError::Allocation)?;
        self.document
            .objects
            .try_reserve(1)
            .map_err(|_| AdapterError::Allocation)?;
        let bytes = name.as_bytes().to_vec();
        self.document.payload_bytes += bytes.capacity();
        self.document.objects.push(Object::String(bytes));
        self.resource_names.insert(name, id);
        Ok(Atom::Object(id))
    }
    pub fn step(
        &mut self,
        heap: &Heap,
        work: u32,
        chunk: usize,
    ) -> Result<(bool, u32), AdapterError> {
        self.step_inner(heap, work, chunk, None)
    }
    fn step_inner(
        &mut self,
        heap: &Heap,
        work: u32,
        chunk: usize,
        context: Option<&typed::Context<'_>>,
    ) -> Result<(bool, u32), AdapterError> {
        for used in 1..=work {
            if self.cursor == self.sources.len() {
                if self.binding_cursor < self.bindings.len() {
                    self.capture_binding(heap, context.unwrap())?;
                    continue;
                }
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
            if heap.info(source.value)?.kind == ObjectKind::PropertyRef {
                self.bindings
                    .try_reserve(1)
                    .map_err(|_| AdapterError::Allocation)?;
                self.charge
                    .resize(self.charge.bytes() + (4 * size_of::<usize>()) as u64)?;
                self.bindings.push(self.cursor);
                self.cursor += 1;
                continue;
            }
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
                    let resource = self
                        .typed
                        .as_ref()
                        .is_some_and(|state| state.has_resource(source.object));
                    let (key, value) = if resource {
                        let context = context
                            .ok_or(AdapterError::Input("logical resource adapter is missing"))?;
                        let (name, field) = context
                            .references
                            .capture_field(heap, value, offset as u32)
                            .map_err(|e| {
                                context.error.replace(Some(e));
                                AdapterError::Input("logical resource field capture failed")
                            })?;
                        (self.resource_name(name)?, field)
                    } else if heap.info(value)?.kind == ObjectKind::Instance {
                        let field = heap.instance_field(value, offset as u32)?;
                        let (key, complete) = self.field_name(&field, chunk)?;
                        if !complete {
                            continue;
                        }
                        self.watch_cell(heap, field.cell)?;
                        if let (Some(state), Some(context)) = (&mut self.typed, context) {
                            state
                                .field(heap, context, value, offset, &mut self.charge)
                                .map_err(|e| {
                                    context.error.replace(Some(e.into()));
                                    AdapterError::Input("typed State field contract failed")
                                })?;
                        }
                        (key, heap.cell_read(field.cell)?)
                    } else {
                        let (key, value) = heap.table_entry(value, offset as u32)?;
                        (self.atom(heap, key, context)?, value)
                    };
                    let value = self.atom(heap, value, context).inspect_err(|e| {
                        let Some(context) = context else {
                            return;
                        };
                        if context.error.borrow().is_some() {
                            return;
                        }
                        let key_name = match key {
                            Atom::Object(id) => match &self.document.objects[id as usize] {
                                Object::String(v) => String::from_utf8_lossy(v).into_owned(),
                                _ => format!("[{offset}]"),
                            },
                            _ => format!("[{offset}]"),
                        };
                        context.error.replace(Some(typed::TypedDataError::Field {
                            path: format!("object[{object}].{key_name}"),
                            reason: e.to_string(),
                        }));
                    })?;
                    let Object::Table(entries) = &mut self.document.objects[object] else {
                        unreachable!()
                    };
                    entries.push((key, value));
                }
                Object::Array(_) | Object::Tuple(_) | Object::TypedArray(_) => {
                    let value =
                        self.atom(heap, heap.sequence_get(value, offset as u32)?, context)?;
                    let values = match &mut self.document.objects[object] {
                        Object::Array(values) | Object::Tuple(values) => values,
                        Object::TypedArray(array) => &mut array.values,
                        _ => unreachable!(),
                    };
                    values.push(value);
                }
            }
        }
        Ok((false, work))
    }
    pub fn finish(self) -> Result<(Arc<DataDocument>, HeapReservation), AdapterError> {
        self.finish_inner(0)
    }
    fn finish_inner(
        self,
        extra: u64,
    ) -> Result<(Arc<DataDocument>, HeapReservation), AdapterError> {
        let Self {
            document,
            sources,
            ids,
            names,
            resource_names,
            mut charge,
            ..
        } = self;
        drop((sources, ids, names, resource_names));
        charge.resize(document.retained_bytes() + (2 * size_of::<usize>()) as u64 + extra)?;
        Ok((Arc::new(document), charge))
    }
}

pub(super) struct Restore {
    document: Arc<DataDocument>,
    root: RootToken,
    cursor: usize,
    offset: usize,
    sealing: bool,
    typed_arrays: Vec<u32>,
    array_cursor: usize,
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
            typed_arrays: Vec::new(),
            array_cursor: 0,
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
        self.step_inner(heap, work, chunk, None)
    }
    fn step_inner(
        &mut self,
        heap: &mut Heap,
        work: u32,
        chunk: usize,
        mut context: Option<(&mut typed::RestoreState, &typed::Context<'_>)>,
    ) -> Result<bool, AdapterError> {
        let values = heap.root_value(self.root)?;
        for _ in 0..work {
            let len = heap.sequence_len(values)? as usize;
            if len < self.document.objects.len() {
                let special = if let Some((state, context)) = context.as_mut() {
                    state.allocate(heap, len as u32, context).map_err(|e| {
                        state.error = Some(e);
                        AdapterError::Input("typed State allocation failed")
                    })?
                } else {
                    None
                };
                let value = if let Some(value) = special {
                    value
                } else {
                    match &self.document.objects[len] {
                        Object::String(_) | Object::Bytes(_) => heap.alloc_byte_buffer(&[])?,
                        Object::Array(_) | Object::Tuple(_) => heap.alloc_array(&[])?,
                        Object::Table(_) => heap.alloc_table()?,
                        Object::TypedArray(array) => {
                            let registry = context.as_ref().map(|(_, ctx)| ctx.registry);
                            let restored = heap.alloc_restored_array(&array.element, registry)?;
                            if self.typed_arrays.len() == self.typed_arrays.capacity() {
                                let growth = self.typed_arrays.capacity().max(4);
                                self._charge.resize(
                                    self._charge.bytes() + (growth * size_of::<u32>()) as u64,
                                )?;
                                self.typed_arrays
                                    .try_reserve_exact(growth)
                                    .map_err(|_| AdapterError::Allocation)?;
                            }
                            self.typed_arrays.push(len as u32);
                            restored
                        }
                    }
                };
                heap.array_push(values, value)?;
                continue;
            }
            if self.cursor == len {
                if self.sealing {
                    if let Some(&id) = self.typed_arrays.get(self.array_cursor) {
                        let array = heap.sequence_get(values, id)?;
                        if self.offset == self.document.objects[id as usize].len() {
                            self.array_cursor += 1;
                            self.offset = 0;
                        } else {
                            heap.validate_restored_array_element(array, self.offset as u32)?;
                            self.offset += 1;
                        }
                        continue;
                    }
                    if let Some((state, context)) = context.as_mut()
                        && !state.complete(heap, values, context).map_err(|e| {
                            state.error = Some(e);
                            AdapterError::Input("typed State finalization failed")
                        })?
                    {
                        continue;
                    }
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
                if let Some((state, _)) = context.as_mut() {
                    state.seal(heap, values, self.cursor as u32).map_err(|e| {
                        state.error = Some(e);
                        AdapterError::Input("typed State sealing failed")
                    })?;
                }
                self.cursor += 1;
                continue;
            }
            if let (Some((state, context)), Object::Table(entries)) = (context.as_mut(), object)
                && let Some(done) = state
                    .fill(heap, values, self.cursor as u32, entries, context)
                    .map_err(|e| {
                        state.error = Some(e);
                        AdapterError::Input("typed State field restoration failed")
                    })?
            {
                if done {
                    self.cursor += 1;
                    self.offset = 0;
                }
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
                Object::TypedArray(array) => {
                    heap.push_restored_array_value(
                        value,
                        Self::value(heap, values, array.values[self.offset])?,
                    )?;
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
        /// Convert closures, buffers and native resources to data first. Incremental capture
        /// produces a consistent data graph; edits that conflict with values already read fail.
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
