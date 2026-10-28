//! Local media admission and optional physical-block ownership.
use reel_core::{
    budget::{BudgetPool, BudgetTicket},
    result::RejectReason,
};

/// The local limit is a media quota; only the second ticket charges a physical domain.
pub(super) struct Reservation {
    _local: BudgetTicket,
    _physical: Option<BudgetTicket>,
}
pub(super) fn reserve(
    local: &BudgetPool,
    physical: Option<&BudgetPool>,
    bytes: u64,
) -> Result<Reservation, RejectReason> {
    let local = local.reserve(bytes)?;
    let physical = physical.map(|pool| pool.reserve(bytes)).transpose()?;
    Ok(Reservation {
        _local: local,
        _physical: physical,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_core::budget::{BudgetCategory, BudgetGroup, QueueLimits};
    use std::sync::Arc;
    #[test]
    fn physical_admission_rolls_back_and_tracks_final_alias() {
        let limits = QueueLimits {
            max_requests: 8,
            max_bytes: 8,
        };
        let local = BudgetPool::new(BudgetCategory::CpuOutput, limits);
        let group = BudgetGroup::new(4);
        let physical = group.pool(BudgetCategory::CpuOutput, limits);
        let heap = reserve(&local, None, 2).unwrap();
        assert_eq!(group.usage().bytes, 0);
        let block = Arc::new(reserve(&local, Some(&physical), 4).unwrap());
        assert_eq!(group.usage().bytes, 4);
        let before = local.usage().bytes;
        assert!(reserve(&local, Some(&physical), 1).is_err());
        assert_eq!(local.usage().bytes, before);
        let reader = block.clone();
        drop(block);
        assert_eq!(group.usage().bytes, 4);
        drop(reader);
        assert_eq!(group.usage().bytes, 0);
        drop(heap);
        assert_eq!(local.usage().bytes, 0);
    }
}
