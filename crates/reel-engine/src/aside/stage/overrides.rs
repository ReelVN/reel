//! Scope-owned property layers. Selection changes never write the base property.
use super::*;
use aside_vm::HeapReservation;

const TAG: i64 = -1109;
pub(super) const NODE_SLOT: u32 = 5;
const TARGET: u32 = 1;
const INDICES: u32 = 2;
const REFERENCES: u32 = 3;
const SELECTED: u32 = 0;
const LAYERS: u32 = 1;
const COUNT: usize = NodeProperty::PROPERTIES.len();

pub(in crate::aside) fn is_token(heap: &Heap, value: Value) -> bool {
    heap.host_payload(value, 0)
        .is_ok_and(|v| v.as_int() == Some(TAG))
}

pub(super) fn validate(
    heap: &Heap,
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
    pub(super) fn new(heap: &Heap) -> Result<Box<Self>, AdapterError> {
        let charge = heap
            .budget_usage()
            .reserve((size_of::<Self>() + COUNT * size_of::<u64>()) as u64)?;
        let mut versions = Vec::new();
        versions
            .try_reserve_exact(COUNT)
            .map_err(|_| AdapterError::Allocation)?;
        versions.resize(COUNT, 0);
        Ok(Box::new(Self {
            versions,
            _charge: charge,
        }))
    }
    pub(super) fn changed(&self, heap: &Heap, selected: Value) -> Result<bool, HeapError> {
        for (index, old) in self.versions.iter().enumerate() {
            if *old != version(heap, selected, index as u32)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) fn accept(&mut self, heap: &Heap, selected: Value) -> Result<(), HeapError> {
        for (index, old) in self.versions.iter_mut().enumerate() {
            *old = version(heap, selected, index as u32)?;
        }
        Ok(())
    }
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
                "animation property is not present in this override",
            ));
        }
        Ok(self.property_offset(heap, target)? + index as u32)
    }
    pub(in crate::aside) fn create_override(
        &mut self,
        vm: &mut Vm,
        arguments: [Value; 3],
        remove: FunctionId,
        limit: usize,
    ) -> Result<Value, AdapterError> {
        let [scope, node, properties] = arguments;
        let heap = vm.heap_mut();
        let id = self.id(heap, node)?;
        let count = heap.table_len(properties)?;
        let fields = heap.alloc_table()?;
        let indices = heap.alloc_array(&[])?;
        for field in 0..count {
            let (key, raw) = heap.table_entry(properties, field)?;
            let name = text::read_string(heap, key, limit)?;
            let index = NodeProperty::PROPERTIES
                .iter()
                .position(|p| p.name == name)
                .filter(|&i| i != NodeProperty::OnInteraction.index())
                .ok_or(AdapterError::Input(
                    "override requires common layout or appearance properties",
                ))?;
            let value = heap.property_argument(raw)?;
            NodeProperty::validate(heap, index as u32, value)?;
            heap.table_store(fields, field, key, value)?;
            heap.array_push(indices, Value::int(index as i64))?;
        }
        let references = heap.alloc_array(&[])?;
        let token = heap.alloc_host(
            fields,
            &[Value::int(TAG), node, indices, references],
            Some(validate),
        )?;
        heap.observe_host_properties(token, count, 0)?;
        for field in 0..count {
            let raw = heap.table_entry(properties, field)?.1;
            if heap.is_property_ref(raw) {
                heap.bind_host_property(PropertyId { host: token, field }, raw)?;
            }
            let reference = heap.alloc_property_ref(PropertyId { host: token, field }, false)?;
            heap.array_push(references, reference)?;
        }
        let method = vm.bind_native(token, remove)?;
        let heap = vm.heap_mut();
        let key = self.property_key(heap, "remove")?;
        heap.table_store(fields, count, key, method)?;
        let mut state = heap.host_payload(node, NODE_SLOT)?;
        let mut cache = None;
        if state.tag() == ValueTag::Null {
            let selected = heap.alloc_array(&[Value::NULL; COUNT])?;
            let layers = heap.alloc_array(&[])?;
            state = heap.alloc_array(&[selected, layers])?;
            cache = Some(Cache::new(heap)?);
        }
        let mut owned = heap.host_payload(scope, 3)?;
        if owned.tag() == ValueTag::Null {
            owned = heap.alloc_array(&[])?;
            heap.set_host_payload(scope, 3, owned)?;
        }
        let layers = heap.sequence_get(state, LAYERS)?;
        // All allocation precedes publication. On admission failure the old
        // selection and scope ownership remain intact.
        heap.array_push(owned, token)?;
        if let Err(error) = heap.array_push(layers, token) {
            heap.array_pop(owned)?;
            return Err(error.into());
        }
        let selected = heap.sequence_get(state, SELECTED)?;
        for field in 0..count {
            heap.set_field(
                selected,
                heap.sequence_get(indices, field)?.as_int().unwrap() as u32,
                heap.sequence_get(references, field)?,
            )?;
        }
        heap.set_host_payload(node, NODE_SLOT, state)?;
        let binding = self.handles.get_mut(&id).unwrap();
        if cache.is_some() {
            binding.overrides = cache;
        }
        binding.projection_dirty = true;
        Ok(token)
    }
    pub(in crate::aside) fn remove_override(
        &mut self,
        heap: &mut Heap,
        token: Value,
    ) -> Result<(), AdapterError> {
        if matches!(heap.host_fields(token), Err(HeapError::RetiredHost)) {
            return Ok(());
        }
        if !is_token(heap, token) {
            return Err(AdapterError::Input("expected an override token"));
        }
        let node = heap.host_payload(token, TARGET)?;
        let id = self.id(heap, node)?;
        let state = heap.host_payload(node, NODE_SLOT)?;
        let selected = heap.sequence_get(state, SELECTED)?;
        let layers = heap.sequence_get(state, LAYERS)?;
        let len = heap.sequence_len(layers)?;
        let index = (0..len)
            .rev()
            .find(|&i| heap.sequence_get(layers, i).unwrap().same_bits(token))
            .unwrap();
        for i in index..len - 1 {
            heap.set_field(layers, i, heap.sequence_get(layers, i + 1)?)?;
        }
        heap.array_pop(layers)?;
        let indices = heap.host_payload(token, INDICES)?;
        for field in 0..heap.sequence_len(indices)? {
            let property = heap.sequence_get(indices, field)?.as_int().unwrap();
            let mut reference = Value::NULL;
            'layers: for i in (0..len - 1).rev() {
                let other = heap.sequence_get(layers, i)?;
                let other_indices = heap.host_payload(other, INDICES)?;
                for j in 0..heap.sequence_len(other_indices)? {
                    if heap.sequence_get(other_indices, j)?.as_int() == Some(property) {
                        reference = heap.sequence_get(heap.host_payload(other, REFERENCES)?, j)?;
                        break 'layers;
                    }
                }
            }
            heap.set_field(selected, property as u32, reference)?;
        }
        let binding = self.handles.get_mut(&id).unwrap();
        if len == 1 {
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
        let layers = heap.sequence_get(state, LAYERS)?;
        for index in (0..heap.sequence_len(layers)?).rev() {
            heap.retire_host(heap.sequence_get(layers, index)?)?;
        }
    }
    Ok(())
}
