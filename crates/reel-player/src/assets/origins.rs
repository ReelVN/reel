//! Weak provenance and a budgeted lookup accelerator for immutable resources.
use super::{AssetKind, Load, RetainedAssetKind};
use hashbrown::HashTable;
use reel_core::{
    budget::{BudgetPool, BudgetTicket},
    identity::{AssetKey, ContentVersion},
};
use reel_ui::resources::{ResourceHandle, ResourceLease, ResourceWeak};
use std::{
    collections::{BTreeMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    hash: u64,
    handle: ResourceHandle,
}
struct Index {
    entries: HashTable<Entry>,
    ticket: Option<BudgetTicket>,
}
// Recently used GPU images can keep their identity through a story replacement.
// GPU pressure can still evict their pixels and request exact-version IO.
pub(super) struct RecentImages {
    slots: [Option<(String, ResourceLease)>; 64],
    next: usize,
}
impl Default for RecentImages {
    fn default() -> Self {
        Self {
            slots: [const { None }; 64],
            next: 0,
        }
    }
}
impl RecentImages {
    pub(super) fn insert(&mut self, source: String, lease: ResourceLease) {
        let backing = lease.image_backing();
        if lease.is_retired() || backing.pixel_byte_len().is_none() || backing.pixels_resident() {
            return;
        }
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|(key, _)| key == &source));
        if let Some(slot) = slot {
            *slot = Some((source, lease));
        } else {
            self.slots[self.next] = Some((source, lease));
            self.next = (self.next + 1) % self.slots.len();
        }
    }
    pub(super) fn take(&mut self, source: &str) -> Option<ResourceLease> {
        self.slots
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|(key, _)| key == source))?
            .take()
            .map(|(_, lease)| lease)
            .filter(|lease| !lease.is_retired())
    }
}

