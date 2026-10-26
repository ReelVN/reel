//! Immutable native projection of scalar curve keys. Shared across retained views.
use reel_core::{
    budget::{BudgetPool, BudgetTicket},
    result::RejectReason,
};
use std::sync::Arc;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Storage {
    keys: Vec<[f64; 6]>,
    #[serde(skip)]
    ticket: Option<BudgetTicket>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ParticleCurve(Arc<Storage>);
impl PartialEq for ParticleCurve {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.keys == other.0.keys
    }
}
impl ParticleCurve {
    pub fn bytes(count: usize) -> u64 {
        (size_of::<Storage>() + 2 * size_of::<usize>() + count * size_of::<[f64; 6]>()) as u64
    }
    /// The caller reserves storage before allocating keys and resizes to capacity.
    pub fn new(keys: Vec<[f64; 6]>, ticket: BudgetTicket) -> Self {
        assert!(!keys.is_empty());
        Self(Arc::new(Storage {
            keys,
            ticket: Some(ticket),
        }))
    }
    /// Attach restored storage to its destination session before publication.
    pub fn admit(&mut self, pool: &BudgetPool) -> Result<(), RejectReason> {
        if self.0.ticket.is_none() {
            let bytes = Self::bytes(self.0.keys.capacity());
            let ticket = pool.reserve(bytes)?;
            Arc::get_mut(&mut self.0)
                .expect("unpublished restored curve")
                .ticket = Some(ticket);
        }
        Ok(())
    }
    pub fn sample(&self, time: f64) -> f64 {
        let keys = &self.0.keys;
        let after = keys.partition_point(|key| key[0] <= time);
        if after == 0 {
            return keys[0][1];
        }
        let [start, value, mode, a, b, c] = keys[after - 1];
        if after == keys.len() || start == time || mode == 1. {
            return value;
        }
        let next = keys[after];
        let progress = (time - start) / (next[0] - start);
        if mode == 0. {
            value * (1. - progress) + next[1] * progress
        } else {
            ((a * progress + b) * progress + c) * progress + value
        }
    }
}
