//! Stage image subscriptions over the host's shared resource service.
#[cfg(test)]
pub(crate) mod tests;
use crate::assets::{AssetKind, Assets, ImageAsset, LoadId};
use reel_core::prepared_image::PreparedImage;
use reel_ui::{
    kurbo::{Affine, Rect, Size},
    widgets::ImageTile,
};
pub(crate) mod tiles;
#[cfg(test)]
use reel_core::budget::BudgetPool;
use reel_ui::resources::{EffectShaderFormat, ResourceLease};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
pub(crate) use tiles::{ImageRegion, TileDemand};

struct Pending {
    source: String,
    id: LoadId,
    optional: bool,
    prefetch: bool,
}

#[derive(Default)]
struct PrefetchSources {
    stage: BTreeSet<String>,
    predicted: Box<[Box<str>]>,
}

impl PrefetchSources {
    fn contains(&self, source: &str) -> bool {
        self.predicted(source) || self.stage.contains(source)
    }

    fn predicted(&self, source: &str) -> bool {
        self.predicted.iter().any(|path| path.as_ref() == source)
    }

    fn candidates(&self, include_stage: bool) -> impl Iterator<Item = &str> {
        self.predicted.iter().map(Box::as_ref).chain(
            include_stage
                .then_some(&self.stage)
                .into_iter()
                .flatten()
                .map(String::as_str),
        )
    }
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
    prefetch: PrefetchSources,
    prediction_changed: bool,
    approaching: Vec<reel_ui::stage::NodeId>,
    mounted: BTreeSet<String>,
    encoded: BTreeMap<String, Arc<[u8]>>,
    shader_sources: BTreeSet<String>,
    request_index: Option<Arc<crate::stage_widget::ResourceIndex>>,
    pub(crate) shader_format: EffectShaderFormat,
    /// Advances when newly prepared content can affect the current projection.
    pub(crate) content_revision: u64,
    displayed: BTreeSet<String>,
    pending: Vec<Pending>,
    failure: Option<String>,
    closed: bool,
    settled: bool,
    /// Wanted metadata survives tile demand, displayed-lease retirement and
    /// commits. Only demand changes and completed source loads update readiness.
    metadata_ready: bool,
    #[cfg(test)]
    poll_scans: usize,
    #[cfg(test)]
    metadata_scans: usize,
    #[cfg(test)]
    pub(crate) loads: u64,
    #[cfg(test)]
    pub(crate) stage_requests: u64,
    #[cfg(test)]
    pub(crate) tile_requests: u64,
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
            prefetch: PrefetchSources::default(),
            prediction_changed: false,
            approaching: Vec::new(),
            mounted: BTreeSet::new(),
            encoded: BTreeMap::new(),
            shader_sources: BTreeSet::new(),
            request_index: None,
            shader_format: if cfg!(target_os = "vita") {
                EffectShaderFormat::Gxp
            } else {
                EffectShaderFormat::Wgsl
            },
            displayed: BTreeSet::new(),
            content_revision: 0,
            pending: Vec::new(),
            failure: None,
            closed: false,
            settled: false,
            metadata_ready: true,
            #[cfg(test)]
            poll_scans: 0,
            #[cfg(test)]
            metadata_scans: 0,
            #[cfg(test)]
            loads: 0,
            #[cfg(test)]
            stage_requests: 0,
            #[cfg(test)]
            tile_requests: 0,
        }
    }
    pub fn set_waker(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.assets.set_waker(wake);
    }
    pub fn has_result_pending(&self) -> bool {
        self.assets.needs_poll()
    }
    #[cfg(test)]
    pub fn request_stage(&mut self, view: &crate::stage_widget::StageView) {
        self.request_stage_images(view.image_requests(self.shader_format));
    }
    /// Return true when settled whole-image demand and diagnostics are unchanged.
    /// The caller may retain its resolved leases; tiled viewport work stays live.
    pub(crate) fn request_stage_images(
        &mut self,
        requests: crate::stage_widget::ImageRequests,
    ) -> bool {
        #[cfg(test)]
        {
            self.stage_requests += 1;
        }
        if self.closed {
            return false;
        }
        let same_index = self
            .request_index
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, &requests.index));
        let same_approach = self.approaching == requests.approaching;
        let same_prefetch = same_index
            && same_approach
            && (!requests.index.has_skin_paths
                || requests
                    .preloaded()
                    .eq(self.prefetch.stage.iter().map(String::as_str)));
        if same_prefetch
            && self.failure.is_none()
            && self.manifests.is_empty()
            && self
                .skipped
                .iter()
                .all(|source| self.prefetch.contains(source))
            && requests.required().eq(self
                .wanted
                .iter()
                .map(|(source, origin)| (source.as_str(), origin.as_deref())))
        {
            // Geometry may move again while the same images are still being read.
            // poll() owns their progress; only changed demand needs reconciliation.
            return self.settled;
        }
        if !same_prefetch {
            self.prefetch.stage = requests.preloaded().map(str::to_owned).collect();
            self.approaching.clone_from(&requests.approaching);
        }
        if !same_index {
            self.encoded = requests
                .index
                .encoded()
                .map(|(source, bytes)| (source.to_owned(), bytes.clone()))
                .collect();
            self.shader_sources = requests.index.shaders().map(str::to_owned).collect();
            self.mounted = requests.index.mounted().map(str::to_owned).collect();
            self.request_index = Some(requests.index.clone());
        }
        self.wanted
            .retain(|source, _| requests.origin(source).is_some());
        for (source, origin) in requests.required() {
            if let Some(current) = self.wanted.get_mut(source) {
                if current.as_deref() != origin {
                    *current = origin.map(str::to_owned);
                }
            } else {
                self.wanted
                    .insert(source.to_owned(), origin.map(str::to_owned));
            }
        }
        self.reconcile_requests();
        false
    }
    pub(crate) fn layout_ready(&self, requests: &crate::stage_widget::ImageRequests) -> bool {
        requests.intrinsic_sources().all(|source| {
            self.loaded.contains_key(source)
                || self.manifests.contains_key(source)
                || self.pending_image_size(source).is_some()
        })
    }
    pub(crate) fn pending_image_size(&self, source: &str) -> Option<[u32; 2]> {
        let pending = self
            .pending
            .iter()
            .find(|pending| pending.source == source)?;
        self.assets.pending_image_size(pending.id)
    }
    pub(crate) fn is_required(&self, source: &str) -> bool {
        self.wanted.contains_key(source)
    }
    pub(crate) fn set_image_prediction(&mut self, sources: &[Box<str>]) {
        if self.prefetch.predicted.as_ref() != sources {
            self.prefetch.predicted = sources.into();
            // Apply alongside the next visible demand so it can take over
            // an in-flight read when the next view makes its predicted image visible.
            self.prediction_changed = true;
        }
    }
    #[cfg(test)]
    pub fn request(&mut self, wanted: BTreeMap<String, Option<String>>) {
        self.request_paths(wanted, BTreeSet::new());
    }
    pub(crate) fn request_paths(
        &mut self,
        wanted: BTreeMap<String, Option<String>>,
        prefetch: BTreeSet<String>,
    ) {
        if self.closed {
            return;
        }
        self.request_index = None;
        self.mounted = wanted.keys().cloned().collect();
        self.wanted = wanted;
        self.prefetch = PrefetchSources {
            stage: prefetch,
            predicted: Box::default(),
        };
        self.approaching.clear();
        self.reconcile_requests();
    }
    fn reconcile_requests(&mut self) {
        self.prediction_changed = false;
        self.settled = false;
        // Reuse completed CPU work before requesting IO. Uploaded textures keep
        // their source addressable through the ordinary asset lookup.
        for source in self.wanted.keys() {
            if !self.loaded.contains_key(source)
                && let Some(lease) = self.assets.take_recent_image(source)
            {
                self.loaded.insert(source.clone(), lease);
                self.content_revision = self.content_revision.wrapping_add(1);
            }
        }
        let foreground_ready = self
            .wanted
            .keys()
            .all(|source| self.loaded.contains_key(source) || self.manifests.contains_key(source));
        self.pending.retain_mut(|pending| {
            let wanted = self.wanted.contains_key(&pending.source);
            if pending.prefetch && wanted && self.assets.promote_prefetch(pending.id) {
                pending.prefetch = false;
                pending.optional = false;
                return true;
            }
            let retain = if pending.prefetch {
                foreground_ready && self.prefetch.contains(&pending.source) && !wanted
            } else {
                wanted
                    || self
                        .tiles
                        .get(&pending.source)
                        .is_some_and(|tile| self.wanted.contains_key(&tile.owner))
            };
            if !retain {
                self.assets.cancel(pending.id);
            }
            retain
        });
        self.tiles
            .retain(|_, tile| self.wanted.contains_key(&tile.owner));
        self.skipped
            .retain(|source| foreground_ready && self.prefetch.contains(source));
        self.manifests.retain(|source, _| {
            self.wanted.contains_key(source) || (foreground_ready && self.prefetch.contains(source))
        });
        self.failure = None;
        self.update_metadata_ready();
        let loading = !self.metadata_ready;
        for (source, lease) in self.loaded.extract_if(.., |source, _| {
            !(self.wanted.contains_key(source)
                || self.displayed.contains(source)
                || self.tiles.contains_key(source)
                || (!loading && (self.mounted.contains(source) || self.prefetch.contains(source))))
        }) {
            self.assets.remember_stage_image(source, lease);
        }
        self.trim_hidden_prefetch();
    }
    pub fn poll(&mut self) -> Result<bool, String> {
        self.assets.maintain();
        if self.closed {
            return Ok(false);
        }
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        if self.prediction_changed {
            self.reconcile_requests();
        }
        if self.settled {
            return Ok(true);
        }
        #[cfg(test)]
        {
            self.poll_scans += 1;
        }
        let mut index = 0;
        let mut metadata_completed = false;
        while index < self.pending.len() {
            let pending = &self.pending[index];
            let result = self.assets.take_image(pending.id).and_then(|result| {
                result
                    .map(|image| match image {
                        ImageAsset::Tiled(manifest) => {
                            if self.shader_sources.contains(&pending.source) {
                                return Err("effect shaders require prepared shader data".into());
                            }
                            if self.tiles.contains_key(&pending.source) {
                                return Err("image tiles must contain raster data".into());
                            }
                            self.manifests.insert(pending.source.clone(), manifest);
                            Ok(())
                        }
                        ImageAsset::Raster(lease) => {
                            if let Some(tile) = self.tiles.get(&pending.source)
                                && lease.image_size() != Some(tile.dimensions)
                            {
                                return Err("tile dimensions differ from manifest".into());
                            }
                            self.loaded.insert(pending.source.clone(), lease);
                            Ok(())
                        }
                    })
                    .transpose()
            });
            match result {
                Ok(None) => index += 1,
                Ok(Some(())) => {
                    let pending = self.pending.remove(index);
                    metadata_completed |=
                        !self.metadata_ready && self.wanted.contains_key(&pending.source);
                    if !pending.prefetch
                        || self.tiles.contains_key(&pending.source)
                        || self
                            .request_index
                            .as_ref()
                            .is_some_and(|index| index.has_visible_use(&pending.source))
                    {
                        self.content_revision = self.content_revision.wrapping_add(1);
                    }
                }
                Err(error) => {
                    let pending = self.pending.remove(index);
                    if pending.optional {
                        self.skipped.insert(pending.source);
                    } else {
                        let error = self.diagnostic(&pending.source, &error);
                        self.failure = Some(error.clone());
                        if metadata_completed {
                            self.update_metadata_ready();
                        }
                        return Err(error);
                    }
                }
            }
        }
        if metadata_completed {
            self.update_metadata_ready();
        }
        let sources = if self.metadata_ready {
            Vec::new()
        } else {
            #[cfg(test)]
            {
                self.metadata_scans += 1;
            }
            self.wanted
                .keys()
                .filter(|source| {
                    !self.loaded.contains_key(*source)
                        && !self.manifests.contains_key(*source)
                        && !self.is_loading(source)
                })
                .take(self.free_slots())
                .cloned()
                .collect::<Vec<_>>()
        };
        for source in sources {
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
                self.assets.request_stage_asset(
                    source.clone(),
                    if self.shader_sources.contains(&source) {
                        AssetKind::EffectShader
                    } else {
                        AssetKind::Image
                    },
                )
            };
            self.push_pending(Pending {
                source,
                id,
                optional: false,
                prefetch: false,
            });
        }
        let ready = if self.metadata_ready() {
            self.poll_tiles()
        } else {
            false
        };
        self.assets.maintain();
        while ready && self.free_slots() != 0 {
            let in_flight = self.pending.iter().position(|pending| pending.prefetch);
            if in_flight.is_some_and(|index| self.prefetch.predicted(&self.pending[index].source)) {
                break;
            }
            let Some(source) = self
                .prefetch
                .candidates(in_flight.is_none())
                .find(|source| {
                    !self.wanted.contains_key(*source)
                        && !self.loaded.contains_key(*source)
                        && !self.manifests.contains_key(*source)
                        && !self.skipped.contains(*source)
                        && !self.is_loading(source)
                })
                .map(str::to_owned)
            else {
                break;
            };
            if let Some(lease) = self.assets.take_recent_image(&source) {
                self.loaded.insert(source, lease);
                continue;
            }
            // Follow the current prediction ahead of unrelated stage prefetch.
            // A matching pending request keeps its work.
            if let Some(index) = in_flight {
                self.assets.cancel(self.pending.remove(index).id);
            }
            let kind = if self.shader_sources.contains(&source) {
                AssetKind::EffectShader
            } else {
                AssetKind::Image
            };
            let limit = self.assets.prefetch_available(self.hidden_prefetch_bytes());
            let request = if let Some(bytes) = self.encoded.get(&source) {
                self.assets.prefetch_encoded(source.clone(), bytes, limit)
            } else {
                Ok(self.assets.prefetch(source.clone(), kind, limit))
            };
            match request {
                Ok(id) => self.push_pending(Pending {
                    source,
                    id,
                    optional: true,
                    prefetch: true,
                }),
                Err(_) => {
                    self.skipped.insert(source);
                }
            }
            self.assets.maintain();
        }
        // Loaded leases stay owned until demand changes. Optional tiles denied
        // by the resident budget remain unsettled so released capacity is retried.
        self.settled = ready
            && self.pending.is_empty()
            && self
                .tiles
                .keys()
                .all(|source| self.loaded.contains_key(source) || self.skipped.contains(source));
        Ok(ready)
    }
    fn is_loading(&self, source: &str) -> bool {
        self.pending.iter().any(|pending| pending.source == source)
    }
    fn hidden_prefetch_bytes(&self) -> u64 {
        use reel_ui::resources::ResourceView;
        let mut backings = BTreeSet::new();
        self.loaded
            .iter()
            .filter_map(|(source, lease)| {
                if !self.prefetch.contains(source)
                    || self.wanted.contains_key(source)
                    || self.displayed.contains(source)
                {
                    return None;
                }
                let backing = lease.image_backing();
                if !backings.insert(backing.handle()) {
                    return None;
                }
                if backing.pixels_resident() {
                    let header = u64::from(backing.texture_descriptor().is_some())
                        * reel_core::texture::TextureHeader::LEN as u64;
                    Some(backing.pixel_byte_len().unwrap() as u64 + header)
                } else if let ResourceView::EffectShader { bytes, .. } = backing.view() {
                    Some(bytes.data().len() as u64)
                } else {
                    None
                }
            })
            .sum()
    }
    fn trim_hidden_prefetch(&mut self) {
        if (self.prefetch.predicted.is_empty() && self.prefetch.stage.is_empty())
            || self.hidden_prefetch_bytes() <= self.assets.prefetch_available(0)
        {
            return;
        }
        for (key, _) in self.loaded.extract_if(.., |key, _| {
            self.prefetch.contains(key)
                && !self.wanted.contains_key(key)
                && !self.displayed.contains(key)
        }) {
            self.skipped.insert(key);
        }
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
        self.metadata_ready
    }
    fn update_metadata_ready(&mut self) {
        #[cfg(test)]
        {
            self.metadata_scans += 1;
        }
        self.metadata_ready = self
            .wanted
            .keys()
            .all(|source| self.loaded.contains_key(source) || self.manifests.contains_key(source));
    }
    pub(crate) fn restore_diagnostic(&self, lease: &ResourceLease, error: &str) -> String {
        if let Some((source, _)) = self
            .loaded
            .iter()
            .find(|(_, candidate)| candidate.handle() == lease.handle())
        {
            self.diagnostic(source, error)
        } else {
            format!("image {:?}: {error}", lease.version().key.0)
        }
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
        let kind = if self.shader_sources.contains(source) {
            "effect shader"
        } else {
            "image"
        };
        format!("{origin}: {kind} {source:?}: {error}")
    }
    pub(crate) fn request_tiles(&mut self, tiles: BTreeMap<String, TileDemand>) -> bool {
        #[cfg(test)]
        {
            self.tile_requests += 1;
        }
        let changed = self.tiles != tiles;
        if changed {
            self.settled = false;
        }
        self.pending.retain_mut(|pending| {
            let wanted = self.wanted.contains_key(&pending.source);
            let tile = tiles.get(&pending.source);
            let retain = wanted
                || tile.is_some()
                || (pending.prefetch && self.prefetch.contains(&pending.source));
            if !retain {
                self.assets.cancel(pending.id);
            } else if !pending.prefetch {
                pending.optional = !wanted && tile.is_some_and(|tile| !tile.required);
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
            .retain(|source| self.tiles.contains_key(source) || self.prefetch.contains(source));
        self.retain_loaded();
        changed
    }
    fn retain_loaded(&mut self) {
        self.loaded.retain(|source, _| {
            self.wanted.contains_key(source)
                || self.displayed.contains(source)
                || self.tiles.contains_key(source)
                || (self.metadata_ready && self.prefetch.contains(source))
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
                optional,
                prefetch: false,
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
                Some(ImageTile {
                    resource: lease.clone(),
                    transform: tile.transform,
                    clip: tile.clip,
                })
            })
            .collect()
    }
    pub fn commit(&mut self) {
        // A previously required image may now be only a skin alternative.
        // Releasing the old frame must not turn its whole pixel allocation into
        // speculative residency outside the prefetch quota.
        let retiring_prefetch = self
            .displayed
            .iter()
            .any(|key| !self.wanted.contains_key(key) && self.prefetch.contains(key));
        self.displayed = self.wanted.keys().cloned().collect();
        self.displayed.extend(
            self.tiles
                .iter()
                .filter(|(_, tile)| tile.required)
                .map(|(source, _)| source.clone()),
        );
        if retiring_prefetch {
            self.trim_hidden_prefetch();
        }
        for (source, lease) in self.loaded.extract_if(.., |source, _| {
            !self.mounted.contains(source)
                && !self.prefetch.contains(source)
                && !self.tiles.contains_key(source)
        }) {
            self.assets.remember_stage_image(source, lease);
        }
    }
    pub fn close(&mut self) {
        self.closed = true;
        for pending in self.pending.drain(..) {
            self.assets.cancel(pending.id);
        }
        self.wanted.clear();
        self.prefetch = PrefetchSources::default();
        self.approaching.clear();
        self.metadata_ready = true;
        self.mounted.clear();
        self.tiles.clear();
        self.manifests.clear();
        self.encoded.clear();
        self.shader_sources.clear();
        self.request_index = None;
        self.skipped.clear();
        self.failure = None;
        self.displayed.clear();
        self.loaded.clear();
        // Assets belongs to the host. A replacement session and other consumers
        // keep using it after this stage has released its own requests and leases.
    }
}
impl Drop for Images {
    fn drop(&mut self) {
        self.close();
    }
}
