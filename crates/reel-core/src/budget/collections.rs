//! Capacity admission for the workspace's pinned hashbrown tables.
use super::{BudgetPool, BudgetTicket};
use crate::result::{RejectCode, RejectReason};
use hashbrown::HashTable;

/// Reserve table growth while charging both old and new allocations.
/// The retained ticket records hashbrown's actual allocation size. A rejected
/// reservation leaves the table and its existing ticket unchanged.
pub fn reserve_table<T>(
    table: &mut HashTable<T>,
    additional: usize,
    storage: &mut Option<BudgetTicket>,
    pool: &BudgetPool,
    hash: impl Fn(&T) -> u64,
) -> Result<(), RejectReason> {
    if additional <= table.capacity() - table.len() {
        return Ok(());
    }
    let exhausted = || RejectReason::new(RejectCode::OutOfMemory, "hash table capacity");
    // hashbrown 0.17 uses power-of-two buckets and at least 7/8 occupancy
    // beyond its small tables. Twice the requested entries bounds those
    // buckets; the trailing control group and alignment fit this padding.
    let buckets = table
        .len()
        .checked_add(additional)
        .and_then(|n| n.checked_mul(2))
        .and_then(usize::checked_next_power_of_two)
        .ok_or_else(exhausted)?
        .max(4);
    let bytes = buckets
        .checked_mul(size_of::<T>() + 1)
        .and_then(|n| n.checked_add(align_of::<T>().max(16)))
        .ok_or_else(exhausted)?;
    let mut replacement = pool.reserve(bytes as u64)?;
    table
        .try_reserve(additional, hash)
        .map_err(|_| exhausted())?;
    replacement
        .resize(table.allocation_size() as u64)
        .expect("pinned hashbrown allocation fits admitted growth");
    *storage = Some(replacement);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetCategory, QueueLimits};

    #[test]
    fn growth_accounts_actual_storage_and_rejection_preserves_entries() {
        let pool = BudgetPool::new(
            BudgetCategory::Metadata,
            QueueLimits {
                max_requests: 2,
                max_bytes: 1 << 20,
            },
        );
        let mut table = HashTable::new();
        let mut storage = None;
        for value in 0..1024_u64 {
            reserve_table(&mut table, 1, &mut storage, &pool, |v| *v).unwrap();
            table.insert_unique(value, value, |v| *v);
            assert_eq!(pool.usage().bytes, table.allocation_size() as u64);
        }
        let before = pool.usage().bytes;
        assert!(reserve_table(&mut table, 1 << 20, &mut storage, &pool, |v| *v).is_err());
        assert_eq!(pool.usage().bytes, before);
        assert_eq!(table.find(17, |v| *v == 17), Some(&17));
    }
}
