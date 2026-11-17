//! Bounded access for private array builders; normal observable mutations use
//! the existing per-field/model publication APIs.
use super::*;

enum ArrayWrite<'a> {
    Copy { start: u32, values: &'a [Value] },
    AppendRepeated { value: Value, count: u32 },
}

impl Heap {
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
