use super::*;
use reel_core::budget::{BudgetCategory, BudgetGroup, QueueLimits};
use std::sync::Arc;

struct Owner {
    _storage: BudgetTicket,
}
fn pool(bytes: u64) -> BudgetPool {
    BudgetPool::new(
        BudgetCategory::Offscreen,
        QueueLimits {
            max_requests: u32::MAX,
            max_bytes: bytes,
        },
    )
}
fn requirements(values: &[([u32; 2], u64)]) -> Requirements {
    Requirements::collect(values.iter().copied(), |_| Ok(())).unwrap()
}
fn put(cache: &mut Cache<Arc<Owner>>, size: [u32; 2], bytes: u64, data: &BudgetPool) {
    cache.reserve(1, &pool(u64::MAX), u64::MAX).unwrap();
    cache.insert(
        size,
        Arc::new(Owner {
            _storage: data.reserve(bytes).unwrap(),
        }),
    );
}
fn unique(value: &Arc<Owner>) -> bool {
    Arc::strong_count(value) == 1
}

#[test]
fn alternating_sizes_stay_warm_then_simple_frames_retire_cold_storage() {
    let data = pool(96);
    let mut cache = Cache::default();
    put(&mut cache, [2, 2], 16, &data);
    put(&mut cache, [4, 2], 32, &data);
    for frame in 0..64 {
        let size = if frame % 2 == 0 { [2, 2] } else { [4, 2] };
        let used = cache.get(size, 0).unwrap().clone();
        cache.submitted();
        cache.mark_used(size, |owner| Arc::ptr_eq(owner, &used));
        assert!(!cache.prune_cold(unique));
        assert_eq!(data.usage().bytes, 48);
    }
    for _ in 0..32 {
        let used = cache.get([2, 2], 0).unwrap().clone();
        cache.submitted();
        cache.mark_used([2, 2], |owner| Arc::ptr_eq(owner, &used));
        cache.prune_cold(unique);
    }
    assert_eq!(data.usage().bytes, 16);
    let metadata = cache.storage.as_ref().unwrap().bytes();
    for _ in 0..32 {
        let used = cache.get([2, 2], 0).unwrap().clone();
        cache.submitted();
        cache.mark_used([2, 2], |owner| Arc::ptr_eq(owner, &used));
        assert!(!cache.prune_cold(unique));
        assert_eq!(cache.storage.as_ref().unwrap().bytes(), metadata);
    }
}

#[test]
fn prepared_and_flight_owners_keep_storage_until_their_final_reference() {
    let data = pool(96);
    let mut cache = Cache::default();
    put(&mut cache, [4, 4], 64, &data);
    let prepared = cache.get([4, 4], 0).unwrap().clone();
    let flight = prepared.clone();
    cache.submitted();
    cache.submitted();
    assert!(!cache.prune_cold(unique));
    assert!(!cache.reclaim(&Requirements::default(), true, unique));
    drop(prepared);
    assert!(!cache.prune_cold(unique));
    assert_eq!(data.usage().bytes, 64);
    drop(flight);
    assert!(cache.prune_cold(unique));
    assert_eq!(data.usage().bytes, 0);
}

#[test]
fn shared_pressure_reclaims_optional_storage_and_pins_exact_multiplicity() {
    let group = BudgetGroup::new(128);
    let data = group.pool(
        BudgetCategory::Offscreen,
        QueueLimits {
            max_requests: u32::MAX,
            max_bytes: 128,
        },
    );
    let other = group.pool(
        BudgetCategory::GpuResource,
        QueueLimits {
            max_requests: u32::MAX,
            max_bytes: 128,
        },
    );
    let _images = other.reserve(64).unwrap();
    let mut cache = Cache::default();
    for _ in 0..4 {
        put(&mut cache, [2, 2], 16, &data);
    }
    assert!(data.reserve(32).is_err());
    let required = requirements(&[([2, 2], 2)]);
    assert!(!cache.reclaim(&required, false, unique));
    assert!(cache.reclaim(&required, true, unique));
    assert!(cache.get([2, 2], 2).is_some());
    assert!(cache.reclaim(&required, true, unique));
    assert!(cache.get([2, 2], 1).is_some());
    assert!(cache.get([2, 2], 2).is_none());
    let _next = data.reserve(32).unwrap();
    assert_eq!(group.usage().bytes, 128);
    assert!(!cache.reclaim(&required, true, unique));
    assert!(data.reserve(1).is_err());
}

