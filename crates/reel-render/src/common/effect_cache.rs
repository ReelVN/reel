//! Device-owned shader identity cache, with explicit producer and metadata lives.
use super::GpuError;
use hashbrown::HashTable;
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::resources::{ResourceHandle, ResourceLease, ResourceWeak};
use std::hash::{BuildHasher, DefaultHasher};

fn hash(key: &[u8; 32]) -> u64 {
    std::hash::BuildHasherDefault::<DefaultHasher>::default().hash_one(key)
}

struct Entry<T> {
    key: [u8; 32],
    owners: Vec<(ResourceHandle, ResourceWeak)>,
    compiled: T,
    storage: BudgetTicket,
}
pub(crate) struct Cache<T> {
    entries: HashTable<Entry<T>>,
    storage: Option<BudgetTicket>,
}
impl<T> Default for Cache<T> {
    fn default() -> Self {
        Self {
            entries: HashTable::new(),
            storage: None,
        }
    }
}
pub(crate) fn reserve(
    pool: &BudgetPool,
    bytes: usize,
    limit: u64,
) -> Result<BudgetTicket, GpuError> {
    pool.reserve(bytes as u64).map_err(|_| GpuError::Budget {
        domain: "effect pipeline metadata",
        requested: bytes as u64,
        available: limit.saturating_sub(pool.usage().bytes),
    })
}
fn grow<T>(values: &mut Vec<T>, pool: &BudgetPool, limit: u64) -> Result<BudgetTicket, GpuError> {
    let capacity = values.capacity().saturating_mul(2).max(values.len() + 1);
    let bytes = capacity
        .checked_mul(size_of::<T>())
        .ok_or(GpuError::Budget {
            domain: "effect pipeline metadata",
            requested: u64::MAX,
            available: limit.saturating_sub(pool.usage().bytes),
        })?;
    // Admit overlap before allocating the replacement. Publish it only after
    // charging the allocator's actual capacity; failure leaves old entries intact.
    let mut storage = reserve(pool, bytes, limit)?;
    let mut replacement = Vec::with_capacity(capacity);
    storage
        .resize((replacement.capacity() * size_of::<T>()) as u64)
        .map_err(|_| GpuError::Budget {
            domain: "effect pipeline metadata",
            requested: (replacement.capacity() * size_of::<T>()) as u64,
            available: limit.saturating_sub(pool.usage().bytes),
        })?;
    replacement.append(values);
    *values = replacement;
    Ok(storage)
}
impl<T: Clone> Cache<T> {
    #[cfg(feature = "wgpu")]
    pub(crate) fn peek(&self, key: [u8; 32]) -> Option<&T> {
        self.entries
            .find(hash(&key), |entry| entry.key == key)
            .map(|entry| &entry.compiled)
    }

    pub(crate) fn prune(&mut self) {
        self.entries.retain(|entry| {
            entry.owners.retain(|(_, owner)| !owner.is_released());
            !entry.owners.is_empty()
        });
        if self.entries.is_empty() {
            self.entries = HashTable::new();
            self.storage = None;
        }
    }
    pub(crate) fn get(
        &mut self,
        key: [u8; 32],
        lease: &ResourceLease,
        pool: &BudgetPool,
        limit: u64,
    ) -> Result<Option<T>, GpuError> {
        let Some(entry) = self.entries.find_mut(hash(&key), |entry| entry.key == key) else {
            return Ok(None);
        };
        if !entry
            .owners
            .iter()
            .any(|(handle, _)| *handle == lease.handle())
        {
            if entry.owners.len() == entry.owners.capacity() {
                entry.storage = grow(&mut entry.owners, pool, limit)?;
            }
            entry.owners.push((lease.handle(), lease.downgrade()));
        }
        Ok(Some(entry.compiled.clone()))
    }
    pub(crate) fn insert(
        &mut self,
        key: [u8; 32],
        lease: &ResourceLease,
        compiled: T,
        pool: &BudgetPool,
        limit: u64,
    ) -> Result<(), GpuError> {
        reel_core::budget::collections::reserve_table(
            &mut self.entries,
            1,
            &mut self.storage,
            pool,
            |entry| hash(&entry.key),
        )
        .map_err(|_| GpuError::Budget {
            domain: "effect pipeline metadata",
            requested: size_of::<Entry<T>>() as u64,
            available: limit.saturating_sub(pool.usage().bytes),
        })?;
        let storage = reserve(pool, size_of::<(ResourceHandle, ResourceWeak)>(), limit)?;
        self.entries.insert_unique(
            hash(&key),
            Entry {
                key,
                compiled,
                owners: vec![(lease.handle(), lease.downgrade())],
                storage,
            },
            |entry| hash(&entry.key),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_core::{
        budget::{BudgetCategory, OwnedBytes, QueueLimits},
        identity::{AssetKey, ContentVersion, ResourceVersion},
    };
    use reel_ui::{
        resources::{EffectShaderDescriptor, EffectShaderFormat, ResourceRegistry},
        stage::effect::EffectType,
    };
    use std::rc::Rc;
    #[test]
    fn code_identity_shares_across_definitions_and_releases_after_last_lease() {
        let pool = BudgetPool::new(
            BudgetCategory::GpuResource,
            QueueLimits {
                max_requests: 64,
                max_bytes: 1 << 20,
            },
        );
        let input = BudgetPool::new(
            BudgetCategory::CpuInput,
            QueueLimits {
                max_requests: 64,
                max_bytes: 1 << 20,
            },
        );
        let mut registry = ResourceRegistry::new(8, 1 << 20).unwrap();
        let mut register = |definition| {
            let raw = b"void main() {}".to_vec();
            let descriptor = EffectShaderDescriptor::new(
                [definition; 32],
                EffectShaderFormat::Wgsl,
                vec![],
                EffectType::Vec4,
                &raw,
            )
            .unwrap();
            let key = descriptor.shader_key;
            let ticket = input.reserve(raw.capacity() as u64).unwrap();
            let handle = registry
                .register_effect_shader(
                    ResourceVersion {
                        key: AssetKey(format!("effect/{definition}")),
                        version: ContentVersion([definition; 32]),
                    },
                    descriptor,
                    OwnedBytes::new(raw, ticket).unwrap(),
                )
                .unwrap();
            (key, registry.acquire(handle).unwrap())
        };
        let (key, a) = register(1);
        let (other, b) = register(2);
        assert_eq!(key, other);
        let mut cache = Cache::default();
        let compiled = Rc::new(17);
        cache
            .insert(key, &a, compiled.clone(), &pool, 1 << 20)
            .unwrap();
        assert!(Rc::ptr_eq(
            &cache.get(other, &b, &pool, 1 << 20).unwrap().unwrap(),
            &compiled
        ));
        registry.retire(a.handle());
        registry.retire(b.handle());
        drop(a);
        cache.prune();
        assert!(cache.get(key, &b, &pool, 1 << 20).unwrap().is_some());
        drop(b);
        cache.prune();
        assert_eq!(pool.usage().bytes, 0);
        assert_eq!(Rc::strong_count(&compiled), 1);
    }
}
