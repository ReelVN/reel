//! Versioned State metadata layered over the existing detached data graph.
use super::*;
use aside_vm::typed_data::{DefaultBuild, FieldSchema, Registry, SchemaError, StateSchema};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const VERSION: u32 = 1;
/// Engine-owned identity for a resource reconstructed by an adapter in a candidate heap.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogicalReference {
    pub kind: String,
    pub key: Vec<u8>,
}
/// Adapters translate native values to logical identities and allocate current handles.
/// Returned values remain rooted by the graph before its next work boundary.
pub trait LogicalReferences {
    fn capture(
        &self,
        heap: &Heap,
        value: Value,
    ) -> Option<Result<LogicalReference, TypedDataError>>;
    fn restore(
        &self,
        heap: &mut Heap,
        reference: &LogicalReference,
    ) -> Result<Value, TypedDataError>;
}
/// Reject native values when no logical-resource adapter was provided.
pub struct NoReferences;
impl LogicalReferences for NoReferences {
    fn capture(&self, _: &Heap, _: Value) -> Option<Result<LogicalReference, TypedDataError>> {
        None
    }
    fn restore(&self, _: &mut Heap, _: &LogicalReference) -> Result<Value, TypedDataError> {
        Err(TypedDataError::Contract(
            "logical resource adapter is missing",
        ))
    }
}
#[derive(Debug)]
pub enum TypedDataError {
    Schema(SchemaError),
    Data(AdapterError),
    Contract(&'static str),
}
impl From<SchemaError> for TypedDataError {
    fn from(e: SchemaError) -> Self {
        Self::Schema(e)
    }
}
impl From<AdapterError> for TypedDataError {
    fn from(e: AdapterError) -> Self {
        Self::Data(e)
    }
}
impl From<aside_vm::HeapError> for TypedDataError {
    fn from(e: aside_vm::HeapError) -> Self {
        Self::Schema(e.into())
    }
}
impl std::fmt::Display for TypedDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Schema(e) => write!(f, "{e}"),
            Self::Data(e) => write!(f, "{e}"),
            Self::Contract(e) => f.write_str(e),
        }
    }
}
impl std::error::Error for TypedDataError {}

