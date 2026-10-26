//! Completed geometry storage; a slot is never shared with an active submission.
use reel_core::budget::BudgetTicket;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Capacity {
    pub vertices: u64,
    pub indices: u64,
}
impl Capacity {
    pub fn bytes(self) -> u64 {
        self.vertices + self.indices
    }
    fn fits(self, requested: Self) -> bool {
        self.vertices >= requested.vertices && self.indices >= requested.indices
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
}
impl<T> Default for Pool<T> {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
        }
    }
}
impl<T> Pool<T> {
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
        if let Some(slot) = self.slots.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(allocation);
        }
    }
    pub fn bytes(&self) -> u64 {
        self.slots
            .iter()
            .flatten()
            .map(|slot| slot.ticket.bytes())
            .sum()
    }
    pub fn clear(&mut self) {
        self.slots.iter_mut().for_each(|slot| *slot = None);
    }
}
