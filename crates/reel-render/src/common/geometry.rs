//! Completed geometry storage; a slot is never shared with an active submission.
use reel_core::budget::BudgetTicket;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Capacity {
    pub vertices: u64,
    pub indices: u64,
    pub index_format: super::indices::Format,
}
impl Capacity {
    pub fn bytes(self) -> u64 {
        self.vertices + self.indices
    }
    fn fits(self, requested: Self) -> bool {
        self.vertices >= requested.vertices
            && self.indices >= requested.indices
            && self.index_format == requested.index_format
    }
}

pub(crate) struct Allocation<T> {
    pub capacity: Capacity,
    pub storage: T,
    pub ticket: BudgetTicket,
}

/// A fixed handle bound complements the existing byte budget. Unusually large
/// frames can use more allocations, but do not permanently enlarge this cache.
pub(crate) struct Pool<T> {
    slots: [Option<Box<Allocation<T>>>; 64],
    next: usize,
}
impl<T> Default for Pool<T> {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            next: 0,
        }
    }
}
impl<T> Pool<T> {
    pub fn take_matching(
        &mut self,
        requested: Capacity,
        score: impl Fn(&T) -> usize,
    ) -> Option<Box<Allocation<T>>> {
        let index = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                let slot = slot.as_ref()?;
                slot.capacity
                    .fits(requested)
                    .then(|| (index, score(&slot.storage), slot.capacity.bytes()))
            })
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.2.cmp(&a.2)))?
            .0;
        self.slots[index].take()
    }
    pub fn take(&mut self, requested: Capacity) -> Option<Box<Allocation<T>>> {
        let index = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                let slot = slot.as_ref()?;
                slot.capacity
                    .fits(requested)
                    .then_some((index, slot.capacity.bytes()))
            })
            .min_by_key(|(_, bytes)| *bytes)?
            .0;
        self.slots[index].take()
    }
    pub fn put(&mut self, allocation: Box<Allocation<T>>) {
        // Completed allocations track recent demand. Keeping the first 64
        // sizes forever otherwise forces every later, larger frame to allocate.
        let index = (0..self.slots.len())
            .map(|offset| (self.next + offset) % self.slots.len())
            .find(|&index| self.slots[index].is_none())
            .unwrap_or(self.next);
        self.slots[index] = Some(allocation);
        self.next = (index + 1) % self.slots.len();
    }
    pub fn bytes(&self) -> u64 {
        self.slots
            .iter()
            .flatten()
            .map(|slot| slot.ticket.bytes())
            .sum()
    }
    pub fn additional_bytes(&self, bytes: impl Fn(&T) -> u64) -> u64 {
        self.slots
            .iter()
            .flatten()
            .map(|slot| bytes(&slot.storage))
            .sum()
    }
    pub fn clear(&mut self) {
        self.slots.iter_mut().for_each(|slot| *slot = None);
        self.next = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};

    #[test]
    fn growing_geometry_retains_recent_completed_sizes_and_releases_evictions() {
        let budget = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: u32::MAX,
                max_bytes: u64::MAX,
            },
        );
        let mut pool = Pool::default();
        for vertices in 1..=128 {
            let capacity = Capacity {
                vertices,
                indices: 4,
                index_format: super::super::indices::Format::U16,
            };
            pool.put(Box::new(Allocation {
                capacity,
                storage: vertices,
                ticket: budget.reserve(capacity.bytes()).unwrap(),
            }));
        }
        assert_eq!(budget.usage().tickets, 64);
        assert_eq!(budget.usage().bytes, (65..=128).map(|v| v + 4).sum());
        let requested = Capacity {
            vertices: 128,
            indices: 4,
            index_format: super::super::indices::Format::U16,
        };
        for _ in 0..120 {
            let slot = pool
                .take(requested)
                .expect("warm geometry remains reusable");
            assert_eq!(slot.storage, 128);
            pool.put(slot);
        }
        pool.clear();
        assert_eq!(budget.usage().bytes, 0);
    }
}