#[derive(Default, Debug, Serialize, Deserialize)]
struct Metadata {
    schemas: Vec<StateSchema>,
    instances: BTreeMap<u32, u32>,
    observable: Vec<u32>,
    references: BTreeMap<u32, LogicalReference>,
    #[serde(skip)]
    bytes: usize,
}
/// A typed graph with an independent version envelope. The embedded plain graph's
/// encoding remains the same format used by persistent system records.
#[derive(Debug, Serialize, Deserialize)]
pub struct TypedDocument {
    version: u32,
    data: Arc<DataDocument>,
    metadata: Metadata,
    #[serde(skip)]
    validated: bool,
}
impl TypedDocument {
    pub fn write_to(&self, writer: impl std::io::Write) -> Result<(), postcard::Error> {
        postcard::to_io(self, writer).map(|_| ())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (mut value, tail): (Self, _) = postcard::take_from_bytes(bytes)?;
        if value.version != VERSION || !tail.is_empty() {
            return Err("unsupported typed State document".into());
        }
        let count = value.data.objects.len();
        let mut types = BTreeSet::new();
        for schema in &value.metadata.schemas {
            if !types.insert(&schema.key) {
                return Err("saved State repeats a type key".into());
            }
            let mut fields = BTreeSet::new();
            for field in &schema.fields {
                if !fields.insert(&field.key) {
                    return Err("saved State repeats a field key".into());
                }
            }
        }
        for (&id, &schema) in &value.metadata.instances {
            if id as usize >= count
                || schema as usize >= value.metadata.schemas.len()
                || !matches!(value.data.objects[id as usize], Object::Table(_))
            {
                return Err("invalid State object metadata".into());
            }
            if value.data.objects[id as usize].len()
                != value.metadata.schemas[schema as usize].fields.len()
            {
                return Err("State field data does not match its schema".into());
            }
        }
        if value.metadata.observable.windows(2).any(|p| p[0] >= p[1]) {
            return Err("observable object IDs must be unique and ordered".into());
        }
        for &id in &value.metadata.observable {
            if id as usize >= count {
                return Err("invalid observable object metadata".into());
            }
        }
        for &id in value.metadata.references.keys() {
            if id as usize >= count || value.metadata.instances.contains_key(&id) {
                return Err("invalid logical reference metadata".into());
            }
            if !matches!(&value.data.objects[id as usize],Object::Table(v) if v.is_empty()) {
                return Err("logical reference must have an empty data placeholder".into());
            }
        }
        // Reuse graph validation instead of maintaining a second reference decoder.
        Arc::get_mut(&mut value.data).unwrap().validate()?;
        value.metadata.bytes = metadata_bytes(&value.metadata);
        value.validated = true;
        Ok(value)
    }
    /// Return cached retained storage without traversing schemas or field contracts.
    pub fn retained_bytes(&self) -> u64 {
        self.data.retained_bytes() + self.metadata.bytes as u64 + size_of::<Self>() as u64
    }
}
fn schema_bytes(s: &StateSchema) -> usize {
    s.key.module.capacity()
        + s.key.name.capacity()
        + s.fields.capacity() * size_of::<aside_vm::typed_data::FieldSchema>()
        + s.fields
            .iter()
            .map(|f| {
                f.key.owner.module.capacity()
                    + f.key.owner.name.capacity()
                    + f.key.name.capacity()
                    + f.ty.retained_bytes()
            })
            .sum::<usize>()
}
// Include full B-tree node slots and edges, with room for a sparse root.
fn tree_bytes<K, V>(len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (len / 5 + 1) * (128 + 11 * (size_of::<K>() + size_of::<V>()))
    }
}
fn field_bytes(field: &FieldSchema) -> usize {
    field.key.owner.module.capacity()
        + field.key.owner.name.capacity()
        + field.key.name.capacity()
        + field.ty.retained_bytes()
}
fn metadata_bytes(m: &Metadata) -> usize {
    m.schemas.capacity() * size_of::<StateSchema>()
        + m.schemas.iter().map(schema_bytes).sum::<usize>()
        + tree_bytes::<u32, u32>(m.instances.len())
        + m.observable.capacity() * size_of::<u32>()
        + tree_bytes::<u32, LogicalReference>(m.references.len())
        + m.references
            .iter()
            .map(|(_, r)| r.kind.capacity() + r.key.capacity())
            .sum::<usize>()
}
pub(super) struct Context<'a> {
    pub registry: &'a Registry,
    pub references: &'a dyn LogicalReferences,
    pub error: std::cell::RefCell<Option<TypedDataError>>,
}
#[derive(Default)]
pub(super) struct CaptureState {
    metadata: Metadata,
    types: BTreeMap<u32, u32>,
}
impl CaptureState {
    pub fn record(
        &mut self,
        heap: &Heap,
        context: &Context<'_>,
        value: Value,
        kind: ObjectKind,
        id: u32,
        reference: Option<LogicalReference>,
        charge: &mut HeapReservation,
    ) -> Result<(), SchemaError> {
        charge.resize(charge.bytes() + 512)?;
        if kind != ObjectKind::Host && heap.data_is_observable(value)? {
            let before = self.metadata.observable.capacity();
            self.metadata.observable.push(id);
            self.metadata.bytes +=
                (self.metadata.observable.capacity() - before) * size_of::<u32>();
        }
        if kind == ObjectKind::Instance {
            let class = context.registry.class_for_instance(heap, value)?;
            let schema = if let Some(&schema) = self.types.get(&class) {
                schema
            } else {
                let key = &context.registry.schema(class)?.key;
                charge.resize(charge.bytes() + (key.module.len() + key.name.len()) as u64)?;
                let index = self.metadata.schemas.len() as u32;
                let before = self.metadata.schemas.capacity();
                self.metadata.schemas.push(StateSchema {
                    key: key.clone(),
                    fields: Vec::new(),
                });
                let key = &self.metadata.schemas[index as usize].key;
                self.metadata.bytes += (self.metadata.schemas.capacity() - before)
                    * size_of::<StateSchema>()
                    + key.module.capacity()
                    + key.name.capacity();
                self.types.insert(class, index);
                index
            };
            let before = tree_bytes::<u32, u32>(self.metadata.instances.len());
            self.metadata.instances.insert(id, schema);
            self.metadata.bytes += tree_bytes::<u32, u32>(self.metadata.instances.len()) - before;
        }
        if let Some(reference) = reference {
            charge.resize(
                charge.bytes() + (reference.kind.capacity() + reference.key.capacity()) as u64,
            )?;
            let before = tree_bytes::<u32, LogicalReference>(self.metadata.references.len());
            self.metadata.bytes += reference.kind.capacity() + reference.key.capacity();
            self.metadata.references.insert(id, reference);
            self.metadata.bytes +=
                tree_bytes::<u32, LogicalReference>(self.metadata.references.len()) - before;
        }
        Ok(())
    }
    pub fn field(
        &mut self,
        heap: &Heap,
        context: &Context<'_>,
        value: Value,
        index: usize,
        charge: &mut HeapReservation,
    ) -> Result<(), SchemaError> {
        let class = context.registry.class_for_instance(heap, value)?;
        let schema = self.types[&class] as usize;
        if self.metadata.schemas[schema].fields.len() == index {
            let field = &context.registry.schema(class)?.fields[index];
            charge.resize(
                charge.bytes()
                    + 512
                    + (field.key.owner.module.len()
                        + field.key.owner.name.len()
                        + field.key.name.len()
                        + field.ty.retained_bytes()) as u64,
            )?;
            let fields = &mut self.metadata.schemas[schema].fields;
            let before = fields.capacity();
            fields.push(field.clone());
            self.metadata.bytes += (fields.capacity() - before) * size_of::<FieldSchema>()
                + field_bytes(fields.last().unwrap());
        }
        Ok(())
    }
}
/// Frozen-root typed capture. Finish or cancel explicitly releases its source root.
pub struct TypedCapture<'a> {
    inner: Capture,
    context: Context<'a>,
    root: RootToken,
    done: bool,
}
impl<'a> TypedCapture<'a> {
    pub fn new(
        heap: &mut Heap,
        owner: RootOwner,
        value: Value,
        registry: &'a Registry,
        references: &'a dyn LogicalReferences,
    ) -> Result<Self, TypedDataError> {
        let context = Context {
            registry,
            references,
            error: std::cell::RefCell::new(None),
        };
        let root = heap.add_root(owner, RootKind::Host, value)?;
        let inner = match Capture::new_inner(heap, value, Some(&context)) {
            Ok(c) => c,
            Err(e) => {
                heap.release_root(root)?;
                return Err(context
                    .error
                    .borrow_mut()
                    .take()
                    .unwrap_or(TypedDataError::Data(e)));
            }
        };
        Ok(Self {
            inner,
            context,
            root,
            done: false,
        })
    }
    pub fn step(
        &mut self,
        heap: &Heap,
        work: u32,
        chunk: usize,
    ) -> Result<(bool, u32), TypedDataError> {
        let result = self
            .inner
            .step_inner(heap, work, chunk, Some(&self.context))
            .map_err(|e| {
                self.context
                    .error
                    .borrow_mut()
                    .take()
                    .unwrap_or(TypedDataError::Data(e))
            })?;
        self.done = result.0;
        Ok(result)
    }
    pub fn finish(
        mut self,
        heap: &mut Heap,
    ) -> Result<(Arc<TypedDocument>, HeapReservation), TypedDataError> {
        heap.release_root(self.root)?;
        if !self.done {
            return Err(TypedDataError::Contract(
                "typed State capture is incomplete",
            ));
        }
        let metadata = self.inner.typed.take().unwrap().metadata;
        let extra = (metadata.bytes + size_of::<TypedDocument>() + 2 * size_of::<usize>()) as u64;
        let (data, charge) = self.inner.finish_inner(extra)?;
        let document = TypedDocument {
            version: VERSION,
            data,
            metadata,
            validated: true,
        };
        Ok((Arc::new(document), charge))
    }
    pub fn cancel(self, heap: &mut Heap) -> Result<(), TypedDataError> {
        heap.release_root(self.root)?;
        Ok(())
    }
}