#[test]
fn metadata_growth_admits_overlap_and_keeps_capacity_after_retirement() {
    let metadata = pool(3 * size_of::<Slot<()>>() as u64);
    let mut cache = Cache::default();
    cache.reserve(1, &metadata, u64::MAX).unwrap();
    cache.insert([1, 1], ());
    cache.reserve(1, &metadata, u64::MAX).unwrap();
    cache.insert([2, 1], ());
    let resident = metadata.usage().bytes;
    assert_eq!(resident, 2 * size_of::<Slot<()>>() as u64);
    assert_eq!(
        metadata.usage().peak_bytes,
        3 * size_of::<Slot<()>>() as u64
    );
    assert!(cache.reserve(1, &metadata, u64::MAX).is_err());
    assert_eq!(metadata.usage().bytes, resident);
    cache.submitted();
    cache.submitted();
    assert!(cache.prune_cold(|_| true));
    assert_eq!(metadata.usage().bytes, resident);
    cache.reserve(2, &metadata, u64::MAX).unwrap();
    assert_eq!(metadata.usage().bytes, resident);
    drop(cache);
    assert_eq!(metadata.usage().bytes, 0);
}

#[test]
fn physical_requirements_merge_equal_storage_groups() {
    let demand = requirements(&[([4, 4], 2), ([2, 2], 1), ([4, 4], 3)]);
    assert_eq!(demand.count([4, 4]), 5);
    assert_eq!(demand.count([2, 2]), 1);
    assert_eq!(demand.count([3, 3]), 0);
}

#[test]
fn optional_metadata_refusal_preserves_the_submission_owner() {
    let data = pool(64);
    let metadata = pool(0);
    let mut cache = Cache::default();
    let submitted = Arc::new(Owner {
        _storage: data.reserve(64).unwrap(),
    });
    assert!(cache.reserve(1, &metadata, 0).is_err());
    assert!(!cache.insert([4, 4], submitted.clone()));
    assert_eq!(data.usage().bytes, 64);
    assert_eq!(Arc::strong_count(&submitted), 1);
    assert_eq!(metadata.usage().bytes, 0);
    drop(submitted);
    assert_eq!(data.usage().bytes, 0);
}

#[test]
fn transient_pressure_releases_metadata_after_external_readers_retire() {
    let data = pool(64);
    let metadata = pool(u64::MAX);
    let mut cache = Cache::default();
    cache.reserve(2, &metadata, u64::MAX).unwrap();
    for _ in 0..2 {
        cache.insert(
            [2, 2],
            Arc::new(Owner {
                _storage: data.reserve(16).unwrap(),
            }),
        );
    }
    let flight = cache.get([2, 2], 0).unwrap().clone();
    let bytes = metadata.usage().bytes;
    cache.release_idle(unique);
    assert_eq!(data.usage().bytes, 16);
    assert_eq!(metadata.usage().bytes, bytes);
    drop(flight);
    cache.release_idle(unique);
    assert_eq!(data.usage().bytes, 0);
    assert_eq!(metadata.usage().bytes, 0);
}

#[test]
fn scratch_lookup_skips_effect_owned_targets_without_changing_identity_order() {
    let metadata = pool(4096);
    let mut cache = Cache::default();
    cache.reserve(4, &metadata, 4096).unwrap();
    for (value, pinned) in [(1, false), (2, true), (3, false), (4, true)] {
        cache.insert([8, 8], (value, pinned));
    }
    assert_eq!(
        cache.get_eligible([8, 8], 0, |entry| !entry.1),
        Some(&(1, false))
    );
    assert_eq!(
        cache.get_eligible([8, 8], 1, |entry| !entry.1),
        Some(&(3, false))
    );
    assert_eq!(cache.get_eligible([8, 8], 2, |entry| !entry.1), None);
}

#[test]
fn effect_owned_slots_do_not_count_as_protected_scratch() {
    let metadata = pool(4096);
    let mut cache = Cache::default();
    cache.reserve(3, &metadata, 4096).unwrap();
    for (value, pinned) in [(1, true), (2, false), (3, false)] {
        cache.insert([8, 8], (value, pinned));
    }
    let demand = requirements(&[([8, 8], 1)]);
    assert!(cache.reclaim_eligible(&demand, true, |entry| !entry.1, |_| true));
    assert_eq!(
        cache.get_eligible([8, 8], 0, |entry| !entry.1),
        Some(&(2, false))
    );
    assert!(!cache.reclaim_eligible(&demand, true, |entry| !entry.1, |_| true));
}
