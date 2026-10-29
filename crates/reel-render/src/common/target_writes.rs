//! Frame-local initialization of pooled target uses. Allocation and first write
//! are separate events; sampling an unwritten source materializes transparent pixels.
use super::{GpuError, scopes::push_stack};

/// A fullscreen, non-discarding replacement writes every physical sample.
/// Callers choose a shader with unconditional color output and disabled blending.
/// Pooled padding must be written too, even when no logical node occupies it.
#[derive(Clone, Copy)]
pub(crate) struct FullOverwrite(());
impl FullOverwrite {
    pub(crate) fn composite(
        viewport: [u32; 2],
        storage: [u32; 2],
        scissor: super::scissor::Scissor,
        replaces: bool,
    ) -> Option<Self> {
        (replaces
            && !storage.contains(&0)
            && viewport == storage
            && scissor == super::scissor::Scissor::full(storage))
        .then_some(Self(()))
    }
}

#[derive(Default)]
pub(crate) struct TargetWrites {
    pending: Vec<u64>,
}

impl TargetWrites {
    pub(crate) fn begin(
        &mut self,
        slot: usize,
        mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        // Preallocated targets can reach their first use in any slot order.
        while slot / 64 >= self.pending.len() {
            push_stack(&mut self.pending, 0, &mut reserve)?;
        }
        self.pending[slot / 64] |= 1 << (slot % 64);
        Ok(())
    }

    /// Whether this logical target use still needs its first complete initialization.
    pub(crate) fn pending(&self, slot: usize) -> bool {
        self.pending[slot / 64] & (1 << (slot % 64)) != 0
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
    fn preallocated_slots_can_begin_out_of_order_without_initializing_the_gaps() {
        let mut writes = TargetWrites::default();
        writes.begin(129, |_| Ok(())).unwrap();
        assert!(writes.take(129));
        assert!(!writes.take(128));
        assert!(!writes.take(64));
        writes.begin(0, |_| Ok(())).unwrap();
        writes.begin(64, |_| Ok(())).unwrap();
        assert!(writes.take(64));
        assert!(writes.take(0));
        assert!(!writes.take(129));
    }

    #[test]
    fn overwrite_requires_the_whole_physical_surface_and_replacement() {
        use super::super::scissor::Scissor;
        let size = [65, 11];
        let full = Scissor::full(size);
        assert!(FullOverwrite::composite(size, size, full, true).is_some());
        assert!(FullOverwrite::composite(size, size, full, false).is_none());
        assert!(FullOverwrite::composite(size, [128, 16], full, true).is_none());
        assert!(FullOverwrite::composite([0, 0], [0, 0], Scissor::full([0, 0]), true).is_none());
        for scissor in [
            Scissor {
                x: 1,
                width: 64,
                ..full
            },
            Scissor { height: 10, ..full },
            Scissor::full([0, 0]),
        ] {
            assert!(FullOverwrite::composite(size, size, scissor, true).is_none());
        }
    }

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
            assert!(first.pending(slot));
            assert!(first.pending(slot));
            assert!(first.take(slot));
            assert!(!first.pending(slot));
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
