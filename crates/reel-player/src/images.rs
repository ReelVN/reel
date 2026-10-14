//! Stage image subscriptions over the host's shared resource service.
#[cfg(test)]
mod tests;
use crate::assets::{AssetKind, Assets, LoadId};
#[cfg(test)]
use reel_core::budget::BudgetPool;
use reel_ui::resources::ResourceLease;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

struct Pending {
    source: String,
    id: LoadId,
}
pub(crate) struct Images {
    pub(crate) assets: Assets,
    #[cfg(test)]
    pub(crate) pool: BudgetPool,
    #[cfg(test)]
    pub(crate) work: BudgetPool,
    pub(crate) loaded: BTreeMap<String, ResourceLease>,
    wanted: BTreeMap<String, Option<String>>,
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
            wanted: BTreeMap::new(),
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
    pub fn request(&mut self, wanted: BTreeMap<String, Option<String>>) {
        if self.closed {
            return;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|p| !wanted.contains_key(&p.source))
        {
            self.assets.cancel(self.pending.take().unwrap().id);
        }
        self.wanted = wanted;
        self.failure = None;
        self.loaded.retain(|source, lease| {
            let retain = self.wanted.contains_key(source) || self.displayed.contains(source);
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
        if let Some(pending) = &self.pending {
            match self.assets.poll(pending.id) {
                Ok(Some(lease)) => {
                    self.loaded.insert(pending.source.clone(), lease);
                    self.pending = None;
                }
                Ok(None) => {}
                Err(error) => {
                    let pending = self.pending.take().unwrap();
                    let origin = self
                        .wanted
                        .get(&pending.source)
                        .and_then(|origin| origin.as_deref())
                        .unwrap_or("stage");
                    let error = format!("{origin}: image {:?}: {error}", pending.source);
                    self.failure = Some(error.clone());
                    return Err(error);
                }
            }
        }
        if let Some(source) = self
            .wanted
            .keys()
            .find(|source| !self.loaded.contains_key(*source))
        {
            if self.pending.is_none() {
                let id = self.assets.request(source.clone(), AssetKind::Image);
                self.pending = Some(Pending {
                    source: source.clone(),
                    id,
                });
                self.assets.maintain();
                #[cfg(test)]
                {
                    self.loads += 1;
                }
            }
            Ok(false)
        } else {
            Ok(true)
        }
    }
    pub fn commit(&mut self) {
        self.displayed = self.wanted.keys().cloned().collect();
        self.loaded.retain(|source, lease| {
            let retain = self.wanted.contains_key(source);
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
        self.commit();
    }
}
