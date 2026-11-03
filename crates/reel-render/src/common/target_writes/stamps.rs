//! Sparse frame-local lease observations for completed ordinary effect outputs.
use super::*;

/// Valid only in the TargetWrites preparation that created this observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    slot: usize,
    version: u64,
}
struct Watched {
    slot: usize,
    version: u64,
}
#[derive(Default)]
pub(super) struct Stamps(Vec<Watched>);
impl Stamps {
    pub(super) fn begin(&mut self, slot: usize) {
        if let Ok(index) = self.0.binary_search_by_key(&slot, |watched| watched.slot) {
            self.0[index].version = self.0[index]
                .version
                .checked_add(1)
                .expect("bounded preparation target leases");
        }
    }
    pub(super) fn watch(
        &mut self,
        slot: usize,
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Stamp, GpuError> {
        let index = match self.0.binary_search_by_key(&slot, |watched| watched.slot) {
            Ok(index) => index,
            Err(index) => {
                push_stack(&mut self.0, Watched { slot, version: 0 }, reserve)?;
                self.0[index..].rotate_right(1);
                index
            }
        };
        Ok(Stamp {
            slot,
            version: self.0[index].version,
        })
    }
    pub(super) fn is_current(&self, stamp: Stamp) -> bool {
        self.0
            .binary_search_by_key(&stamp.slot, |watched| watched.slot)
            .is_ok_and(|index| self.0[index].version == stamp.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reuse_invalidates_only_the_observed_physical_slot() {
        let mut writes = TargetWrites::default();
        writes.begin(129, |_| Ok(())).unwrap();
        writes.take(129);
        let original = writes.watch(129, |_| Ok(())).unwrap();
        writes.begin(0, |_| Ok(())).unwrap();
        let other = writes.watch(0, |_| Ok(())).unwrap();
        assert!(writes.is_current(original));
        // Sampling an initialized source and checking initialization do not
        // start another writable lease.
        assert!(!writes.take(129));
        assert!(!writes.pending(129));
        assert!(writes.is_current(original));
        writes.begin(129, |_| Ok(())).unwrap();
        assert!(!writes.is_current(original));
        assert!(writes.is_current(other));
        let next = writes
            .watch(129, |_| panic!("reuse retains the sparse stamp entry"))
            .unwrap();
        assert_ne!(next, original);
        assert!(writes.is_current(next));
    }

    #[test]
    fn refused_optional_stamp_does_not_change_initialization_or_existing_versions() {
        let mut writes = TargetWrites::default();
        writes.begin(0, |_| Ok(())).unwrap();
        assert!(
            writes
                .watch(0, |_| Err(GpuError::Budget {
                    domain: "transient",
                    requested: 16,
                    available: 0
                }))
                .is_err()
        );
        assert!(writes.pending(0));
        let stamp = writes.watch(0, |_| Ok(())).unwrap();
        assert!(writes.is_current(stamp));
        assert!(writes.take(0));
        assert!(writes.is_current(stamp));
    }
}
