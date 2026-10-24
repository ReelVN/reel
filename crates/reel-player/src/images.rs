//! Stage image subscriptions over the host's shared resource service.
#[cfg(test)]
pub(crate) mod tests;
use crate::assets::{AssetKind, Assets, LoadId};
use reel_core::prepared_image::PreparedImage;
use reel_ui::{
    kurbo::{Affine, Rect, Size},
    resources::ResourceData,
    widgets::ImageTile,
};
pub(crate) mod tiles;
#[cfg(test)]
use reel_core::budget::BudgetPool;
use reel_ui::resources::ResourceLease;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
pub(crate) use tiles::{ImageRegion, TileDemand};

struct Pending {
    source: String,
    id: LoadId,
    manifest: bool,
    optional: bool,
}
pub(crate) struct Images {
    pub(crate) assets: Assets,
    #[cfg(test)]
    pub(crate) pool: BudgetPool,
    #[cfg(test)]
    pub(crate) work: BudgetPool,
    pub(crate) loaded: BTreeMap<String, ResourceLease>,
    pub(crate) manifests: BTreeMap<String, PreparedImage>,
    pub(crate) tiles: BTreeMap<String, TileDemand>,
    skipped: BTreeSet<String>,
    wanted: BTreeMap<String, Option<String>>,
    mounted: BTreeSet<String>,
    encoded: BTreeMap<String, Arc<[u8]>>,
    displayed: BTreeSet<String>,
    pending: Vec<Pending>,
    failure: Option<String>,
    closed: bool,
    #[cfg(test)]
    pub(crate) loads: u64,
}
impl Images {
    pub fn new(assets: Assets) -> Self {
        Self {
            #[cfg(test)]
            pool: assets.output_pool(),
            #[cfg(test)]
            work: assets.temporary_pool(),
            assets,
            loaded: BTreeMap::new(),
            manifests: BTreeMap::new(),
            tiles: BTreeMap::new(),
            skipped: BTreeSet::new(),
            wanted: BTreeMap::new(),
            mounted: BTreeSet::new(),
            encoded: BTreeMap::new(),
            displayed: BTreeSet::new(),
            pending: Vec::new(),
            failure: None,
            closed: false,
            #[cfg(test)]
            loads: 0,
        }
    }
    pub fn set_waker(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.assets.set_waker(wake);
    }
    #[cfg(feature = "window")]
    pub fn has_result_pending(&self) -> bool {
        self.assets.needs_poll()
    }
    pub fn request_stage(&mut self, view: &crate::stage_widget::StageView) {
        match view.image_requests() {
            Ok(requests) => {
                self.encoded = requests.encoded;
                self.request_mounted(requests.required, requests.mounted);
            }
            Err(error) => {
                self.encoded.clear();
                self.request(BTreeMap::new());
                self.failure = Some(error);
            }
        }
    }
    pub fn request(&mut self, wanted: BTreeMap<String, Option<String>>) {
        let mounted = wanted.keys().cloned().collect();
        self.request_mounted(wanted, mounted);
    }
    fn request_mounted(
        &mut self,
        wanted: BTreeMap<String, Option<String>>,
        mounted: BTreeSet<String>,
    ) {
        if self.closed {
            return;
        }
        self.pending.retain(|pending| {
            let retain = wanted.contains_key(&pending.source)
                || self
                    .tiles
                    .get(&pending.source)
                    .is_some_and(|tile| wanted.contains_key(&tile.owner));
            if !retain {
                self.assets.cancel(pending.id);
            }
            retain
        });
        self.wanted = wanted;
        self.mounted = mounted;
        self.tiles
            .retain(|_, tile| self.wanted.contains_key(&tile.owner));
        self.skipped.clear();
        self.manifests
            .retain(|source, _| self.wanted.contains_key(source));
        self.failure = None;
        let loading = self.wanted.keys().any(|source| {
            !self.loaded.contains_key(source) && !self.manifests.contains_key(source)
        });
        self.loaded.retain(|source, _| {
            self.wanted.contains_key(source)
                || self.displayed.contains(source)
                || self.tiles.contains_key(source)
                || (!loading && self.mounted.contains(source))
        });
    }
    pub fn poll(&mut self) -> Result<bool, String> {
        self.assets.maintain();
        if self.closed {
            return Ok(false);
        }
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        let mut index = 0;
        while index < self.pending.len() {
            let pending = &self.pending[index];
            let result = if pending.manifest {
                self.assets.poll_bytes(pending.id).and_then(|result| {
                    result
                        .map(|(_, bytes)| {
                            let manifest =
                                PreparedImage::decode(bytes.as_slice()).map_err(str::to_owned)?;
                            self.manifests.insert(pending.source.clone(), manifest);
                            Ok(())
                        })
                        .transpose()
                })
            } else {
                self.assets.take_ready(pending.id).and_then(|result| {
                    result
                        .map(|lease| {
                            if let Some(tile) = self.tiles.get(&pending.source) {
                                let ResourceData::Image { image, .. } = lease.data() else {
                                    unreachable!()
                                };
                                if [image.width, image.height] != tile.dimensions {
                                    return Err("tile dimensions differ from manifest".into());
                                }
                            }
                            self.loaded.insert(pending.source.clone(), lease);
                            Ok(())
                        })
                        .transpose()
                })
            };
            match result {
                Ok(None) => index += 1,
                Ok(Some(())) => {
                    self.pending.remove(index);
                }
                Err(error) => {
                    let pending = self.pending.remove(index);
                    if pending.optional {
                        self.skipped.insert(pending.source);
                    } else {
                        let error = self.diagnostic(&pending.source, &error);
                        self.failure = Some(error.clone());
                        return Err(error);
                    }
                }
            }
        }
        let sources = self
            .wanted
            .keys()
            .filter(|source| {
                !self.loaded.contains_key(*source)
                    && !self.manifests.contains_key(*source)
                    && !self.is_loading(source)
            })
            .take(self.free_slots())
            .cloned()
            .collect::<Vec<_>>();
        for source in sources {
            let manifest = source.ends_with(".reel-image");
            let id = if let Some(bytes) = self.encoded.get(&source) {
                let Some(id) = self
                    .assets
                    .try_request_encoded(source.clone(), bytes)
                    .map_err(|error| self.diagnostic(&source, &error))?
                else {
                    continue;
                };
                id
            } else {
                self.assets.request(
                    source.clone(),
                    if manifest {
                        AssetKind::Bytes
                    } else {
                        AssetKind::Image
                    },
                )
            };
            self.push_pending(Pending {
                source,
                id,
                manifest,
                optional: false,
            });
        }
        let ready = if self.metadata_ready() {
            self.poll_tiles()
        } else {
            false
        };
        self.assets.maintain();
        Ok(ready)
    }
    fn is_loading(&self, source: &str) -> bool {
        self.pending.iter().any(|pending| pending.source == source)
    }
    fn free_slots(&self) -> usize {
        (self.assets.worker_limits().max_requests as usize).saturating_sub(self.pending.len())
    }
    fn push_pending(&mut self, pending: Pending) {
        self.pending.push(pending);
        #[cfg(test)]
        {
            self.loads += 1;
        }
    }
    pub(crate) fn metadata_ready(&self) -> bool {
        self.wanted
            .keys()
            .all(|source| self.loaded.contains_key(source) || self.manifests.contains_key(source))
    }
    fn diagnostic(&self, source: &str, error: &str) -> String {
        let owner = self
            .tiles
            .get(source)
            .map_or(source, |tile| tile.owner.as_str());
        let origin = self
            .wanted
            .get(owner)
            .and_then(|origin| origin.as_deref())
            .unwrap_or("stage");
        format!("{origin}: image {source:?}: {error}")
    }
    pub(crate) fn request_tiles(&mut self, tiles: BTreeMap<String, TileDemand>) -> bool {
        let changed = self.tiles != tiles;
        self.pending.retain(|pending| {
            let retain = pending.manifest
                || self.wanted.contains_key(&pending.source)
                || tiles.contains_key(&pending.source);
            if !retain {
                self.assets.cancel(pending.id);
            }
            retain
        });
        self.tiles = tiles;
        let missing_bytes: u64 = self
            .tiles
            .iter()
            .filter(|(source, tile)| tile.required && !self.loaded.contains_key(*source))
            .map(|(_, tile)| tile.bytes())
            .fold(0, u64::saturating_add);
        if missing_bytes != 0 {
            self.pending.retain_mut(|pending| {
                if pending.optional
                    && self
                        .tiles
                        .get(&pending.source)
                        .is_none_or(|tile| !tile.required)
                {
                    self.assets.cancel(pending.id);
                    return false;
                }
                pending.optional = false;
                true
            });
            if missing_bytes.saturating_add(24) > self.assets.resident_available() {
                self.loaded.retain(|source, lease| {
                    let retain = self.wanted.contains_key(source)
                        || self.displayed.contains(source)
                        || self.tiles.get(source).is_some_and(|tile| tile.required);
                    if !retain {
                        lease.prohibit_new_leases();
                    }
                    retain
                });
            }
        }
        self.skipped
            .retain(|source| self.tiles.contains_key(source));
        self.retain_loaded();
        changed
    }
    fn retain_loaded(&mut self) {
        self.loaded.retain(|source, lease| {
            let retain = self.wanted.contains_key(source)
                || self.displayed.contains(source)
                || self.tiles.contains_key(source);
            if !retain {
                lease.prohibit_new_leases();
            }
            retain
        });
    }
    fn poll_tiles(&mut self) -> bool {
        let required = self
            .tiles
            .iter()
            .any(|(source, tile)| tile.required && !self.loaded.contains_key(source));
        let sources = self
            .tiles
            .iter()
            .filter(|(source, tile)| {
                !self.loaded.contains_key(*source)
                    && !self.is_loading(source)
                    && if required {
                        tile.required
                    } else {
                        !self.skipped.contains(*source)
                    }
            })
            .take(self.free_slots())
            .map(|(source, _)| source.clone())
            .collect::<Vec<_>>();
        // Pending optional tiles have not reserved their eventual decoded storage yet.
        let reserved = self
            .pending
            .iter()
            .filter_map(|pending| self.tiles.get(&pending.source))
            .map(|tile| tile.bytes().saturating_add(24))
            .fold(0, u64::saturating_add);
        let mut available = self.assets.resident_available().saturating_sub(reserved);
        for source in sources {
            let tile = &self.tiles[&source];
            let optional = !tile.required;
            if optional {
                let bytes = tile.bytes().saturating_add(24);
                if bytes > available {
                    continue;
                }
                available -= bytes;
            }
            let id = self.assets.request(source.clone(), AssetKind::Image);
            self.push_pending(Pending {
                source,
                id,
                manifest: false,
                optional,
            });
        }
        !required
    }
    pub(crate) fn tile_paint(&self, region: &ImageRegion) -> Vec<ImageTile> {
        region
            .tiles
            .iter()
            .filter_map(|key| {
                let tile = self.tiles.get(key)?;
                let lease = self.loaded.get(key)?;
                let ResourceData::Image { image, .. } = lease.data() else {
                    unreachable!()
                };
                Some(ImageTile {
                    image: image.clone().into(),
                    transform: tile.transform,
                    clip: tile.clip,
                })
            })
            .collect()
    }
    pub fn commit(&mut self) {
        self.displayed = self.wanted.keys().cloned().collect();
        self.displayed.extend(
            self.tiles
                .iter()
                .filter(|(_, tile)| tile.required)
                .map(|(source, _)| source.clone()),
        );
        self.loaded
            .retain(|source, _| self.mounted.contains(source) || self.tiles.contains_key(source));
    }
    pub fn close(&mut self) {
        self.closed = true;
        for pending in self.pending.drain(..) {
            self.assets.cancel(pending.id);
        }
        self.wanted.clear();
        self.mounted.clear();
        self.tiles.clear();
        self.manifests.clear();
        self.encoded.clear();
        self.skipped.clear();
        self.failure = None;
        self.commit();
        // Assets belongs to the host. A replacement session and other consumers
        // keep using it after this stage has released its own requests and leases.
    }
}
impl Drop for Images {
    fn drop(&mut self) {
        self.close();
    }
}