struct ClassMapping {
    class: u32,
    fields: Vec<Option<u32>>,
    present: BTreeSet<u32>,
}
fn reserve_slot<T>(
    values: &mut Vec<T>,
    charge: &mut HeapReservation,
) -> Result<(), TypedDataError> {
    if values.len() < values.capacity() {
        return Ok(());
    }
    let capacity = values
        .capacity()
        .checked_mul(2)
        .unwrap_or(usize::MAX)
        .max(4);
    let requested = capacity
        .checked_mul(size_of::<T>())
        .ok_or(TypedDataError::Contract("State mapping capacity overflow"))?;
    let before = charge.bytes();
    charge.resize(
        before
            .checked_add(requested as u64)
            .ok_or(TypedDataError::Contract("State mapping capacity overflow"))?,
    )?;
    let mut data = Vec::new();
    data.try_reserve_exact(capacity)
        .map_err(|_| TypedDataError::Data(AdapterError::Allocation))?;
    let actual = data.capacity() * size_of::<T>();
    charge.resize(before + actual as u64)?;
    data.append(values);
    let old = std::mem::replace(values, data);
    let released = old.capacity() * size_of::<T>();
    drop(old);
    charge.resize(before + actual as u64 - released as u64)?;
    Ok(())
}
pub(super) struct RestoreState<'a> {
    document: Arc<TypedDocument>,
    registry: &'a Registry,
    references: &'a dyn LogicalReferences,
    mappings: Vec<ClassMapping>,
    schema: usize,
    matching_type: bool,
    prepared: bool,
    field: u32,
    old: usize,
    default: Option<DefaultBuild>,
    default_field: u32,
    owner: RootOwner,
    pub error: Option<TypedDataError>,
    connecting: bool,
    next_instance: u32,
    sealing_field: u32,
    filled: bool,
}
impl RestoreState<'_> {
    fn prepare(&mut self, charge: &mut HeapReservation) -> Result<(), TypedDataError> {
        if self.schema == self.document.metadata.schemas.len() {
            self.prepared = true;
            return Ok(());
        }
        let saved = &self.document.metadata.schemas[self.schema];
        if !self.matching_type {
            let class = self.registry.find(&saved.key)?;
            reserve_slot(&mut self.mappings, charge)?;
            self.mappings.push(ClassMapping {
                class,
                fields: Vec::new(),
                present: BTreeSet::new(),
            });
            self.matching_type = true;
            return Ok(());
        }
        let mapping = &mut self.mappings[self.schema];
        if mapping.fields.len() == saved.fields.len() {
            self.schema += 1;
            self.matching_type = false;
            self.prepared = self.schema == self.document.metadata.schemas.len();
            return Ok(());
        }
        let field = &saved.fields[mapping.fields.len()];
        let current = self.registry.match_field(mapping.class, field)?;
        reserve_slot(&mut mapping.fields, charge)?;
        if let Some(index) = current {
            if mapping.present.contains(&index) {
                return Err(SchemaError::Field {
                    key: field.key.clone(),
                    reason: "saved schema repeats a field key",
                }
                .into());
            }
            let before = tree_bytes::<u32, ()>(mapping.present.len());
            let after = tree_bytes::<u32, ()>(mapping.present.len() + 1);
            charge.resize(charge.bytes() + (after - before) as u64)?;
            mapping.present.insert(index);
        }
        mapping.fields.push(current);
        Ok(())
    }
    pub fn allocate(&self, heap: &mut Heap, id: u32) -> Result<Option<Value>, TypedDataError> {
        if let Some(&schema) = self.document.metadata.instances.get(&id) {
            return Ok(Some(
                self.registry
                    .begin_instance(heap, self.mappings[schema as usize].class)?,
            ));
        }
        if let Some(reference) = self.document.metadata.references.get(&id) {
            return Ok(Some(self.references.restore(heap, reference)?));
        }
        Ok(None)
    }
    pub fn fill(
        &mut self,
        heap: &mut Heap,
        values: Value,
        id: u32,
        entries: &[(Atom, Atom)],
    ) -> Result<Option<bool>, TypedDataError> {
        let Some(&schema) = self.document.metadata.instances.get(&id) else {
            return Ok(None);
        };
        let mapping = &self.mappings[schema as usize];
        let class = mapping.class;
        let count = self.registry.schema(class)?.fields.len() as u32;
        let instance = heap.sequence_get(values, id)?;
        if !self.connecting {
            if self.field < count {
                self.registry
                    .allocate_field(heap, class, instance, self.field)?;
                self.field += 1;
                return Ok(Some(false));
            }
            self.field = 0;
            return Ok(Some(true));
        }
        if self.old < entries.len() {
            let index = self.old;
            self.old += 1;
            if let Some(Some(current)) = mapping.fields.get(index) {
                self.registry.initialize_field(
                    heap,
                    class,
                    instance,
                    *current,
                    Restore::value(heap, values, entries[index].1)?,
                )?;
            }
            return Ok(Some(false));
        }
        if self.default_field < count {
            let index = self.default_field;
            if mapping.present.contains(&index) {
                self.default_field += 1;
                return Ok(Some(false));
            }
            let plan = self.registry.default(class, index)?;
            if self.default.is_none() {
                let mut build = plan.begin(heap, self.owner)?;
                if self.document.metadata.observable.binary_search(&id).is_ok() {
                    build.observe_containers();
                }
                self.default = Some(build);
                return Ok(Some(false));
            }
            let build = self.default.as_mut().unwrap();
            if build.step(heap, plan, 1)? {
                self.registry.initialize_field(
                    heap,
                    class,
                    instance,
                    index,
                    build.value(heap, plan)?,
                )?;
                self.default.take().unwrap().release(heap)?;
                self.default_field += 1;
            }
            return Ok(Some(false));
        }
        Ok(Some(true))
    }
    pub fn seal(&self, heap: &mut Heap, values: Value, id: u32) -> Result<(), TypedDataError> {
        let observable = self.document.metadata.observable.binary_search(&id).is_ok();
        let value = heap.sequence_get(values, id)?;
        if !self.document.metadata.instances.contains_key(&id) && observable {
            heap.observe_restored_data(value)?;
        }
        Ok(())
    }
    pub fn complete(&mut self, heap: &mut Heap, values: Value) -> Result<bool, TypedDataError> {
        self.connecting = true;
        let Some((&id, &schema)) = self
            .document
            .metadata
            .instances
            .range(self.next_instance..)
            .next()
        else {
            return Ok(true);
        };
        if !self.filled {
            let document = self.document.clone();
            let Object::Table(entries) = &document.data.objects[id as usize] else {
                unreachable!()
            };
            if self.fill(heap, values, id, entries)? == Some(true) {
                self.filled = true;
            }
            return Ok(false);
        }
        let class = self.mappings[schema as usize].class;
        if self.registry.finish_instance(
            heap,
            class,
            heap.sequence_get(values, id)?,
            self.document.metadata.observable.binary_search(&id).is_ok(),
            &mut self.sealing_field,
            1,
        )? {
            self.next_instance = id + 1;
            self.old = 0;
            self.default_field = 0;
            self.sealing_field = 0;
            self.filled = false;
        }
        Ok(false)
    }
}
/// A candidate graph restore; release or cancel before releasing its class Registry.
pub struct TypedRestore<'a> {
    inner: Restore,
    state: RestoreState<'a>,
    done: bool,
    _charge: HeapReservation,
}
impl<'a> TypedRestore<'a> {
    /// Retain a validated document and create the fixed graph root. Compatibility
    /// lookup and mapping allocation advance only through `step`.
    pub fn new(
        heap: &mut Heap,
        owner: RootOwner,
        document: Arc<TypedDocument>,
        registry: &'a Registry,
        references: &'a dyn LogicalReferences,
    ) -> Result<Self, TypedDataError> {
        if document.version != VERSION || !document.validated {
            return Err(TypedDataError::Contract("unsupported typed State document"));
        }
        let metadata_charge =
            document.retained_bytes() - document.data.retained_bytes() + size_of::<Self>() as u64;
        let charge = heap.budget_usage().reserve(metadata_charge)?;
        let prepared = document.metadata.schemas.is_empty();
        let inner = Restore::new(heap, owner, document.data.clone())?;
        Ok(Self {
            inner,
            done: false,
            _charge: charge,
            state: RestoreState {
                document,
                registry,
                references,
                mappings: Vec::new(),
                schema: 0,
                matching_type: false,
                prepared,
                field: 0,
                old: 0,
                default: None,
                default_field: 0,
                owner,
                error: None,
                connecting: false,
                next_instance: 0,
                sealing_field: 0,
                filled: false,
            },
        })
    }
    /// Advance type, field, graph or default work units. Schema preparation finishes
    /// before instance allocation; each field uses the current stable-key index.
    pub fn step(
        &mut self,
        heap: &mut Heap,
        work: u32,
        chunk: usize,
    ) -> Result<bool, TypedDataError> {
        if self.done {
            return Ok(true);
        }
        let mut remaining = work;
        while remaining > 0 && !self.state.prepared {
            self.state.prepare(&mut self._charge)?;
            remaining -= 1;
        }
        if !self.state.prepared {
            return Ok(false);
        }
        let done = self
            .inner
            .step_inner(heap, remaining, chunk, Some(&mut self.state))
            .map_err(|e| self.state.error.take().unwrap_or(TypedDataError::Data(e)))?;
        self.done = done;
        Ok(done)
    }
    pub fn value_root(
        &self,
        heap: &mut Heap,
        owner: RootOwner,
    ) -> Result<RootToken, TypedDataError> {
        if !self.done {
            return Err(TypedDataError::Contract(
                "typed State restore is incomplete",
            ));
        }
        Ok(self.inner.value_root(heap, owner)?)
    }
    pub fn release(self, heap: &mut Heap) -> Result<(), TypedDataError> {
        let Self {
            inner,
            mut state,
            _charge,
            ..
        } = self;
        if let Some(default) = state.default.take() {
            default.release(heap)?;
        }
        drop(state);
        drop(_charge);
        inner.release(heap)?;
        Ok(())
    }
}
