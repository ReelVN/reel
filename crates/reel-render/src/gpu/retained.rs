//! Native owners of asynchronous retained-result preparation.
use super::{Quality, renderer::Inflight, target_cache::Allocation};
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::bridge::CandidateKey;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(super) struct Output {
    pub allocation: Arc<Allocation>,
    pub target: super::targets::Target,
}

pub(super) struct Pending {
    pub notify_ready: bool,
    pub flight: Inflight,
}
#[derive(Default)]
pub(super) struct PendingQueue {
    pub entries: Vec<Pending>,
    storage: Option<BudgetTicket>,
}
impl PendingQueue {
    pub fn reserve(&mut self, pool: &BudgetPool, limit: u64) -> Result<(), super::GpuError> {
        if self.entries.len() < self.entries.capacity() {
            return Ok(());
        }
        let capacity = self.entries.capacity().max(1) * 2;
        let old = self.entries.capacity() * size_of::<Pending>();
        let bytes = capacity * size_of::<Pending>();
        let pressure = || super::GpuError::Pressure {
            domain: "retained submission",
            requested: bytes as u64,
            available: limit.saturating_sub(pool.usage().bytes),
        };
        if self.storage.is_none() {
            self.storage = Some(pool.reserve(0).map_err(|_| pressure())?);
        }
        self.storage
            .as_mut()
            .unwrap()
            .resize((old + bytes) as u64)
            .map_err(|_| pressure())?;
        self.entries.reserve_exact(capacity - self.entries.len());
        self.storage
            .as_mut()
            .unwrap()
            .resize((self.entries.capacity() * size_of::<Pending>()) as u64)
            .expect("release queue metadata overlap");
        Ok(())
    }
}
#[derive(Default)]
pub(super) struct Cache {
    pub inputs: crate::common::retained::Cache<Output, Quality>,
    pub pending: Mutex<PendingQueue>,
    pub ceiling: Mutex<Option<(CandidateKey, Quality)>>,
    pub epoch: Mutex<Option<reel_core::identity::SurfaceEpoch>>,
}
