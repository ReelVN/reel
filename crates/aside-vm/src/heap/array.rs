//! Bounded access for private array builders; normal observable mutations use
//! the existing per-field/model publication APIs.
use super::*;
use crate::typed_data::{DataType, Registry, SchemaError, data_type};

enum ArrayWrite<'a> {
    Copy { start: u32, values: &'a [Value] },
    AppendRepeated { value: Value, count: u32 },
}

impl Heap {
    /// Construct an empty array with an element type from the admitted code.
    pub(crate) fn alloc_typed_array(
        &mut self,
        item: crate::bytecode::TypeId,
    ) -> Result<Value, HeapError> {
        if item.index() >= self.typed_array_base() && self.portable_item(item).is_none() {
            return Err(HeapError::TypeMismatch);
        }
        let array = self.alloc_array(&[])?;
        let key = self.key(array)?;
        let Payload::Values(values) = &mut self.objects.get_mut(key).unwrap().payload else {
            unreachable!()
        };
        values.set_array_item(item);
        self.has_typed_arrays = true;
        Ok(array)
    }

    /// Read the object's element contract without walking its elements.
    pub(crate) fn array_item(
        &self,
        array: Value,
    ) -> Result<Option<crate::bytecode::TypeId>, HeapError> {
        if array.tag() != crate::ValueTag::Object {
            return Ok(None);
        }
        let object = self.object(self.key(array)?)?;
        if let Payload::Values(values) = &object.payload {
            return Ok(values.array_item());
        }
        Ok(None)
    }

    /// Start an unpublished work buffer with the receiver's element contract.
    pub(crate) fn alloc_array_like(&mut self, array: Value) -> Result<Value, HeapError> {
        match self.array_item(array)? {
            Some(item) => self.alloc_typed_array(item),
            None => self.alloc_array(&[]),
        }
    }

    pub(crate) fn array_item_work(&self, array: Value) -> Result<u64, HeapError> {
        if !self.has_typed_arrays {
            return Ok(0);
        }
        Ok(match self.array_item(array)? {
            Some(item) => self.portable_item(item).map_or_else(
                || self.code.as_ref().unwrap().checks.extra(item) + 1,
                |item| item.work,
            ),
            None => 0,
        })
    }

    pub(super) fn check_array_items(
        &self,
        item: Option<crate::bytecode::TypeId>,
        values: &[Value],
    ) -> Result<(), HeapError> {
        if let Some(item) = item {
            let portable = self.portable_item(item);
            for &value in values {
                let accepted = if let Some(portable) = portable {
                    portable.accepts(self, value)?
                } else {
                    let code = self.code.as_ref().unwrap();
                    code.checks.accepts(code.package(), self, item, value)?
                };
                if !accepted {
                    return Err(HeapError::TypeMismatch);
                }
            }
        }
        Ok(())
    }

    /// Capture the object's element type independently of the package's numbering.
    pub fn array_data_type(
        &self,
        array: Value,
        registry: Option<&Registry>,
    ) -> Result<Option<DataType>, SchemaError> {
        let Some(item) = self.array_item(array)? else {
            return Ok(None);
        };
        if let Some(portable) = self.portable_item(item) {
            return Ok(Some(portable.shape.element.clone()));
        }
        let code = self.code.as_ref().unwrap();
        let ty = if let Some(registry) = registry {
            registry.data_type(code.package(), item)?
        } else {
            data_type(code.package(), item)?
        };
        Ok(Some(ty))
    }

    /// Allocate a private graph-restore shell using the current type with this stable contract.
    pub fn alloc_restored_array(
        &mut self,
        element: &DataType,
        registry: Option<&Registry>,
    ) -> Result<Value, SchemaError> {
        let item = self.resolve_array_item(element, registry)?;
        Ok(self.alloc_typed_array(item)?)
    }

    /// Fill a private graph-restore shell before referenced objects are complete.
    /// The embedding restore must validate every element before publishing the graph.
    pub fn push_restored_array_value(
        &mut self,
        array: Value,
        value: Value,
    ) -> Result<(), HeapError> {
        self.push_array_value::<false>(array, value)
    }

    /// Validate one element in a completed but still private restored graph.
    pub fn validate_restored_array_element(
        &self,
        array: Value,
        index: u32,
    ) -> Result<(), HeapError> {
        let item = self.array_item(array)?;
        let value = self.sequence_get(array, index)?;
        self.check_array_items(item, std::slice::from_ref(&value))
    }

