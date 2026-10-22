//! Scope-owned property layers. Selection changes never write the base property.
use super::*;
use aside_vm::HeapReservation;

const TAG: i64 = -1109;
pub(super) const NODE_SLOT: u32 = 5;
const TARGET: u32 = 1;
const INDICES: u32 = 2;
const REFERENCES: u32 = 3;
const KIND: u32 = 4;
const SELECTED: u32 = 0;
const LAYERS: u32 = 1;
const OFFSETS: u32 = 2;
const COUNT: usize = NodeProperty::PROPERTIES.len();

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Override = 0,
    Offset = 1,
}
enum Properties {
    Override(Value),
    Offset([Value; 2]),
}
impl Properties {
    fn kind(&self) -> Kind {
        match self {
            Self::Override(_) => Kind::Override,
            Self::Offset(_) => Kind::Offset,
        }
    }
    fn count(&self, heap: &Heap) -> Result<u32, HeapError> {
        match self {
            Self::Override(value) => heap.table_len(*value),
            Self::Offset(_) => Ok(2),
        }
    }
    fn raw(&self, heap: &Heap, field: u32) -> Result<Value, HeapError> {
        match self {
            Self::Override(value) => Ok(heap.table_entry(*value, field)?.1),
            Self::Offset(values) => Ok(values[field as usize]),
        }
    }
}

pub(in crate::aside) fn is_token(heap: &Heap, value: Value) -> bool {
    heap.host_payload(value, 0)
        .is_ok_and(|v| v.as_int() == Some(TAG))
}

pub(super) fn validate(
    heap: &aside_vm::PropertyRead<'_>,
    host: Value,
    field: u32,
    value: Value,
) -> Result<(), HeapError> {
    let indices = heap.host_payload(host, INDICES)?;
    if field >= heap.sequence_len(indices)? {
        return Err(HeapError::ReadOnly);
    }
    NodeProperty::validate(
        heap,
        heap.sequence_get(indices, field)?.as_int().unwrap() as u32,
        value,
    )
}

