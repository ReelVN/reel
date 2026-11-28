//! Shared immutable execution descriptors retain original per-function slot order.
use super::{Descriptor, Error, ErrorKind};
use std::{collections::HashSet, ops::Index, sync::Arc};

/// Descriptor access shared by editable compiler data and admitted execution data.
pub trait DescriptorSequence: Index<usize, Output = Descriptor> {
    /// Number of original descriptor slots.
    fn len(&self) -> usize;
    /// Whether the function has no descriptor slots.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Resolve an original function-local descriptor slot.
    fn get(&self, index: usize) -> Option<&Descriptor>;
    /// Borrow descriptors in original slot order without allocating.
    fn iter(&self) -> impl ExactSizeIterator<Item = &Descriptor>;
    /// Direct function-local storage. Shared payloads are counted per package.
    fn capacity_bytes(&self) -> usize;
    /// Whether nested payloads share ownership across descriptor slots.
    fn shared(&self) -> bool {
        false
    }
}

impl DescriptorSequence for Vec<Descriptor> {
    fn len(&self) -> usize {
        Vec::len(self)
    }
    fn get(&self, index: usize) -> Option<&Descriptor> {
        self.as_slice().get(index)
    }
    fn iter(&self) -> impl ExactSizeIterator<Item = &Descriptor> {
        self.as_slice().iter()
    }
    fn capacity_bytes(&self) -> usize {
        self.capacity() * size_of::<Descriptor>()
    }
}

/// Function-local slots point at package-shared immutable descriptors.
/// Package admission accounts for each unique descriptor and its Arc header once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SharedDescriptors {
    pub(super) entries: Vec<Arc<Descriptor>>,
}

impl SharedDescriptors {
    /// Number of original descriptor slots.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    /// Whether this function has no descriptor slots.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// Borrow descriptors in their original slot order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Descriptor> {
        self.entries.iter().map(AsRef::as_ref)
    }
    /// Resolve an original function-local slot.
    pub fn get(&self, index: usize) -> Option<&Descriptor> {
        self.entries.get(index).map(AsRef::as_ref)
    }
    /// Capacity of this function's reference slots. Shared payloads are counted per package.
    pub fn capacity_bytes(&self) -> usize {
        self.entries.capacity() * size_of::<Arc<Descriptor>>()
    }
    /// Clone into the compiler's independently editable representation.
    pub fn to_vec(&self) -> Vec<Descriptor> {
        self.entries
            .iter()
            .map(|item| item.as_ref().clone())
            .collect()
    }
}

impl Index<usize> for SharedDescriptors {
    type Output = Descriptor;
    fn index(&self, index: usize) -> &Self::Output {
        &self.entries[index]
    }
}
impl DescriptorSequence for SharedDescriptors {
    fn len(&self) -> usize {
        self.entries.len()
    }
    fn get(&self, index: usize) -> Option<&Descriptor> {
        self.entries.get(index).map(AsRef::as_ref)
    }
    fn iter(&self) -> impl ExactSizeIterator<Item = &Descriptor> {
        self.entries.iter().map(AsRef::as_ref)
    }
    fn capacity_bytes(&self) -> usize {
        self.entries.capacity() * size_of::<Arc<Descriptor>>()
    }
    fn shared(&self) -> bool {
        true
    }
}

impl<'a> IntoIterator for &'a SharedDescriptors {
    type Item = &'a Descriptor;
    type IntoIter = std::iter::Map<
        std::slice::Iter<'a, Arc<Descriptor>>,
        fn(&'a Arc<Descriptor>) -> &'a Descriptor,
    >;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter().map(AsRef::as_ref)
    }
}

#[derive(Default)]
pub(super) struct DescriptorInterner {
    entries: HashSet<Arc<Descriptor>>,
}
impl DescriptorInterner {
    pub(super) fn get(&self, value: &Descriptor) -> Option<Arc<Descriptor>> {
        self.entries.get(value).cloned()
    }
    pub(super) fn insert(&mut self, value: Descriptor) -> Result<Arc<Descriptor>, Error> {
        self.entries
            .try_reserve(1)
            .map_err(|_| Error::new(ErrorKind::Allocation, "descriptor interner"))?;
        let value = Arc::new(value);
        self.entries.insert(value.clone());
        Ok(value)
    }
    pub(super) fn scratch_bytes(&self) -> usize {
        if self.entries.capacity() == 0 {
            return 0;
        }
        // HashSet capacity excludes its control bytes and empty buckets.
        let buckets = (self.entries.capacity() + 1).next_power_of_two();
        buckets * (size_of::<Arc<Descriptor>>() + 1) + 16
    }
    pub(super) fn insertion_scratch(&self) -> usize {
        if self.entries.len() < self.entries.capacity() {
            return 0;
        }
        let buckets = if self.entries.capacity() == 0 {
            4
        } else {
            (self.entries.capacity() + 1).next_power_of_two() * 2
        };
        buckets * (size_of::<Arc<Descriptor>>() + 1) + 16
    }
}

