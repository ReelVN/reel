//! Moving tickets, including returned output, keeps resident bytes charged once.
pub mod collections;
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
    /// Archive index records, paths and their bounded mounting workspace.
    Metadata,
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
struct GroupAccount {
    max_bytes: u64,
    usage: BudgetUsage,
}

/// A shared physical byte limit across independently limited resource domains.
/// Each physical buffer carries one ticket through moves or shared ownership.
/// Distinct storage receives distinct reservations, even on a shared-memory device.
#[derive(Clone, Debug)]
pub struct BudgetGroup(Arc<Mutex<GroupAccount>>);

impl BudgetGroup {
    /// Create an empty group with a byte limit. Request limits belong to its pools.
    pub fn new(max_bytes: u64) -> Self {
        Self(Arc::new(Mutex::new(GroupAccount {
            max_bytes,
            usage: BudgetUsage::default(),
        })))
    }

    /// Create a domain constrained by both these local limits and the group.
    /// Cloning the returned pool shares that domain; each call creates a new domain.
    pub fn pool(&self, category: BudgetCategory, limits: QueueLimits) -> BudgetPool {
        BudgetPool::with_group(category, limits, Some(self.clone()))
    }

    /// Current aggregate reservations and their actual simultaneous byte peak.
    /// Tickets count reservations across all domains, including zero-byte tickets.
    pub fn usage(&self) -> BudgetUsage {
        self.0.lock().expect("budget group poisoned").usage
    }
}

/// Local and optional shared-group accounting captured under the same locks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetSnapshot {
    /// Limits for this pool.
    pub limits: QueueLimits,
    /// Current pool reservations and their simultaneous peak.
    pub usage: BudgetUsage,
    /// Shared group byte limit and current simultaneous usage.
    pub group: Option<(u64, BudgetUsage)>,
}

#[derive(Debug)]
struct Account {
    category: BudgetCategory,
    limits: QueueLimits,
    usage: BudgetUsage,
    group: Option<BudgetGroup>,
}

/// Clones share accounting, never duplicate tickets. Uses no 64-bit atomics.
#[derive(Clone, Debug)]
pub struct BudgetPool(Arc<Mutex<Account>>);

impl BudgetPool {
    pub fn new(category: BudgetCategory, limits: QueueLimits) -> Self {
        Self::with_group(category, limits, None)
    }

    fn with_group(
        category: BudgetCategory,
        limits: QueueLimits,
        group: Option<BudgetGroup>,
    ) -> Self {
        Self(Arc::new(Mutex::new(Account {
            category,
            limits,
            usage: BudgetUsage::default(),
            group,
        })))
    }

    pub fn usage(&self) -> BudgetUsage {
        self.0.lock().expect("budget account poisoned").usage
    }

    /// Capture the local limit, usage and optional group under their accounting locks.
    pub fn snapshot(&self) -> BudgetSnapshot {
        let account = self.0.lock().expect("budget account poisoned");
        let group = account.group.as_ref().map(|group| {
            let group = group.0.lock().expect("budget group poisoned");
            (group.max_bytes, group.usage)
        });
        BudgetSnapshot {
            limits: account.limits,
            usage: account.usage,
            group,
        }
    }

    /// Capacity available for one new ticket at this observation. A shared
    /// group can reduce it further; `reserve` commits the eventual admission.
    pub fn available_bytes(&self) -> u64 {
        let account = self.0.lock().expect("budget account poisoned");
        if account.usage.tickets >= account.limits.max_requests {
            return 0;
        }
        let available = account.limits.max_bytes - account.usage.bytes;
        account.group.as_ref().map_or(available, |group| {
            let total = group.0.lock().expect("budget group poisoned");
            available.min(total.max_bytes - total.usage.bytes)
        })
    }

