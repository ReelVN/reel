//! Ordered table edges with an incremental, non-owning text-key index.
use super::Values;
use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hasher};

pub(crate) const MIN_INDEX_LEN: u32 = 16;
pub(crate) const KEY_BYTES: usize = 64;
// Covers tree nodes, including sparse internal nodes. The boxed header also
// reserves the first node, which is larger than one entry's amortized charge.
pub(crate) const INDEX_ENTRY_BYTES: u64 = 128;
pub(crate) const INDEX_BASE_BYTES: u64 = size_of::<TextIndex>() as u64 + 512;

pub(crate) fn text_hash(string: bool, bytes: &[u8]) -> u64 {
    let mut hash = DefaultHasher::new();
    hash.write_u8(u8::from(string));
    hash.write(bytes);
    hash.finish()
}

pub(crate) fn search_work(len: u32) -> u64 {
    12 * u64::from(len.max(1).ilog2() / 2 + 1)
}

#[derive(Default)]
pub(crate) struct Table {
    pub values: Values,
    pub index: Option<Box<TextIndex>>,
}

#[derive(Default)]
pub(crate) struct TextIndex {
    pub entries: BTreeSet<(u64, u32)>,
    // Every earlier key has been inspected, including keys outside this index's
    // text domain. Appending extends the unindexed suffix; overwrites retain it.
    pub scanned: u32,
    pub reserved: u64,
}

impl TextIndex {
    pub fn candidate(&self, hash: u64, from: u32) -> u32 {
        self.entries
            .range((hash, from)..=(hash, u32::MAX))
            .next()
            .map_or(self.scanned.max(from), |&(_, position)| position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_reservation_covers_tree_growth_and_sparse_release() {
        for count in [1, 12, 64, 1024, 100_000] {
            let mut index = None;
            let allocation = allocation_counter::measure(|| {
                let tree = index.insert(Box::<TextIndex>::default());
                for position in 0..count {
                    tree.entries
                        .insert((u64::from(position).reverse_bits(), position));
                }
            });
            assert!(
                allocation.bytes_max as u64
                    <= INDEX_BASE_BYTES + u64::from(count) * INDEX_ENTRY_BYTES
            );
            let tree = index.as_mut().unwrap();
            while tree.entries.pop_first().is_some() {}
        }
    }

    #[test]
    fn hash_collisions_visit_each_candidate_in_insertion_order() {
        let index = TextIndex {
            entries: [(5, 1), (5, 4), (7, 2)].into(),
            scanned: 6,
            reserved: 0,
        };
        assert_eq!(index.candidate(5, 0), 1);
        assert_eq!(index.candidate(5, 2), 4);
        assert_eq!(index.candidate(5, 5), 6);
        assert_eq!(index.candidate(8, 0), 6);
    }
}
