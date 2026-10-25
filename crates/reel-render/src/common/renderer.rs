use super::{GpuError, GpuLimits};
use reel_core::budget::{BudgetCategory, BudgetPool, BudgetTicket, QueueLimits};
pub(crate) struct Budgets {
    pub(crate) targets: BudgetPool,
    pub(crate) resources: BudgetPool,
    pub(crate) transient: BudgetPool,
}
pub(crate) struct Reservation {
    pub(crate) _shared: Option<BudgetTicket>,
    pub(crate) _local: BudgetTicket,
}
impl Budgets {
    pub(crate) fn new(limits: GpuLimits) -> Self {
        let pool = |category, max_bytes| {
            BudgetPool::new(
                category,
                QueueLimits {
                    max_requests: u32::MAX,
                    max_bytes,
                },
            )
        };
        Self {
            targets: pool(BudgetCategory::Offscreen, limits.target_bytes),
            resources: pool(BudgetCategory::GpuResource, limits.resource_bytes),
            transient: pool(BudgetCategory::Temporary, limits.transient_bytes),
        }
    }
}
pub(crate) fn reserve(
    pool: &BudgetPool,
    local: &BudgetPool,
    bytes: u64,
    limit: u64,
    domain: &'static str,
) -> Result<Reservation, GpuError> {
    // Test the preparation's own live demand first. Other preparations cannot
    // hide a request which already exceeds this preparation's own quota.
    let local = local.reserve(bytes).map_err(|_| GpuError::Budget {
        domain,
        requested: bytes,
        available: limit.saturating_sub(local.usage().bytes),
    })?;
    let shared = pool.reserve(bytes).map_err(|_| GpuError::Pressure {
        domain,
        requested: bytes,
        available: limit.saturating_sub(pool.usage().bytes),
    })?;
    Ok(Reservation {
        _shared: Some(shared),
        _local: local,
    })
}