pub(super) struct Origins {
    items: BTreeMap<ResourceHandle, (ResourceWeak, RetainedAssetKind)>,
    index: Option<Index>,
    metadata: BudgetPool,
}
fn kind(kind: AssetKind) -> Option<RetainedAssetKind> {
    match kind {
        AssetKind::Image => Some(RetainedAssetKind::Image),
        AssetKind::Font => Some(RetainedAssetKind::Font),
        AssetKind::EffectShader => Some(RetainedAssetKind::EffectShader),
        AssetKind::Bytes => None,
    }
}
fn fingerprint(key: &AssetKey, version: ContentVersion, kind: RetainedAssetKind) -> u64 {
    let mut hash = DefaultHasher::new();
    key.hash(&mut hash);
    version.hash(&mut hash);
    (match kind {
        RetainedAssetKind::Image => 0_u8,
        RetainedAssetKind::Font => 1,
        RetainedAssetKind::EffectShader => 2,
    })
    .hash(&mut hash);
    hash.finish()
}
impl Index {
    // Reserve both old and new vectors while growth reallocates. If metadata is
    // occupied, the caller discards this optional accelerator and scans instead.
    fn push(&mut self, item: Entry, metadata: &BudgetPool) -> bool {
        if reel_core::budget::collections::reserve_table(
            &mut self.entries,
            1,
            &mut self.ticket,
            metadata,
            |entry| entry.hash,
        )
        .is_err()
        {
            return false;
        }
        self.entries
            .insert_unique(item.hash, item, |entry| entry.hash);
        true
    }
}
impl Origins {
    pub(super) fn new(metadata: BudgetPool) -> Self {
        Self {
            items: BTreeMap::new(),
            index: Some(Index {
                entries: HashTable::new(),
                ticket: None,
            }),
            metadata,
        }
    }
    pub(super) fn clear(&mut self) {
        self.items.clear();
        self.index = None;
    }
    pub(super) fn kind(&self, handle: ResourceHandle) -> Option<RetainedAssetKind> {
        self.items.get(&handle).map(|(_, kind)| *kind)
    }
    pub(super) fn shared(&self, load: &Load) -> Option<ResourceLease> {
        if !load.reloadable {
            return None;
        }
        let version = load.source.version?;
        let kind = kind(load.kind)?;
        let candidate = |(weak, candidate_kind): &(ResourceWeak, RetainedAssetKind)| {
            (*candidate_kind == kind)
                .then(|| weak.upgrade())
                .flatten()
                .filter(|lease| {
                    lease.version().key == load.source.key
                        && lease.version().version == version
                        && (kind != RetainedAssetKind::Image
                            || load.reuse_uploaded_image
                            || lease.pixels_resident())
                })
        };
        if self.items.len() >= 16
            && let Some(index) = &self.index
        {
            let hash = fingerprint(&load.source.key, version, kind);
            index
                .entries
                .iter_hash(hash)
                .filter(|entry| entry.hash == hash)
                .find_map(|entry| self.items.get(&entry.handle).and_then(candidate))
        } else {
            self.items.values().find_map(candidate)
        }
    }
    pub(super) fn remember(&mut self, asset_kind: AssetKind, lease: &ResourceLease) {
        let kind = kind(asset_kind).expect("bytes do not have resource leases");
        if kind == RetainedAssetKind::Image {
            lease.permit_pixel_reload();
        }
        self.items.retain(|_, (weak, _)| !weak.is_released());
        if let Some(index) = &mut self.index {
            index.entries.retain(|entry| {
                entry.handle != lease.handle() && self.items.contains_key(&entry.handle)
            });
            let item = Entry {
                hash: fingerprint(&lease.version().key, lease.version().version, kind),
                handle: lease.handle(),
            };
            if !index.push(item, &self.metadata) {
                self.index = None;
            }
        }
        self.items.insert(lease.handle(), (lease.downgrade(), kind));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{Phase, ResourceRegistry};
    use reel_core::budget::OwnedBytes;
    use reel_core::{
        budget::{BudgetCategory, QueueLimits},
        identity::{AssetSource, ResourceVersion},
    };
    use reel_ui::{
        peniko::{ImageAlphaType, ImageFormat},
        resources::ColorEncoding,
    };

    fn pool(bytes: u64) -> BudgetPool {
        BudgetPool::new(
            BudgetCategory::Metadata,
            QueueLimits {
                max_requests: 10000,
                max_bytes: bytes,
            },
        )
    }
    fn load(version: ResourceVersion) -> Load {
        Load {
            source: AssetSource {
                key: version.key,
                version: Some(version.version),
            },
            reloadable: true,
            reuse_uploaded_image: false,
            kind: AssetKind::Image,
            priority: reel_core::identity::Priority::Visible,
            prefetch_limit: u64::MAX,
            phase: Phase::Describe,
            input: None,
            size: [0, 0],
            prepared: None,
            output: 0,
            temporary: 0,
        }
    }
    fn fixture(
        count: usize,
        metadata: BudgetPool,
    ) -> (Origins, ResourceRegistry, Vec<ResourceLease>) {
        let mut registry = ResourceRegistry::new(count as u32, 1 << 20).unwrap();
        let data = pool(1 << 20);
        let mut origins = Origins::new(metadata);
        let mut leases = Vec::new();
        for i in 0..count {
            let version = ResourceVersion {
                key: AssetKey(format!("image/{i}")),
                version: ContentVersion([i as u8; 32]),
            };
            let handle = registry
                .register_image(
                    version,
                    OwnedBytes::new(vec![i as u8; 4], data.reserve(4).unwrap()).unwrap(),
                    [1, 1],
                    ImageFormat::Rgba8,
                    ImageAlphaType::Alpha,
                    ColorEncoding::Srgb,
                )
                .unwrap();
            let lease = registry.acquire(handle).unwrap();
            origins.remember(AssetKind::Image, &lease);
            leases.push(lease);
        }
        (origins, registry, leases)
    }
    #[test]
    fn indexed_weak_reuse_respects_collisions_retirement_and_metadata_pressure() {
        let metadata = pool(1 << 20);
        let (mut origins, mut registry, leases) = fixture(20, metadata.clone());
        let expected = &leases[10];
        let request = load(expected.version().clone());
        assert_eq!(
            origins.shared(&request).unwrap().handle(),
            expected.handle()
        );
        let index = origins.index.as_mut().unwrap();
        let hash = fingerprint(
            &request.source.key,
            request.source.version.unwrap(),
            RetainedAssetKind::Image,
        );
        index
            .entries
            .retain(|entry| entry.handle != leases[0].handle());
        index.entries.insert_unique(
            hash,
            Entry {
                hash,
                handle: leases[0].handle(),
            },
            |entry| entry.hash,
        );
        assert_eq!(
            origins.shared(&request).unwrap().handle(),
            expected.handle()
        );
        let mut wrong_kind = load(expected.version().clone());
        wrong_kind.kind = AssetKind::Font;
        assert!(origins.shared(&wrong_kind).is_none());
        expected.prohibit_new_leases();
        assert!(origins.shared(&request).is_none());
        assert!(metadata.usage().bytes > 0);
        origins.clear();
        assert_eq!(metadata.usage().bytes, 0);
        for lease in leases {
            assert!(registry.retire(lease.handle()));
        }

        let limited = pool(0);
        let (origins, _, leases) = fixture(2, limited.clone());
        assert!(origins.index.is_none());
        assert_eq!(
            origins
                .shared(&load(leases[1].version().clone()))
                .unwrap()
                .handle(),
            leases[1].handle()
        );
        assert_eq!(limited.usage().bytes, 0);
    }
}
