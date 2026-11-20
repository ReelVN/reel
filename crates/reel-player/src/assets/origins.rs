//! Weak provenance and a budgeted lookup accelerator for immutable resources.
use super::{AssetKind, Load, RetainedAssetKind};
use hashbrown::HashTable;
use reel_core::budget::{BudgetPool, BudgetTicket};
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
// The CPU cache keeps only decoded images. Uploaded sources stay addressable
// through their device cache entries and the ordinary weak origin index.
pub(super) struct RecentImages {
    entries: lru::LruCache<String, RecentImage>,
    decoded_bytes: u64,
}

struct RecentImage {
    lease: ResourceLease,
    bytes: u64,
}

impl Default for RecentImages {
    fn default() -> Self {
        Self {
            entries: lru::LruCache::unbounded(),
            decoded_bytes: 0,
        }
    }
}

impl RecentImages {
    pub(super) fn insert(&mut self, source: String, lease: ResourceLease, decoded_limit: u64) {
        if lease.is_retired() {
            return;
        }
        let Some(pixel_bytes) = lease.pixel_byte_len().filter(|_| lease.pixels_resident()) else {
            return;
        };
        let bytes = pixel_bytes as u64
            + lease.descriptor_bytes()
            + (source.capacity() + size_of::<(String, RecentImage)>()) as u64;
        if bytes > decoded_limit {
            return;
        }
        self.take(&source);
        while self.decoded_bytes > decoded_limit - bytes {
            self.evict_decoded();
        }
        self.entries.put(source, RecentImage { lease, bytes });
        self.decoded_bytes += bytes;
    }

    pub(super) fn take(&mut self, source: &str) -> Option<ResourceLease> {
        let image = self.entries.pop(source)?;
        self.decoded_bytes -= image.bytes;
        (!image.lease.is_retired() && image.lease.pixels_resident()).then_some(image.lease)
    }

    pub(super) fn decoded_bytes(&self) -> u64 {
        self.decoded_bytes
    }

    pub(super) fn make_room(&mut self, output: &BudgetPool, bytes: u64) {
        while self.decoded_bytes != 0 && bytes > output.available_bytes() {
            self.evict_decoded();
        }
    }

    fn evict_decoded(&mut self) {
        let (_, image) = self.entries.pop_lru().expect("bytes own a decoded image");
        self.decoded_bytes -= image.bytes;
    }
}

pub(super) struct Origins {
    items: BTreeMap<ResourceHandle, (ResourceWeak, RetainedAssetKind)>,
    index: Option<Index>,
    metadata: BudgetPool,
    sweep_at: usize,
}
fn kind(kind: AssetKind) -> Option<RetainedAssetKind> {
    match kind {
        AssetKind::Image => Some(RetainedAssetKind::Image),
        AssetKind::Font => Some(RetainedAssetKind::Font),
        AssetKind::EffectShader => Some(RetainedAssetKind::EffectShader),
        AssetKind::Bytes => None,
    }
}
fn fingerprint(key: &str, kind: RetainedAssetKind) -> u64 {
    let mut hash = DefaultHasher::new();
    key.hash(&mut hash);
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
            sweep_at: 64,
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
        self.by_source(&load.source.key.0, kind, |lease| {
            lease.version().version == version
                && (kind != RetainedAssetKind::Image
                    || load.reuse_uploaded_image
                    || lease.pixels_resident())
        })
    }

    /// The mounted stage sees fixed source paths. A texture that has already
    /// released its decoded pixels can reenter the stage before requesting IO.
    pub(super) fn stage_image(&self, source: &str) -> Option<ResourceLease> {
        self.by_source(source, RetainedAssetKind::Image, |lease| {
            !lease.pixels_resident()
        })
    }

    fn by_source(
        &self,
        key: &str,
        kind: RetainedAssetKind,
        accepts: impl Fn(&ResourceLease) -> bool,
    ) -> Option<ResourceLease> {
        let candidate = |(weak, candidate_kind): &(ResourceWeak, RetainedAssetKind)| {
            (*candidate_kind == kind)
                .then(|| weak.upgrade())
                .flatten()
                .filter(|lease| lease.version().key.0 == key && accepts(lease))
        };
        if self.items.len() >= 16
            && let Some(index) = &self.index
        {
            let hash = fingerprint(key, kind);
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
            lease.permit_pixel_reload(Some(&self.metadata));
        }
        let handle = lease.handle();
        if self
            .items
            .insert(handle, (lease.downgrade(), kind))
            .is_some()
        {
            return;
        }
        if self.items.len() >= self.sweep_at {
            self.prune_released();
        }
        let Some(index) = &mut self.index else {
            return;
        };
        let item = Entry {
            hash: fingerprint(&lease.version().key.0, kind),
            handle,
        };
        if !index.push(item, &self.metadata) {
            self.prune_released();
            if !self.index.as_mut().unwrap().push(item, &self.metadata) {
                self.index = None;
            }
        }
    }

    fn prune_released(&mut self) {
        self.items.retain(|_, (weak, _)| !weak.is_released());
        if let Some(index) = &mut self.index {
            index
                .entries
                .retain(|entry| self.items.contains_key(&entry.handle));
        }
        self.sweep_at = self.items.len() + self.items.len().max(64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{Phase, ResourceRegistry};
    use reel_core::budget::OwnedBytes;
    use reel_core::{
        budget::{BudgetCategory, QueueLimits},
        identity::{AssetKey, AssetSource, ContentVersion, ResourceVersion},
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
        let hash = fingerprint(&request.source.key.0, RetainedAssetKind::Image);
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
