//! Native target owners using the common physical-demand and recency policy.
use super::resources::Target;
use crate::common::{target_cache, target_plan::Requirements};
use reel_core::budget::BudgetPool;

#[derive(Default)]
pub(super) struct Cache {
    size: Option<[u32; 2]>,
    pub(super) groups: crate::common::scopes::Cache,
    owners: target_cache::Cache<Target>,
}
impl Cache {
    pub fn output_size(&self) -> Option<[u32; 2]> {
        self.size
    }
    pub fn configure(&mut self, size: [u32; 2]) {
        if self.size != Some(size) {
            self.clear();
            self.size = Some(size);
        }
    }
    pub fn get(
        &self,
        size: [u32; 2],
        storage: super::native::TargetStorage,
        used: &[Target],
    ) -> Option<Target> {
        self.owners
            .get_eligible(size, 0, |target| {
                target.view.target_storage() == Some(storage)
                    && !target.is_effect_pinned()
                    && !used.iter().any(|held| held.same_allocation(target))
            })
            .cloned()
    }
    pub fn insert(&mut self, target: &Target, metadata: &BudgetPool, limit: u64) {
        // Optional cache bookkeeping must not reject a resident target. Its work
        // owner remains valid when no extra cache slot fits the shared budget.
        let _ = self.owners.reserve(1, metadata, limit);
        self.owners.insert(target.view.size(), target.clone());
    }
    pub fn reclaim(&mut self, required: &Requirements, recent: bool) -> bool {
        self.owners.reclaim_eligible(
            required,
            recent,
            |target| !target.is_effect_pinned(),
            Target::cache_only,
        )
    }
    /// A future scratch size is a reuse preference, not a live GPU owner. Its
    /// idle allocation may be rebuilt after a larger physical block is acquired.
    pub fn reclaim_unclaimed(&mut self) -> bool {
        self.reclaim(&Requirements::default(), true)
    }
    pub fn submitted(&mut self, used: &[Target]) {
        self.owners.submitted();
        for target in used {
            self.owners
                .mark_used(target.view.size(), |cached| cached.same_allocation(target));
        }
    }
    pub fn prune_cold(&mut self) {
        self.owners.prune_cold(Target::cache_only);
    }
    pub fn release_idle(&mut self) {
        self.groups.disable();
        self.owners.release_idle(Target::cache_only);
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}
