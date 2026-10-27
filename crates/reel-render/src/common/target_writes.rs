//! Frame-local initialization of pooled target uses. Allocation and first write
//! are separate events; sampling an unwritten source materializes transparent pixels.
use super::{GpuError, scopes::push_stack};

#[derive(Default)]
pub(crate) struct TargetWrites {
    pending: Vec<u64>,
}

impl TargetWrites {
    pub(crate) fn begin(
        &mut self,
        slot: usize,
        reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        if slot / 64 == self.pending.len() {
            push_stack(&mut self.pending, 0, reserve)?;
        }
        self.pending[slot / 64] |= 1 << (slot % 64);
        Ok(())
    }

    /// The first writer clears; later writers preserve earlier contents.
    pub(crate) fn take(&mut self, slot: usize) -> bool {
        let word = &mut self.pending[slot / 64];
        let bit = 1 << (slot % 64);
        let clear = *word & bit != 0;
        *word &= !bit;
        clear
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooled_uses_initialize_once_and_preparations_keep_separate_state() {
        let mut first = TargetWrites::default();
        let mut second = TargetWrites::default();
        let mut reserved = 0;
        for slot in 0..130 {
            first
                .begin(slot, |bytes| {
                    reserved += bytes;
                    Ok(())
                })
                .unwrap();
            second.begin(slot, |_| Ok(())).unwrap();
        }
        assert!(reserved <= 64);
        for slot in (0..130).rev() {
            assert!(first.take(slot));
            assert!(!first.take(slot));
            assert!(second.take(slot));
            first
                .begin(slot, |_| panic!("same-frame reuse retains the bitset"))
                .unwrap();
            assert!(!second.take(slot));
            assert!(first.take(slot));
        }
    }
}
