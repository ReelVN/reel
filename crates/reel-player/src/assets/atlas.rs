//! Atlas view requests share one ordinary page load until their last waiter leaves.
use super::*;
use reel_core::{budget::BudgetTicket, prepared_atlas::PreparedAtlasView};
use std::ops::Bound::{Excluded, Included};

pub(super) struct ViewRequest {
    pub(super) manifest: PreparedAtlasView,
    pub(super) _storage: BudgetTicket,
}
struct Page {
    load: LoadId,
    waiters: usize,
    _storage: BudgetTicket,
}
#[derive(Default)]
pub(super) struct Pages(BTreeMap<String, Page>);
impl Pages {
    pub(super) fn clear(&mut self) {
        self.0.clear();
    }
}
impl Service {
    pub(super) fn promote_prefetch(&mut self, id: LoadId) -> bool {
        let load = &self.loads[&id];
        match &load.phase {
            Phase::Done(Err(_)) => return false,
            Phase::AtlasWait(view) => {
                let page = self.atlas_pages.0[&view.manifest.page].load;
                let backing = &self.loads[&page];
                if matches!(backing.phase, Phase::Done(Err(_))) {
                    return false;
                }
                self.promote_load(page);
            }
            _ => {}
        }
        self.promote_load(id);
        true
    }

    fn promote_load(&mut self, id: LoadId) {
        let load = self.loads.get_mut(&id).unwrap();
        if load.priority >= Priority::Visible {
            return;
        }
        load.priority = Priority::Visible;
        match load.phase {
            Phase::Describing(id) | Phase::Reading(id) => {
                self.io.as_mut().unwrap().promote_visible(id)
            }
            Phase::Probing(id) | Phase::Decoding(id) => {
                self.executor.as_mut().unwrap().promote_visible(id)
            }
            _ => {}
        }
    }