pub(super) fn descriptor_bytes(value: &Descriptor) -> usize {
    fn bytes<T>(items: &Vec<T>) -> usize {
        items.capacity() * size_of::<T>()
    }
    let nested = match value {
        Descriptor::Closure { captures, .. } => bytes(captures),
        Descriptor::Call(call) | Descriptor::Spawn { call, .. } => bytes(&call.names),
        Descriptor::Table(items) => bytes(items),
        Descriptor::TableSources(items) => bytes(items),
        Descriptor::Class(class) => {
            bytes(&class.fields)
                + bytes(&class.methods)
                + class
                    .fields
                    .iter()
                    .map(|f| bytes(&f.captures))
                    .sum::<usize>()
                + class
                    .methods
                    .iter()
                    .map(|f| bytes(&f.captures))
                    .sum::<usize>()
        }
        Descriptor::Pattern(pattern) => {
            bytes(&pattern.nodes)
                + pattern
                    .nodes
                    .iter()
                    .map(|node| match node {
                        super::PatternNode::Tuple { items, .. }
                        | super::PatternNode::Array { items, .. } => bytes(items),
                        super::PatternNode::Object { fields, .. } => {
                            bytes(fields) + fields.iter().map(|f| bytes(&f.captures)).sum::<usize>()
                        }
                        _ => 0,
                    })
                    .sum::<usize>()
        }
        _ => 0,
    };
    size_of::<Descriptor>() + 2 * size_of::<usize>() + nested
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{self as bc, DescriptorSequence, InstructionSequence};

    fn repeated() -> bc::BytecodePackage {
        let mut package = bc::fixtures::helper_native();
        let call = package.functions[0].descriptors[0].clone();
        package.functions[0]
            .descriptors
            .extend(std::iter::repeat_n(call, 4000));
        for function in &mut package.functions {
            function
                .descriptors
                .extend(std::iter::repeat_n(Descriptor::Table(vec![]), 4000));
            function.source_map.clear();
        }
        package.sources.clear();
        package
    }

    #[test]
    fn shared_descriptors_preserve_wire_slots_and_editable_clones() {
        let source = repeated();
        let limits = bc::Limits::default();
        let wire = bc::encode(&source, limits).unwrap();
        let mut decoded = None;
        let allocation = allocation_counter::measure(|| {
            decoded = Some(bc::decode(&wire, limits).unwrap());
        });
        let decoded = decoded.unwrap();
        let capacity = super::super::codec::measure(&decoded, limits).unwrap().1;
        let hot = decoded.functions[0]
            .code
            .runtime_store()
            .unwrap()
            .cache_stats()
            .resident_capacity_bytes;
        assert!(capacity + hot >= allocation.bytes_current as usize);
        assert!(
            allocation.bytes_max < 256 << 10,
            "decode peak {}",
            allocation.bytes_max
        );
        assert_eq!(bc::encode(&decoded, limits).unwrap(), wire);
        let original = &decoded.functions[0].descriptors[0];
        assert!(std::ptr::eq(
            original,
            &decoded.functions[0].descriptors[4000]
        ));
        assert!(std::ptr::eq(
            &decoded.functions[0].descriptors[4001],
            &decoded.functions[1].descriptors[1]
        ));
        let shared: usize = decoded
            .functions
            .iter()
            .map(|f| f.descriptors.capacity_bytes())
            .sum();
        let editable: usize = source
            .functions
            .iter()
            .map(|f| f.descriptors.capacity_bytes())
            .sum();
        assert!(
            shared * 4 < editable,
            "shared {shared}, editable {editable}"
        );
        let verified = bc::verify(decoded, limits).unwrap();
        let copy = verified.to_unpacked().unwrap();
        for (before, after) in source.functions.iter().zip(copy.functions) {
            assert_eq!(before.descriptors, after.descriptors);
        }
        let direct = bc::verify(source, limits).unwrap();
        assert_eq!(bc::encode(direct.package(), limits).unwrap(), wire);
    }

    #[test]
    fn descriptor_scan_admits_shared_capacity_and_still_checks_indices() {
        let mut source = repeated();
        let wire = bc::encode(&source, bc::Limits::default()).unwrap();
        let limits = bc::Limits {
            max_allocation_bytes: 256 << 10,
            ..bc::Limits::default()
        };
        let decoded = bc::decode(&wire, limits).unwrap();
        bc::verify(decoded, limits).unwrap();

        // Identical malformed descriptors must still be validated after interning.
        source.functions[0].descriptors[4000] = Descriptor::Call(bc::CallDesc {
            target: bc::CallTarget::Function(bc::FunctionId(1)),
            arguments: bc::Arguments::Window(bc::RegWindow {
                start: bc::Reg(0),
                len: 1,
            }),
            names: vec![
                bc::NamedArg {
                    name: bc::SymbolId(4),
                    offset: 0
                };
                2
            ],
            contract: None,
        });
        let wire = super::super::codec::encode_unverified(&source);
        let decoded = bc::decode(&wire, bc::Limits::default()).unwrap();
        assert!(bc::verify(decoded, bc::Limits::default()).is_err());
    }

    #[test]
    fn replacing_the_first_reference_owner_keeps_shared_payload_accounting() {
        let source = repeated();
        let wire = bc::encode(&source, bc::Limits::default()).unwrap();
        let mut decoded = bc::decode(&wire, bc::Limits::default()).unwrap();
        let table = &decoded.functions[0].descriptors[4001] as *const Descriptor;
        decoded.functions[0].descriptors = decoded.functions[1].descriptors.clone();
        assert_eq!(
            std::ptr::from_ref(&decoded.functions[0].descriptors[1]),
            table
        );
        let bytes = super::super::codec::shared_descriptor_capacity(&decoded, |_| Ok(())).unwrap();
        let expected = descriptor_bytes(&decoded.functions[0].descriptors[0])
            + descriptor_bytes(&decoded.functions[0].descriptors[1]);
        assert_eq!(bytes, expected);
        let stats = bc::verify(decoded, bc::Limits::default()).unwrap().stats();
        assert!(stats.owned_capacity_bytes >= expected);
    }
}