pub(super) struct Cache {
    versions: Vec<u64>,
    _charge: HeapReservation,
}
impl Cache {
    pub(super) fn new(heap: &Heap, offsets: usize) -> Result<Box<Self>, AdapterError> {
        let len = COUNT + offsets;
        let mut charge = heap
            .budget_usage()
            .reserve((size_of::<Self>() + len * size_of::<u64>()) as u64)?;
        let mut versions = Vec::new();
        versions
            .try_reserve_exact(len)
            .map_err(|_| AdapterError::Allocation)?;
        charge.resize((size_of::<Self>() + versions.capacity() * size_of::<u64>()) as u64)?;
        versions.resize(len, 0);
        Ok(Box::new(Self {
            versions,
            _charge: charge,
        }))
    }
    fn reserve_offsets(&mut self, heap: &Heap, offsets: usize) -> Result<(), AdapterError> {
        let len = COUNT + offsets;
        if len > self.versions.capacity() {
            let capacity = len.max(self.versions.capacity() * 2);
            // Keep both storage charges while the replacement is allocated.
            let mut charge = heap
                .budget_usage()
                .reserve((size_of::<Self>() + capacity * size_of::<u64>()) as u64)?;
            let mut versions = Vec::new();
            versions
                .try_reserve_exact(capacity)
                .map_err(|_| AdapterError::Allocation)?;
            charge.resize((size_of::<Self>() + versions.capacity() * size_of::<u64>()) as u64)?;
            versions.extend_from_slice(&self.versions);
            self.versions = versions;
            self._charge = charge;
        }
        self.versions.resize(len, 0);
        Ok(())
    }
    pub(super) fn changed(&self, heap: &Heap, node: Value) -> Result<bool, HeapError> {
        let state = heap.host_payload(node, NODE_SLOT)?;
        let selected = heap.sequence_get(state, SELECTED)?;
        if heap.sequence_len(heap.sequence_get(state, LAYERS)?)? != 0 {
            for (index, old) in self.versions[..COUNT].iter().enumerate() {
                if *old != version(heap, selected, index as u32)? {
                    return Ok(true);
                }
            }
        }
        let offsets = heap.sequence_get(state, OFFSETS)?;
        for index in 0..heap.sequence_len(offsets)? {
            if self.versions[COUNT + index as usize]
                != offset_version(heap, heap.sequence_get(offsets, index)?)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) fn accept(&mut self, heap: &Heap, node: Value) -> Result<(), HeapError> {
        let state = heap.host_payload(node, NODE_SLOT)?;
        let selected = heap.sequence_get(state, SELECTED)?;
        for (index, old) in self.versions[..COUNT].iter_mut().enumerate() {
            *old = version(heap, selected, index as u32)?;
        }
        let offsets = heap.sequence_get(state, OFFSETS)?;
        for index in 0..heap.sequence_len(offsets)? {
            self.versions[COUNT + index as usize] =
                offset_version(heap, heap.sequence_get(offsets, index)?)?;
        }
        Ok(())
    }
}
fn offset_version(heap: &Heap, token: Value) -> Result<u64, HeapError> {
    Ok(heap.info(heap.host_fields(token)?)?.content_version)
}
pub(super) fn offset_count(heap: &Heap, node: Value) -> Result<usize, HeapError> {
    let state = heap.host_payload(node, NODE_SLOT)?;
    if state.tag() == ValueTag::Null {
        Ok(0)
    } else {
        Ok(heap.sequence_len(heap.sequence_get(state, OFFSETS)?)? as usize)
    }
}
pub(super) fn apply_offsets(
    heap: &Heap,
    node: Value,
    props: &mut NodeProps,
) -> Result<(), AdapterError> {
    let state = heap.host_payload(node, NODE_SLOT)?;
    if state.tag() != ValueTag::Null {
        let offsets = heap.sequence_get(state, OFFSETS)?;
        for index in 0..heap.sequence_len(offsets)? {
            let fields = heap.host_fields(heap.sequence_get(offsets, index)?)?;
            props.x += number(heap.table_entry(fields, 0)?.1)?;
            props.y += number(heap.table_entry(fields, 1)?.1)?;
        }
    }
    Ok(())
}
fn version(heap: &Heap, selected: Value, index: u32) -> Result<u64, HeapError> {
    let reference = heap.sequence_get(selected, index)?;
    if reference.tag() == ValueTag::Null {
        return Ok(0);
    }
    let property = heap.property_ref(reference)?.0;
    Ok(heap.info(heap.host_fields(property.host)?)?.content_version)
}

pub(super) fn selection(heap: &Heap, node: Value) -> Result<Value, HeapError> {
    let state = heap.host_payload(node, NODE_SLOT)?;
    if state.tag() == ValueTag::Null {
        Ok(Value::NULL)
    } else {
        heap.sequence_get(state, SELECTED)
    }
}
pub(super) fn get(
    heap: &Heap,
    fields: Value,
    offset: usize,
    selected: Value,
    index: usize,
) -> Result<Value, HeapError> {
    if selected.tag() != ValueTag::Null {
        let reference = heap.sequence_get(selected, index as u32)?;
        if reference.tag() != ValueTag::Null {
            return heap.property_argument(reference);
        }
    }
    Ok(heap.table_entry(fields, (offset + index) as u32)?.1)
}

impl StageBinding {
    pub(in crate::aside) fn effective_property(
        &self,
        heap: &Heap,
        node: Value,
        property: NodeProperty,
    ) -> Result<Value, AdapterError> {
        let offset = self.property_offset(heap, node)? as usize;
        Ok(get(
            heap,
            heap.host_fields(node)?,
            offset,
            selection(heap, node)?,
            property.index(),
        )?)
    }
    pub(in crate::aside) fn animation_target(
        &self,
        heap: &Heap,
        target: Value,
    ) -> Result<(), AdapterError> {
        if is_token(heap, target) {
            heap.host_fields(target)?;
            self.id(heap, heap.host_payload(target, TARGET)?)?;
        } else {
            self.id(heap, target)?;
        }
        Ok(())
    }
    pub(in crate::aside) fn animation_field(
        &self,
        heap: &Heap,
        target: Value,
        index: usize,
    ) -> Result<u32, AdapterError> {
        if is_token(heap, target) {
            let indices = heap.host_payload(target, INDICES)?;
            for field in 0..heap.sequence_len(indices)? {
                if heap.sequence_get(indices, field)?.as_int() == Some(index as i64) {
                    return Ok(field);
                }
            }
            return Err(AdapterError::Input(
                "animation property is not present in this scope layer",
            ));
        }
        Ok(self.property_offset(heap, target)? + index as u32)
    }
    pub(in crate::aside) fn create_override(
        &mut self,
        heap: &mut Heap,
        arguments: [Value; 3],
        limit: usize,
    ) -> Result<Value, AdapterError> {
        let [scope, node, properties] = arguments;
        self.create_layer(heap, scope, node, Properties::Override(properties), limit)
    }
    pub(in crate::aside) fn create_offset(
        &mut self,
        heap: &mut Heap,
        arguments: [Value; 4],
    ) -> Result<Value, AdapterError> {
        let [scope, node, x, y] = arguments;
        self.create_layer(heap, scope, node, Properties::Offset([x, y]), 0)
    }
    fn create_layer(
        &mut self,
        heap: &mut Heap,
        scope: Value,
        node: Value,
        properties: Properties,
        limit: usize,
    ) -> Result<Value, AdapterError> {
        let id = self.id(heap, node)?;
        let kind = properties.kind();
        let count = properties.count(heap)?;
        let fields = heap.alloc_table()?;
        let indices = heap.alloc_array(&[])?;
        for field in 0..count {
            let (key, index) = match properties {
                Properties::Override(properties) => {
                    let key = heap.table_entry(properties, field)?.0;
                    let name = text::read_string(heap, key, limit)?;
                    let index = NodeProperty::PROPERTIES
                        .iter()
                        .position(|p| p.name == name)
                        .filter(|&i| i != NodeProperty::OnInteraction.index())
                        .ok_or(AdapterError::Input(
                            "override requires common layout or appearance properties",
                        ))?;
                    (key, index)
                }
                Properties::Offset(_) => {
                    let index = [NodeProperty::X.index(), NodeProperty::Y.index()][field as usize];
                    (
                        self.property_key(heap, NodeProperty::PROPERTIES[index].name)?,
                        index,
                    )
                }
            };
            let raw = properties.raw(heap, field)?;
            let value = heap.property_argument(raw)?;
            NodeProperty::validate(heap, index as u32, value)?;
            heap.table_store(fields, field, key, value)?;
            heap.array_push(indices, Value::int(index as i64))?;
        }
        let references = if kind == Kind::Override {
            heap.alloc_array(&[])?
        } else {
            Value::NULL
        };
        let token = heap.alloc_host(
            fields,
            &[
                Value::int(TAG),
                node,
                indices,
                references,
                Value::int(kind as i64),
            ],
            Some(validate),
        )?;
        self.types.mark(heap, token, "reel::types::Override")?;
        heap.observe_host_properties(token, count, 0)?;
        for field in 0..count {
            let raw = properties.raw(heap, field)?;
            if heap.is_property_ref(raw) {
                heap.bind_host_property(PropertyId { host: token, field }, raw)?;
            }
            if kind == Kind::Override {
                let reference =
                    heap.alloc_property_ref(PropertyId { host: token, field }, false)?;
                heap.array_push(references, reference)?;
            }
        }
        let mut state = heap.host_payload(node, NODE_SLOT)?;
        let mut cache = None;
        if state.tag() == ValueTag::Null {
            let selected = heap.alloc_array(&[Value::NULL; COUNT])?;
            let layers = heap.alloc_array(&[])?;
            let offsets = heap.alloc_array(&[])?;
            state = heap.alloc_array(&[selected, layers, offsets])?;
            cache = Some(Cache::new(heap, usize::from(kind == Kind::Offset))?);
        } else if kind == Kind::Offset {
            let count = heap.sequence_len(heap.sequence_get(state, OFFSETS)?)? as usize + 1;
            self.handles
                .get_mut(&id)
                .unwrap()
                .overrides
                .as_mut()
                .unwrap()
                .reserve_offsets(heap, count)?;
        }
        let mut owned = heap.host_payload(scope, 3)?;
        if owned.tag() == ValueTag::Null {
            owned = heap.alloc_array(&[])?;
            heap.set_host_payload(scope, 3, owned)?;
        }
        let layers = heap.sequence_get(
            state,
            if kind == Kind::Offset {
                OFFSETS
            } else {
                LAYERS
            },
        )?;
        // All allocation precedes publication. On admission failure the old
        // selection and scope ownership remain intact.
        heap.array_push(owned, token)?;
        if let Err(error) = heap.array_push(layers, token) {
            heap.array_pop(owned)?;
            return Err(error.into());
        }
        if kind == Kind::Override {
            let selected = heap.sequence_get(state, SELECTED)?;
            for field in 0..count {
                heap.set_field(
                    selected,
                    heap.sequence_get(indices, field)?.as_int().unwrap() as u32,
                    heap.sequence_get(references, field)?,
                )?;
            }
        }
        heap.set_host_payload(node, NODE_SLOT, state)?;
        let binding = self.handles.get_mut(&id).unwrap();
        if cache.is_some() {
            binding.overrides = cache;
        }
        binding.projection_dirty = true;
        Ok(token)
    }
    pub(in crate::aside) fn remove_layer(
        &mut self,
        heap: &mut Heap,
        token: Value,
    ) -> Result<(), AdapterError> {
        if matches!(heap.host_fields(token), Err(HeapError::RetiredHost)) {
            return Ok(());
        }
        if !is_token(heap, token) {
            return Err(AdapterError::Input("expected a scope property layer"));
        }
        let node = heap.host_payload(token, TARGET)?;
        let id = self.id(heap, node)?;
        let state = heap.host_payload(node, NODE_SLOT)?;
        let selected = heap.sequence_get(state, SELECTED)?;
        let is_offset = heap.host_payload(token, KIND)?.as_int() == Some(Kind::Offset as i64);
        let layers = heap.sequence_get(state, if is_offset { OFFSETS } else { LAYERS })?;
        let len = heap.sequence_len(layers)?;
        let index = (0..len)
            .rev()
            .find(|&i| heap.sequence_get(layers, i).unwrap().same_bits(token))
            .unwrap();
        for i in index..len - 1 {
            heap.set_field(layers, i, heap.sequence_get(layers, i + 1)?)?;
        }
        heap.array_pop(layers)?;
        if !is_offset {
            let indices = heap.host_payload(token, INDICES)?;
            for field in 0..heap.sequence_len(indices)? {
                let property = heap.sequence_get(indices, field)?.as_int().unwrap();
                let mut reference = Value::NULL;
                'layers: for i in (0..len - 1).rev() {
                    let other = heap.sequence_get(layers, i)?;
                    let other_indices = heap.host_payload(other, INDICES)?;
                    for j in 0..heap.sequence_len(other_indices)? {
                        if heap.sequence_get(other_indices, j)?.as_int() == Some(property) {
                            reference =
                                heap.sequence_get(heap.host_payload(other, REFERENCES)?, j)?;
                            break 'layers;
                        }
                    }
                }
                heap.set_field(selected, property as u32, reference)?;
            }
        }
        let binding = self.handles.get_mut(&id).unwrap();
        if is_offset {
            binding
                .overrides
                .as_mut()
                .unwrap()
                .versions
                .truncate(COUNT + len as usize - 1);
        }
        if heap.sequence_len(heap.sequence_get(state, LAYERS)?)? == 0
            && heap.sequence_len(heap.sequence_get(state, OFFSETS)?)? == 0
        {
            heap.set_host_payload(node, NODE_SLOT, Value::NULL)?;
            binding.overrides = None;
        }
        binding.projection_dirty = true;
        heap.retire_host(token)?;
        Ok(())
    }
}

pub(super) fn retire(heap: &mut Heap, node: Value) -> Result<(), HeapError> {
    let state = heap.host_payload(node, NODE_SLOT)?;
    if state.tag() != ValueTag::Null {
        for slot in [LAYERS, OFFSETS] {
            let layers = heap.sequence_get(state, slot)?;
            for index in (0..heap.sequence_len(layers)?).rev() {
                heap.retire_host(heap.sequence_get(layers, index)?)?;
            }
        }
    }
    Ok(())
}
