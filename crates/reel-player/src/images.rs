//! Stage image subscriptions over the host's shared resource service.
#[cfg(test)]
mod tests;
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
    encoded: BTreeMap<String, Arc<[u8]>>,
    displayed: BTreeSet<String>,
    pending: Option<Pending>,
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
            encoded: BTreeMap::new(),
            displayed: BTreeSet::new(),
            pending: None,
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
        self.encoded = view.encoded_images();
        match view.image_requests() {
            Ok(wanted) => self.request(wanted),
            Err(error) => {
                self.request(BTreeMap::new());
                self.failure = Some(error);
            }
        }
    }
    pub fn request(&mut self, wanted: BTreeMap<String, Option<String>>) {
        if self.closed {
            return;
        }
        if self.pending.as_ref().is_some_and(|p| {
            !wanted.contains_key(&p.source)
                && !self
                    .tiles
                    .get(&p.source)
                    .is_some_and(|tile| wanted.contains_key(&tile.owner))
        }) {
            self.assets.cancel(self.pending.take().unwrap().id);
        }
        self.wanted = wanted;
        self.tiles
            .retain(|_, tile| self.wanted.contains_key(&tile.owner));
        self.skipped.clear();
        self.manifests
            .retain(|source, _| self.wanted.contains_key(source));
        self.failure = None;
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
    pub fn poll(&mut self) -> Result<bool, String> {
        self.assets.maintain();
        if self.closed {
            return Ok(false);
        }
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.manifest)
        {
            let pending = self.pending.as_ref().unwrap();
            match self.assets.poll_bytes(pending.id) {
                Ok(Some((_, bytes))) => {
                    let manifest = PreparedImage::decode(bytes.as_slice())
                        .map_err(|error| self.diagnostic(&pending.source, error))?;
                    self.manifests.insert(pending.source.clone(), manifest);
                    self.pending = None;
                }
                Ok(None) => {}
                Err(error) => return Err(self.diagnostic(&pending.source, &error)),
            }
        } else if let Some(pending) = &self.pending {
            match self.assets.poll(pending.id) {
                Ok(Some(lease)) => {
                    if let Some(tile) = self.tiles.get(&pending.source) {
                        let ResourceData::Image { image, .. } = lease.data() else {
                            unreachable!()
                        };
                        if [image.width, image.height] != tile.dimensions {
                            let error = self.diagnostic(
                                &tile.owner,
                                &format!(
                                    "tile {:?} dimensions differ from manifest",
                                    pending.source
                                ),
                            );
                            let pending = self.pending.take().unwrap();
                            if pending.optional {
                                self.skipped.insert(pending.source);
                                return self.poll_tiles();
                            }
                            self.failure = Some(error.clone());
                            return Err(error);
                        }
                    }
                    self.loaded.insert(pending.source.clone(), lease);
                    self.pending = None;
                }
                Ok(None) => {}
                Err(error) => {
                    let pending = self.pending.take().unwrap();
                    if pending.optional {
                        self.skipped.insert(pending.source);
                        return Ok(true);
                    }
                    let origin = self
                        .wanted
                        .get(
                            self.tiles
                                .get(&pending.source)
                                .map_or(&pending.source, |tile| &tile.owner),
                        )
                        .and_then(|origin| origin.as_deref())
                        .unwrap_or("stage");
                    let error = format!("{origin}: image {:?}: {error}", pending.source);
                    self.failure = Some(error.clone());
                    return Err(error);
                }
            }
        }
        if let Some(source) = self.wanted.keys().find(|source| {
            !self.loaded.contains_key(*source) && !self.manifests.contains_key(*source)
        }) {
            if self.pending.is_none() {
                let manifest = source.ends_with(".reel-image");
                let id = if let Some(bytes) = self.encoded.get(source) {
                    self.assets.request_encoded(source.clone(), bytes)?
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
                self.pending = Some(Pending {
                    source: source.clone(),
                    id,
                    manifest,
                    optional: false,
                });
                self.assets.maintain();
                #[cfg(test)]
                {
                    self.loads += 1;
                }
            }
            Ok(false)
        } else {
            self.poll_tiles()
        }
    }
    pub(crate) fn metadata_ready(&self) -> bool {
        self.wanted
            .keys()
            .all(|source| self.loaded.contains_key(source) || self.manifests.contains_key(source))
    }
    fn diagnostic(&self, source: &str, error: &str) -> String {
        let origin = self
            .wanted
            .get(source)
            .and_then(|origin| origin.as_deref())
            .unwrap_or("stage");
        format!("{origin}: image {source:?}: {error}")
    }
    pub(crate) fn request_tiles(&mut self, tiles: BTreeMap<String, TileDemand>) -> bool {
        let changed = self.tiles != tiles;
        if self.pending.as_ref().is_some_and(|p| {
            !p.manifest && !self.wanted.contains_key(&p.source) && !tiles.contains_key(&p.source)
        }) {
            self.assets.cancel(self.pending.take().unwrap().id);
        }
        self.tiles = tiles;
        let missing_bytes: u64 = self
            .tiles
            .iter()
            .filter(|(source, tile)| tile.required && !self.loaded.contains_key(*source))
            .map(|(_, tile)| tile.bytes())
            .fold(0, u64::saturating_add);
        if missing_bytes != 0 {
            if self.pending.as_ref().is_some_and(|p| {
                p.optional && self.tiles.get(&p.source).is_none_or(|tile| !tile.required)
            }) {
                self.assets.cancel(self.pending.take().unwrap().id);
            }
            if let Some(pending) = &mut self.pending {
                pending.optional = false;
            }
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
    fn poll_tiles(&mut self) -> Result<bool, String> {
        let required = self
            .tiles
            .iter()
            .find(|(source, tile)| tile.required && !self.loaded.contains_key(*source))
            .map(|(source, _)| source.clone());
        if self.pending.is_none() {
            let next = required.clone().or_else(|| {
                self.tiles
                    .iter()
                    .find(|(source, tile)| {
                        !self.loaded.contains_key(*source)
                            && !self.skipped.contains(*source)
                            && tile.bytes().saturating_add(24) <= self.assets.resident_available()
                    })
                    .map(|(source, _)| source.clone())
            });
            if let Some(source) = next {
                let optional = !self.tiles[&source].required;
                let id = self.assets.request(source.clone(), AssetKind::Image);
                self.pending = Some(Pending {
                    source,
                    id,
                    manifest: false,
                    optional,
                });
                self.assets.maintain();
                #[cfg(test)]
                {
                    self.loads += 1;
                }
            }
        }
        Ok(required.is_none())
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
        self.loaded.retain(|source, lease| {
            let retain = self.wanted.contains_key(source) || self.tiles.contains_key(source);
            if !retain {
                lease.prohibit_new_leases();
            }
            retain
        });
    }
    pub fn close(&mut self) {
        self.closed = true;
        if let Some(pending) = self.pending.take() {
            self.assets.cancel(pending.id);
        }
        self.wanted.clear();
        self.tiles.clear();
        self.manifests.clear();
        self.encoded.clear();
        self.commit();
        self.assets.close();
    }
}
impl Drop for Images {
    fn drop(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.assets.cancel(pending.id);
        }
        self.wanted.clear();
        self.tiles.clear();
        self.commit();
    }
}