    /// Borrow a bounded suffix of one array storage block for the current step.
    /// Never allocates or traverses other blocks. The borrow excludes mutation
    /// and GC; callers retain the array in their ordinary VM roots across steps.
    /// At most max_mutation_values elements are exposed. The suffix at len is empty.
    pub fn array_segment(&self, array: Value, offset: u32) -> Result<&[Value], HeapError> {
        let object = self.object(self.key(array)?)?;
        if object.kind != ObjectKind::Array {
            return Err(HeapError::WrongKind);
        }
        let Payload::Values(values) = &object.payload else {
            unreachable!()
        };
        if offset > values.len {
            return Err(HeapError::OutOfBounds);
        }
        let segment = values.segment(offset);
        Ok(&segment[..segment.len().min(self.limits.max_mutation_values as usize)])
    }
    /// Overwrite/append a contiguous prefix in a private, unobserved plain array.
    /// Returns the actual element count, limited by max_mutation_values; callers
    /// charge that many work units and resume the remaining input themselves.
    /// Bounds, incoming handles and chunk capacity are validated before mutation.
    /// Each copied object receives the normal GC insertion barrier. Actual chunk
    /// growth is charged; one content version is published for the written prefix
    /// and one structure version when it grows. Empty input changes no versions.
    /// Models, property-owned, watched or subscribed arrays return WrongKind;
    /// their public mutations must keep using the ordinary observable APIs.
    pub fn array_write(
        &mut self,
        array: Value,
        start: u32,
        input: &[Value],
    ) -> Result<usize, HeapError> {
        let input = &input[..input.len().min(self.limits.max_mutation_values as usize)];
        self.input_values(input)?;
        self.write_private_array(
            array,
            ArrayWrite::Copy {
                start,
                values: input,
            },
        )
    }

    pub(crate) fn array_append_repeated(
        &mut self,
        array: Value,
        value: Value,
        count: u32,
    ) -> Result<usize, HeapError> {
        let count = count.min(self.limits.max_mutation_values);
        if count != 0 {
            self.validate_value(value)?;
        }
        self.write_private_array(array, ArrayWrite::AppendRepeated { value, count })
    }

    fn write_private_array(
        &mut self,
        array: Value,
        write: ArrayWrite<'_>,
    ) -> Result<usize, HeapError> {
        let key = self.key(array)?;
        let object = self.object(key)?;
        if object.kind != ObjectKind::Array
            || object.model
            || object.watched
            || object.observable_fields != 0
            || object.property_owner.is_some()
            || !object.reactive_subscribers.same_bits(Value::NULL)
        {
            return Err(HeapError::WrongKind);
        }
        let Payload::Values(values) = &object.payload else {
            unreachable!()
        };
        let old = values.len;
        let (start, count, roots) = match &write {
            ArrayWrite::Copy { start, values } => (*start, values.len(), *values),
            ArrayWrite::AppendRepeated { value, count } => {
                (old, *count as usize, std::slice::from_ref(value))
            }
        };
        if start > old {
            return Err(HeapError::OutOfBounds);
        }
        let end = start
            .checked_add(count as u32)
            .ok_or(HeapError::OutOfBounds)?;
        if count == 0 {
            return Ok(0);
        }
        self.check_array_items(values.array_item(), roots)?;
        let extra = Values::growth_bytes(u64::from(old), u64::from(end.saturating_sub(old)));
        let chunks = Values::chunks_for_len(u64::from(end.max(old)))
            - Values::chunks_for_len(u64::from(old));
        self.ensure_bytes(extra)?;
        // The entry points validate the incoming values before reserving storage.
        for &value in roots {
            self.shade(value);
        }
        self.charge(extra, chunks);
        let object = self.objects.get_mut(key).unwrap();
        let Payload::Values(values) = &mut object.payload else {
            unreachable!()
        };
        match write {
            ArrayWrite::Copy { values: input, .. } => {
                let existing = count.min((old - start) as usize);
                values.write_slice(start, &input[..existing]);
                values.extend_from_slice(&input[existing..count]);
            }
            ArrayWrite::AppendRepeated { value, .. } => values.extend_repeated(value, count),
        }
        object.charged += extra;
        object.content_version = object.content_version.wrapping_add(1);
        if end > old {
            object.structure_version = object.structure_version.wrapping_add(1);
        }
        self.mutation_revision = self.mutation_revision.wrapping_add(1);
        Ok(count)
    }
}

#[cfg(test)]
mod tests;