    pub fn reserve(&self, bytes: u64) -> Result<BudgetTicket, RejectReason> {
        let mut a = self.0.lock().expect("budget account poisoned");
        let category = a.category;
        let Account {
            limits,
            usage,
            group,
            ..
        } = &mut *a;
        let next = usage
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= limits.max_bytes);
        if usage.tickets >= limits.max_requests || next.is_none() {
            return Err(capacity_exhausted());
        }
        // All mutations lock the domain before its group. Group operations never
        // lock domains. Validate both ledgers before publishing either charge.
        let mut total = group
            .as_ref()
            .map(|group| group.0.lock().expect("budget group poisoned"));
        if let Some(total) = &mut total {
            let next = total
                .usage
                .bytes
                .checked_add(bytes)
                .filter(|n| *n <= total.max_bytes)
                .ok_or_else(capacity_exhausted)?;
            let tickets = total
                .usage
                .tickets
                .checked_add(1)
                .ok_or_else(capacity_exhausted)?;
            total.usage.bytes = next;
            total.usage.tickets = tickets;
            total.usage.peak_bytes = total.usage.peak_bytes.max(next);
        }
        usage.bytes = next.expect("checked above");
        usage.tickets += 1;
        usage.peak_bytes = usage.peak_bytes.max(usage.bytes);
        Ok(BudgetTicket {
            account: self.0.clone(),
            category,
            bytes,
        })
    }
}

fn capacity_exhausted() -> RejectReason {
    RejectReason::new(RejectCode::Busy, "budget capacity exhausted")
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

    /// Move this reservation to another local pool while retaining its bytes.
    /// A common group's bytes and peak remain unchanged; destination admission
    /// and the source debit happen together. Rejection preserves the source.
    /// Unrelated groups admit the destination before releasing the source.
    pub fn transfer_to(&mut self, destination: &BudgetPool) -> Result<(), RejectReason> {
        if Arc::ptr_eq(&self.account, &destination.0) {
            return Ok(());
        }
        // All two-account operations lock in address order. Ordinary account
        // operations take at most one account before its group.
        let source_first =
            (Arc::as_ptr(&self.account) as usize) < (Arc::as_ptr(&destination.0) as usize);
        let (first, second) = if source_first {
            (&self.account, &destination.0)
        } else {
            (&destination.0, &self.account)
        };
        let mut first = first.lock().expect("budget account poisoned");
        let mut second = second.lock().expect("budget account poisoned");
        let (source, target) = if source_first {
            (&mut *first, &mut *second)
        } else {
            (&mut *second, &mut *first)
        };
        let common = match (&source.group, &target.group) {
            (Some(a), Some(b)) => Arc::ptr_eq(&a.0, &b.0),
            (None, None) => true,
            _ => false,
        };
        if common {
            let bytes = target
                .usage
                .bytes
                .checked_add(self.bytes)
                .filter(|&bytes| bytes <= target.limits.max_bytes)
                .ok_or_else(capacity_exhausted)?;
            if target.usage.tickets >= target.limits.max_requests {
                return Err(capacity_exhausted());
            }
            target.usage.bytes = bytes;
            target.usage.tickets += 1;
            target.usage.peak_bytes = target.usage.peak_bytes.max(bytes);
            source.usage.bytes -= self.bytes;
            source.usage.tickets -= 1;
            let category = target.category;
            drop(second);
            drop(first);
            self.account = destination.0.clone();
            self.category = category;
            return Ok(());
        }
        drop(second);
        drop(first);
        let replacement = destination.reserve(self.bytes)?;
        *self = replacement;
        Ok(())
    }

    /// Change this reservation without briefly releasing its existing charge.
    /// A rejected growth leaves the ticket, domain and group usage unchanged.
    pub fn resize(&mut self, bytes: u64) -> Result<(), RejectReason> {
        if bytes == self.bytes {
            return Ok(());
        }
        let mut account = self.account.lock().expect("budget account poisoned");
        let Account {
            limits,
            usage,
            group,
            ..
        } = &mut *account;
        let next = (usage.bytes - self.bytes)
            .checked_add(bytes)
            .filter(|next| *next <= limits.max_bytes)
            .ok_or_else(capacity_exhausted)?;
        let mut total = group
            .as_ref()
            .map(|group| group.0.lock().expect("budget group poisoned"));
        if let Some(total) = &mut total {
            let next = (total.usage.bytes - self.bytes)
                .checked_add(bytes)
                .filter(|next| *next <= total.max_bytes)
                .ok_or_else(capacity_exhausted)?;
            total.usage.bytes = next;
            total.usage.peak_bytes = total.usage.peak_bytes.max(next);
        }
        usage.bytes = next;
        usage.peak_bytes = usage.peak_bytes.max(next);
        self.bytes = bytes;
        Ok(())
    }
}

