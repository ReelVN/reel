use reel_core::budget::BudgetUsage;

/// Renderer-owned residency sampled from its allocation budgets.
/// Cached and in-flight storage remain charged to their original domains.
#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    /// Accounted render-target storage, in bytes.
    pub targets: u64,
    /// Accounted images, glyph atlases and persistent draw resources, in bytes.
    pub resources: u64,
    /// Accounted staging, geometry and temporary storage, in bytes.
    pub transient: u64,
    /// Evictable completed geometry, staging and allocation metadata. This is
    /// a subset of `transient`; in-flight storage is excluded.
    pub cached_geometry: u64,
    /// The shared budget's current usage and actual simultaneous high-water
    /// mark, when the backend constrains these domains through one budget group.
    /// Independent per-domain historical peaks must not be added together.
    pub shared: Option<BudgetUsage>,
}

impl Usage {
    /// Current renderer-owned bytes across the three accounting domains.
    pub fn total(self) -> u64 {
        self.targets + self.resources + self.transient
    }
}
