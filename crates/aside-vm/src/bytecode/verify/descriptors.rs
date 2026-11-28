//! Editable descriptors move into shared storage under the existing admission budget.
use super::*;

pub(super) fn compact_descriptors(
    values: Vec<Descriptor>,
    interner: &mut DescriptorInterner,
    budget: &mut Budget,
    retained: &mut usize,
    temporary: usize,
) -> Result<SharedDescriptors, Error> {
    let old_capacity = values.capacity() * size_of::<Descriptor>();
    let mut shared = SharedDescriptors::default();
    let slots = values.len() * size_of::<std::sync::Arc<Descriptor>>();
    budget.admit_capacity(*retained, temporary + interner.scratch_bytes() + slots)?;
    shared
        .entries
        .try_reserve_exact(values.len())
        .map_err(|_| Error::new(ErrorKind::Allocation, "descriptor slots"))?;
    *retained += shared.capacity_bytes();
    for value in values {
        let bytes = super::super::descriptors::descriptor_bytes(&value);
        budget.work(bytes as u64)?;
        if let Some(entry) = interner.get(&value) {
            // Its enum still occupies the original Vec allocation until the end;
            // only the duplicate's nested allocations disappear at this point.
            *retained -= bytes - size_of::<Descriptor>() - 2 * size_of::<usize>();
            drop(value);
            shared.entries.push(entry);
        } else {
            let header = size_of::<Descriptor>() + 2 * size_of::<usize>();
            budget.admit_capacity(
                *retained,
                temporary + interner.scratch_bytes() + interner.insertion_scratch() + header,
            )?;
            // Nested allocations move from the editable value without copying.
            *retained += header;
            shared.entries.push(interner.insert(value)?);
        }
    }
    *retained -= old_capacity;
    Ok(shared)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_admits_slots_and_interner_while_editable_storage_is_live() {
        let slots = 64 * size_of::<std::sync::Arc<Descriptor>>();
        let header = size_of::<Descriptor>() + 2 * size_of::<usize>();
        for reserve_interner in [false, true] {
            let values = vec![Descriptor::Table(Vec::new()); 64];
            let original = values.capacity() * size_of::<Descriptor>();
            let mut interner = DescriptorInterner::default();
            let extra = if reserve_interner {
                header + interner.insertion_scratch()
            } else {
                0
            };
            let mut budget = Budget::new(
                Limits {
                    max_allocation_bytes: original + slots + extra - 1,
                    ..Limits::default()
                },
                original,
            );
            let mut retained = original;
            let result = compact_descriptors(values, &mut interner, &mut budget, &mut retained, 0);
            assert_eq!(result.unwrap_err().kind, ErrorKind::Limit);
            assert_eq!(interner.scratch_bytes(), 0);
        }

        let values = vec![Descriptor::Table(Vec::new()); 64];
        let original = values.capacity() * size_of::<Descriptor>();
        let mut interner = DescriptorInterner::default();
        let mut budget = Budget::new(Limits::default(), original);
        let mut retained = original;
        let shared =
            compact_descriptors(values, &mut interner, &mut budget, &mut retained, 0).unwrap();
        assert!(std::ptr::eq(&shared[0], &shared[63]));
        assert_eq!(retained, slots + header);
        assert!(budget.peak >= slots + header + interner.scratch_bytes());
    }
}
