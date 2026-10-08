//! Moving tickets, including returned output, keeps resident bytes charged once.
use crate::result::{RejectCode, RejectReason, Rejected};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueLimits {
    pub max_requests: u32,
    pub max_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetCategory {
    CpuInput,
    CpuOutput,
    Temporary,
    Completion,
    GpuResource,
    Offscreen,
    Snapshot,
    VmHeap,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BudgetUsage {
    pub tickets: u32,
    pub bytes: u64,
    pub peak_bytes: u64,
}

#[derive(Debug)]
struct Account {
    category: BudgetCategory,
    limits: QueueLimits,
    usage: BudgetUsage,
}

/// Clones share accounting, never duplicate tickets. Uses no 64-bit atomics.
#[derive(Clone, Debug)]
pub struct BudgetPool(Arc<Mutex<Account>>);

impl BudgetPool {
    pub fn new(category: BudgetCategory, limits: QueueLimits) -> Self {
        Self(Arc::new(Mutex::new(Account {
            category,
            limits,
            usage: BudgetUsage::default(),
        })))
    }

    pub fn usage(&self) -> BudgetUsage {
        self.0.lock().expect("budget account poisoned").usage
    }

    pub fn reserve(&self, bytes: u64) -> Result<BudgetTicket, RejectReason> {
        let mut a = self.0.lock().expect("budget account poisoned");
        let next = a
            .usage
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= a.limits.max_bytes);
        if a.usage.tickets >= a.limits.max_requests || next.is_none() {
            return Err(RejectReason::new(
                RejectCode::Busy,
                "budget capacity exhausted",
            ));
        }
        a.usage.bytes = next.expect("checked above");
        a.usage.tickets += 1;
        a.usage.peak_bytes = a.usage.peak_bytes.max(a.usage.bytes);
        Ok(BudgetTicket {
            account: self.0.clone(),
            category: a.category,
            bytes,
        })
    }
}

/// A linear reservation. Dropping returns exactly this ticket's charge.
#[derive(Debug)]
pub struct BudgetTicket {
    account: Arc<Mutex<Account>>,
    category: BudgetCategory,
    bytes: u64,
}

impl BudgetTicket {
    pub fn category(&self) -> BudgetCategory {
        self.category
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for BudgetTicket {
    fn drop(&mut self) {
        let mut a = self.account.lock().expect("budget account poisoned");
        a.usage.bytes -= self.bytes;
        a.usage.tickets -= 1;
    }
}

/// Immutable owned bytes. The entire vector capacity, not just length, is charged.
#[derive(Debug)]
pub struct OwnedBytes {
    data: Vec<u8>,
    ticket: BudgetTicket,
}

impl OwnedBytes {
    pub fn new(
        data: Vec<u8>,
        ticket: BudgetTicket,
    ) -> Result<Self, Rejected<(Vec<u8>, BudgetTicket)>> {
        if data.capacity() as u64 > ticket.bytes() {
            return Err(Rejected {
                request: (data, ticket),
                reason: RejectReason::new(
                    RejectCode::InvalidRequest,
                    "byte capacity exceeds ticket",
                ),
            });
        }
        Ok(Self { data, ticket })
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }
    pub fn ticket(&self) -> &BudgetTicket {
        &self.ticket
    }
    pub fn into_parts(self) -> (Vec<u8>, BudgetTicket) {
        (self.data, self.ticket)
    }
}
