//! Fixed 64-slot accounting chunks use the target's native integer operations.
//! Snapshot encoding visits logical slots and does not expose this storage.

pub(super) type Word = Chunk<{ usize::BITS as usize }, { 64 / usize::BITS as usize }>;

#[derive(Clone, Copy)]
pub(super) struct Chunk<const BITS: usize, const PARTS: usize>([usize; PARTS]);

impl<const BITS: usize, const PARTS: usize> Default for Chunk<BITS, PARTS> {
    fn default() -> Self {
        Self([0; PARTS])
    }
}

impl<const BITS: usize, const PARTS: usize> Chunk<BITS, PARTS> {
    #[inline]
    pub(super) fn contains(&self, bit: usize) -> bool {
        self.0[(bit % 64) / BITS] & (1 << (bit % BITS)) != 0
    }

    #[inline]
    pub(super) fn set(&mut self, bit: usize, initialized: bool) {
        let part = &mut self.0[(bit % 64) / BITS];
        let mask = 1 << (bit % BITS);
        if initialized {
            *part |= mask;
        } else {
            *part &= !mask;
        }
    }

    /// Clear the inclusive logical span within this 64-slot chunk.
    #[inline]
    pub(super) fn clear(&mut self, start: usize, end: usize) {
        let first = start / BITS;
        let last = end / BITS;
        let mask = usize::MAX >> (usize::BITS as usize - BITS);
        let from = mask << (start % BITS);
        let through = mask >> (BITS - 1 - end % BITS);
        if first == last {
            self.0[first] &= !(from & through);
        } else {
            self.0[first] &= !from;
            self.0[first + 1..last].fill(0);
            self.0[last] &= !through;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches_u64<const BITS: usize, const PARTS: usize>() {
        for bits in [0, u64::MAX, 0xaaaa_aaaa_5555_5555, 0x8000_0001_8000_0001] {
            let mut original = Chunk::<BITS, PARTS>::default();
            for bit in 0..64 {
                original.set(bit, bits & (1 << bit) != 0);
            }
            for start in 0..64 {
                for end in start..64 {
                    let mut actual = original;
                    actual.clear(start, end);
                    let expected = bits & !((u64::MAX << start) & (u64::MAX >> (63 - end)));
                    for bit in 0..64 {
                        assert_eq!(actual.contains(bit), expected & (1 << bit) != 0);
                    }
                }
            }
            for bit in 0..64 {
                original.set(bit, false);
                assert!(!original.contains(bit));
                original.set(bit, true);
                assert!(original.contains(bit));
            }
        }
    }

    #[test]
    fn native_chunks_preserve_the_eight_byte_accounting_and_u64_bits() {
        assert_eq!(size_of::<Word>(), size_of::<u64>());
        matches_u64::<{ usize::BITS as usize }, { 64 / usize::BITS as usize }>();
    }

    #[test]
    fn two_32_bit_parts_match_the_u64_oracle_across_every_clear_boundary() {
        // Exercise Vita's split even when these tests run on a 64-bit host.
        matches_u64::<32, 2>();
    }
}