    /// Remove one waiter. The shared IO/work request is cancelled only at zero.
    pub(super) fn release_atlas(&mut self, path: &str) {
        let Some(page) = self.atlas_pages.0.get_mut(path) else {
            return;
        };
        page.waiters -= 1;
        if page.waiters != 0 {
            return;
        }
        let page = self.atlas_pages.0.remove(path).unwrap();
        if let Some(load) = self.loads.remove(&page.load) {
            match load.phase {
                Phase::Describing(id) | Phase::Reading(id) => {
                    if let Some(io) = &mut self.io {
                        io.cancel(id);
                    }
                }
                Phase::Probing(id) | Phase::Decoding(id) => {
                    if let Some(worker) = &mut self.executor {
                        worker.cancel(id);
                    }
                }
                _ => {}
            }
        }
    }
    /// Advance only the requests present at entry; page requests run next pump.
    pub(super) fn advance_atlases(&mut self) {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::AssetAtlas);
        let limit = self.next;
        let mut after = 0;
        while let Some(id) = self
            .loads
            .range((Excluded(LoadId(after)), Included(LoadId(limit))))
            .next()
            .map(|(&id, _)| id)
        {
            after = id.0;
            if !matches!(
                self.loads[&id].phase,
                Phase::AtlasResolve(_) | Phase::AtlasWait(_)
            ) {
                continue;
            }
            let phase =
                std::mem::replace(&mut self.loads.get_mut(&id).unwrap().phase, Phase::Describe);
            match phase {
                Phase::AtlasResolve(view) => {
                    let result = self.attach_atlas(id, &view.manifest.page);
                    self.loads.get_mut(&id).unwrap().phase = match result {
                        Ok(()) => Phase::AtlasWait(view),
                        Err(error) => Phase::Done(Err(error)),
                    };
                }
                Phase::AtlasWait(view) => {
                    let page = &view.manifest.page;
                    let result = self
                        .atlas_pages
                        .0
                        .get(page)
                        .and_then(|shared| self.loads.get(&shared.load))
                        .and_then(|load| match &load.phase {
                            Phase::Done(result) => Some(result.clone()),
                            Phase::ImageManifest(_) => {
                                Some(Err("atlas page is a tiled image".into()))
                            }
                            _ => None,
                        });
                    if let Some(result) = result {
                        let result = result
                            .and_then(|backing| self.register_atlas(id, &view.manifest, backing));
                        self.loads.get_mut(&id).unwrap().phase = Phase::Done(result);
                        self.release_atlas(page);
                        self.wake.wake();
                    } else {
                        self.loads.get_mut(&id).unwrap().phase = Phase::AtlasWait(view);
                    }
                }
                _ => unreachable!(),
            }
        }
    }
    fn attach_atlas(&mut self, id: LoadId, path: &str) -> Result<(), String> {
        let priority = self.loads[&id].priority;
        let prefetch_limit = self.loads[&id].prefetch_limit;
        if self.atlas_pages.0.values().any(|page| page.load == id) {
            return Err("atlas page cannot reference another atlas view".into());
        }
        if let Some(page) = self.atlas_pages.0.get_mut(path) {
            let backing = page.load;
            page.waiters = page.waiters.checked_add(1).ok_or("atlas waiter capacity")?;
            if priority == Priority::Visible {
                self.promote_load(backing);
            }
            return Ok(());
        }
        let storage = self
            .metadata
            .reserve((path.len() * 2 + 512) as u64)
            .map_err(|e| e.message)?;
        let path = path.to_owned();
        self.next = self
            .next
            .checked_add(1)
            .ok_or("asset request identity exhausted")?;
        let load = LoadId(self.next);
        self.loads.insert(
            load,
            Load {
                source: AssetSource {
                    key: AssetKey(path.clone()),
                    version: None,
                },
                reloadable: true,
                kind: AssetKind::Image,
                priority,
                prefetch_limit,
                phase: Phase::Describe,
                input: None,
                size: [0; 2],
                texture: None,
                output: 0,
                temporary: 0,
            },
        );
        self.atlas_pages.0.insert(
            path,
            Page {
                load,
                waiters: 1,
                _storage: storage,
            },
        );
        self.wake.wake();
        Ok(())
    }
    fn register_atlas(
        &mut self,
        id: LoadId,
        view: &PreparedAtlasView,
        backing: ResourceLease,
    ) -> Result<ResourceLease, String> {
        let load = &self.loads[&id];
        let version = ResourceVersion {
            key: load.source.key.clone(),
            version: load
                .source
                .version
                .ok_or("atlas view has no fixed version")?,
        };
        let mut registry = ResourceRegistry::new(1, 1024 + version.key.0.capacity() as u64)
            .map_err(|e| format!("atlas descriptor: {e:?}"))?;
        let handle = registry
            .register_atlas_view(version, view.image, view.encoding, backing)
            .map_err(|e| format!("atlas view: {e:?}"))?;
        let lease = registry
            .acquire(handle)
            .map_err(|e| format!("atlas lease: {e:?}"))?;
        if load.reloadable {
            self.origins.remember(AssetKind::Image, &lease);
        }
        Ok(lease)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::tests::memory_reader::MemoryReader;
    use reel_core::{
        identity::ContentVersion,
        test_support::{MemoryAsset, MemoryAssetIo},
        texture::{
            ImageView, TextureCodec, TextureDescriptor, TextureFormat, TextureHeader, TextureLayout,
        },
    };

    #[test]
    fn shared_page_survives_one_view_cancel_and_is_loaded_once() {
        let assets = Assets::with_adapters(AssetOptions::default(), |wake| {
            let limits = QueueLimits {
                max_requests: 32,
                max_bytes: 1 << 20,
            };
            let input = BudgetPool::new(BudgetCategory::CpuInput, limits);
            let mut io = MemoryAssetIo::new(limits, limits, wake.clone());
            let page = "__reel/atlas/key/page.reel-texture";
            let descriptor = TextureDescriptor {
                storage_layout: Default::default(),
                mip_count: 1,
                format: TextureFormat::Rgba8,
                source_encoding: ColorEncoding::Srgb,
                layout: TextureLayout::identity([16, 8]),
            };
            let mut data = TextureHeader::new(descriptor, TextureCodec::Raw, 512)
                .unwrap()
                .encode()
                .to_vec();
            data.extend([255; 512]);
            let mut files = vec![(page.to_owned(), data)];
            for (i, name) in ["a.png", "b.png", "c.png"].into_iter().enumerate() {
                let view = PreparedAtlasView {
                    page: page.into(),
                    image: ImageView {
                        logical_size: [4, 4],
                        content_origin: [(i as u32) * 4, 2],
                        content_size: [4, 4],
                    },
                    encoding: ColorEncoding::Srgb,
                };
                files.push((name.into(), view.encode().unwrap()));
            }
            for (key, bytes) in files {
                let version = ResourceVersion {
                    key: AssetKey(key),
                    version: ContentVersion([1; 32]),
                };
                let ticket = input.reserve(bytes.len() as u64).unwrap();
                io.insert_asset(
                    version,
                    MemoryAsset {
                        bytes: OwnedBytes::new(bytes, ticket).unwrap(),
                        media_type: None,
                    },
                )
                .unwrap();
            }
            (
                Box::new(MemoryReader::new(io, wake.clone(), true, None)),
                Box::new(ImageExecutor::new(limits, wake)),
            )
        });
        let a = assets.prefetch("a.png".into(), AssetKind::Image, 0);
        let b = assets.prefetch("b.png".into(), AssetKind::Image, 0);
        let c = assets.prefetch("c.png".into(), AssetKind::Image, 0);
        for _ in 0..20 {
            assets.maintain();
            if assets
                .0
                .borrow()
                .atlas_pages
                .0
                .values()
                .any(|page| page.waiters == 3)
            {
                break;
            }
        }
        assert_eq!(
            assets
                .0
                .borrow()
                .atlas_pages
                .0
                .values()
                .next()
                .unwrap()
                .waiters,
            3
        );
        assert!(assets.promote_prefetch(b));
        assets.cancel(a);
        assert_eq!(
            assets
                .0
                .borrow()
                .atlas_pages
                .0
                .values()
                .next()
                .unwrap()
                .waiters,
            2
        );
        let b = crate::assets::tests::load_until_done(&assets, b).unwrap();
        let c = crate::assets::tests::load_until_done(&assets, c).unwrap();
        assert_eq!(b.image_backing().handle(), c.image_backing().handle());
        assert_ne!(
            b.image_view().unwrap().content_origin,
            c.image_view().unwrap().content_origin
        );
        assert_eq!(
            assets.output_pool().usage().bytes,
            512 + TextureHeader::LEN as u64
        );
        assert!(assets.0.borrow().atlas_pages.0.is_empty());
    }
}