impl Drop for BudgetTicket {
    fn drop(&mut self) {
        let mut a = self.account.lock().expect("budget account poisoned");
        let Account { usage, group, .. } = &mut *a;
        let mut total = group
            .as_ref()
            .map(|group| group.0.lock().expect("budget group poisoned"));
        if let Some(total) = &mut total {
            total.usage.bytes -= self.bytes;
            total.usage.tickets -= 1;
        }
        usage.bytes -= self.bytes;
        usage.tickets -= 1;
    }
}

/// Immutable owned bytes. The entire vector capacity, not just length, is charged.
/// Moving this value or sharing it through `Arc` retains its single reservation
/// until the final owner physically releases the vector.
#[derive(Debug)]
pub struct OwnedBytes {
    data: Vec<u8>,
    range: std::ops::Range<usize>,
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
        let range = 0..data.len();
        Ok(Self {
            data,
            range,
            ticket,
        })
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.data[self.range.clone()]
    }
    /// Full retained allocation capacity, including bytes outside the view.
    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }
    /// Restrict the immutable visible bytes without allocating or moving data.
    /// The range is relative to the current view; the full backing allocation
    /// and its reservation remain owned until the final consumer releases it.
    pub fn into_slice(mut self, range: std::ops::Range<usize>) -> Result<Self, Rejected<Self>> {
        if range.start > range.end || range.end > self.range.len() {
            return Err(Rejected {
                request: self,
                reason: RejectReason::new(
                    RejectCode::InvalidRequest,
                    "byte view exceeds owned bytes",
                ),
            });
        }
        self.range = self.range.start + range.start..self.range.start + range.end;
        Ok(self)
    }
    /// Reclassify this unchanged allocation without copying its bytes.
    /// Rejection leaves the original buffer and reservation intact.
    pub fn transfer_to(&mut self, destination: &BudgetPool) -> Result<(), RejectReason> {
        self.ticket.transfer_to(destination)
    }

    /// Transfer this allocation to an already reserved accounting domain.
    /// Rejection returns both original ownership and the replacement ticket.
    /// The source stays charged until the replacement is validated and installed;
    /// the replacement must cover the complete retained vector capacity.
    pub fn with_ticket(
        mut self,
        ticket: BudgetTicket,
    ) -> Result<Self, Rejected<(Self, BudgetTicket)>> {
        if self.data.capacity() as u64 > ticket.bytes() {
            return Err(Rejected {
                request: (self, ticket),
                reason: RejectReason::new(
                    RejectCode::InvalidRequest,
                    "byte capacity exceeds replacement ticket",
                ),
            });
        }
        self.ticket = ticket;
        Ok(self)
    }
    pub fn ticket(&self) -> &BudgetTicket {
        &self.ticket
    }
    /// Recover a vector containing exactly the visible bytes. A restricted view
    /// moves its bytes to the allocation's front in place, preserving capacity.
    /// Immutable consumers should retain the view to avoid this compaction.
    pub fn into_parts(mut self) -> (Vec<u8>, BudgetTicket) {
        if self.range.start != 0 {
            self.data.copy_within(self.range.clone(), 0);
        }
        self.data.truncate(self.range.len());
        (self.data, self.ticket)
    }
}
