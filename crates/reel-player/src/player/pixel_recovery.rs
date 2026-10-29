//! Candidate-scoped asynchronous restoration of backend-requested pixels.
use crate::assets::{AssetKind, Assets, LoadId};
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_render::PixelDemand;
use reel_ui::{bridge::CandidateKey, resources::ResourceLease};

struct Entry {
    source: ResourceLease,
    load: Option<LoadId>,
}
struct Waiter {
    key: CandidateKey,
    generation: u64,
    capture_id: Option<reel_engine::aside::capture::CaptureId>,
    demand: PixelDemand,
}
#[derive(Default)]
pub(crate) struct PixelRecovery {
    entries: Vec<Entry>,
    storage: Option<BudgetTicket>,
    frame: Option<Waiter>,
    capture: Option<Waiter>,
}
#[derive(Debug)]
pub(super) struct Failure {
    pub source: ResourceLease,
    pub error: String,
    pub frame: Option<CandidateKey>,
    pub capture: Option<(reel_engine::aside::capture::CaptureId, CandidateKey)>,
}
#[derive(Default)]
pub(super) struct Ready {
    pub frame: Option<CandidateKey>,
    pub cancel_frame: bool,
    pub capture: Option<(reel_engine::aside::capture::CaptureId, CandidateKey)>,
}
impl PixelRecovery {
    pub(super) fn active(&self) -> bool {
        self.frame.is_some() || self.capture.is_some() || !self.entries.is_empty()
    }
    fn reserve(&mut self, count: usize, pool: &BudgetPool) -> Result<(), String> {
        if count <= self.entries.capacity() {
            return Ok(());
        }
        let old_bytes = self
            .entries
            .capacity()
            .checked_mul(size_of::<Entry>())
            .ok_or("pixel recovery size overflow")?;
        let new_bytes = count
            .checked_mul(size_of::<Entry>())
            .ok_or("pixel recovery size overflow")?;
        let ticket = match &mut self.storage {
            Some(ticket) => ticket,
            None => self.storage.insert(pool.reserve(0).map_err(|e| e.message)?),
        };
        let overlap = old_bytes
            .checked_add(new_bytes)
            .ok_or("pixel recovery size overflow")?;
        ticket.resize(overlap as u64).map_err(|e| e.message)?;
        let mut next = Vec::new();
        if next.try_reserve_exact(count).is_err() {
            ticket.resize(old_bytes as u64).map_err(|e| e.message)?;
            return Err("pixel recovery allocation failed".into());
        }
        let Some(actual) = next.capacity().checked_mul(size_of::<Entry>()) else {
            drop(next);
            ticket.resize(old_bytes as u64).map_err(|e| e.message)?;
            return Err("pixel recovery size overflow".into());
        };
        let Some(actual_overlap) = old_bytes.checked_add(actual) else {
            drop(next);
            ticket.resize(old_bytes as u64).map_err(|e| e.message)?;
            return Err("pixel recovery size overflow".into());
        };
        if let Err(error) = ticket.resize(actual_overlap as u64) {
            drop(next);
            ticket.resize(old_bytes as u64).map_err(|e| e.message)?;
            return Err(error.message);
        }
        next.append(&mut self.entries);
        self.entries = next;
        ticket.resize(actual as u64).map_err(|e| e.message)?;
        Ok(())
    }
    pub(super) fn request(
        &mut self,
        capture_id: Option<reel_engine::aside::capture::CaptureId>,
        key: CandidateKey,
        generation: u64,
        demand: PixelDemand,
        pool: &BudgetPool,
    ) -> Result<(), String> {
        let additions = demand
            .resources()
            .iter()
            .filter(|source| {
                self.entries
                    .binary_search_by_key(&source.handle(), |entry| entry.source.handle())
                    .is_err()
            })
            .count();
        let count = self
            .entries
            .len()
            .checked_add(additions)
            .ok_or("pixel recovery size overflow")?;
        if let Err(error) = self.reserve(count, pool) {
            if self.entries.capacity() == 0 {
                self.storage = None;
            }
            return Err(error);
        }
        for source in demand.resources() {
            let handle = source.handle();
            if let Err(index) = self
                .entries
                .binary_search_by_key(&handle, |entry| entry.source.handle())
            {
                self.entries.insert(
                    index,
                    Entry {
                        source: source.clone(),
                        load: None,
                    },
                );
            }
        }
        let waiter = Some(Waiter {
            key,
            generation,
            capture_id,
            demand,
        });
        if capture_id.is_some() {
            self.capture = waiter;
        } else {
            self.frame = waiter;
        }
        Ok(())
    }
    fn prune_unused(&mut self, assets: &Assets) {
        let needed = |source: &ResourceLease| {
            [&self.frame, &self.capture]
                .into_iter()
                .flatten()
                .any(|waiter| waiter.demand.contains(source.handle()))
        };
        self.entries.retain(|entry| {
            let keep = needed(&entry.source);
            if !keep && let Some(id) = entry.load {
                assets.cancel(id);
            }
            keep
        });
        if self.frame.is_none() && self.capture.is_none() {
            self.entries = Vec::new();
            self.storage = None;
        }
    }
    pub(super) fn cancel_frame(&mut self, assets: &Assets) {
        self.frame = None;
        self.prune_unused(assets);
    }
    pub(super) fn cancel_capture(&mut self, assets: &Assets) {
        self.capture = None;
        self.prune_unused(assets);
    }
    pub(super) fn cancel(&mut self, assets: &Assets) {
        for entry in &self.entries {
            if let Some(id) = entry.load {
                assets.cancel(id);
            }
        }
        self.entries = Vec::new();
        self.storage = None;
        self.frame = None;
        self.capture = None;
    }
    pub(super) fn poll(
        &mut self,
        assets: &Assets,
        generation: u64,
        frame: Option<&CandidateKey>,
        capture: Option<(reel_engine::aside::capture::CaptureId, &CandidateKey)>,
    ) -> Result<Ready, Failure> {
        let mut ready = Ready::default();
        if self
            .frame
            .as_ref()
            .is_some_and(|w| w.generation != generation || frame != Some(&w.key))
        {
            let old = self.frame.take().unwrap();
            ready.cancel_frame = old.generation != generation && frame == Some(&old.key);
        }
        if self.capture.as_ref().is_some_and(|w| {
            w.generation != generation || capture != w.capture_id.map(|id| (id, &w.key))
        }) {
            let old = self.capture.take().unwrap();
            if old.generation != generation && capture == old.capture_id.map(|id| (id, &old.key)) {
                ready.capture = old.capture_id.map(|id| (id, old.key));
            }
        }
        let needed = |source: &ResourceLease| {
            [&self.frame, &self.capture]
                .into_iter()
                .flatten()
                .any(|w| w.demand.contains(source.handle()))
        };
        self.entries.retain(|entry| {
            let keep = needed(&entry.source);
            if !keep && let Some(id) = entry.load {
                assets.cancel(id);
            }
            keep
        });
        if self.entries.is_empty() && self.frame.is_none() && self.capture.is_none() {
            self.entries = Vec::new();
            self.storage = None;
            return Ok(ready);
        }
        assets.maintain();
        let mut failure = None;
        for entry in &mut self.entries {
            if let Some(id) = entry.load {
                match assets.take_ready(id) {
                    Ok(Some(replacement)) => {
                        entry.load = None;
                        if let Err(error) = entry.source.restore_pixels(&replacement) {
                            failure =
                                Some((entry.source.clone(), format!("restore pixels: {error:?}")));
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        entry.load = None;
                        failure = Some((entry.source.clone(), error));
                        break;
                    }
                }
            }
        }
        if let Some((source, error)) = failure {
            let includes = |waiter: &Waiter| waiter.demand.contains(source.handle());
            let frame = if self.frame.as_ref().is_some_and(includes) {
                self.frame.take().map(|waiter| waiter.key)
            } else {
                None
            };
            let capture = if self.capture.as_ref().is_some_and(includes) {
                self.capture
                    .take()
                    .map(|waiter| (waiter.capture_id.expect("capture id"), waiter.key))
            } else {
                None
            };
            let still_needed = |entry: &Entry| {
                [&self.frame, &self.capture]
                    .into_iter()
                    .flatten()
                    .any(|waiter| waiter.demand.contains(entry.source.handle()))
            };
            self.entries.retain(|entry| {
                let keep = still_needed(entry);
                if !keep && let Some(id) = entry.load {
                    assets.cancel(id);
                }
                keep
            });
            if self.frame.is_none() && self.capture.is_none() {
                self.entries = Vec::new();
                self.storage = None;
            }
            return Err(Failure {
                source,
                error,
                frame,
                capture,
            });
        }
        let in_flight = self
            .entries
            .iter()
            .filter(|entry| entry.load.is_some())
            .count();
        let mut free = (assets.worker_limits().max_requests as usize).saturating_sub(in_flight);
        let mut launched = false;
        for entry in &mut self.entries {
            if free == 0 {
                break;
            }
            if entry.load.is_none() && !entry.source.pixels_resident() {
                entry.load =
                    Some(assets.request_version(entry.source.version().clone(), AssetKind::Image));
                free -= 1;
                launched = true;
            }
        }
        if launched {
            assets.maintain();
        }
        let complete = |waiter: &Waiter| {
            waiter
                .demand
                .resources()
                .iter()
                .all(ResourceLease::pixels_resident)
        };
        if self.frame.as_ref().is_some_and(complete) {
            ready.frame = self.frame.take().map(|w| w.key);
        }
        if self.capture.as_ref().is_some_and(complete) {
            ready.capture = self
                .capture
                .take()
                .map(|w| (w.capture_id.expect("capture id"), w.key));
        }
        self.prune_unused(assets);
        Ok(ready)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::AssetOptions;
    use reel_core::{
        budget::{BudgetCategory, QueueLimits},
        identity::FrameId,
    };
    use std::{
        sync::{Arc, mpsc},
        time::{Duration, Instant},
    };

    #[test]
    fn failed_reload_identifies_both_waiters_and_releases_queue_budget() {
        use reel_core::{
            budget::OwnedBytes,
            identity::{AssetKey, ContentVersion, ResourceVersion},
        };
        use reel_ui::{
            peniko::{ImageAlphaType, ImageFormat},
            resources::{ColorEncoding, ResourceRegistry},
        };
        let assets = Assets::directory(AssetOptions {
            root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
            ..Default::default()
        });
        let (send, wake) = mpsc::channel();
        assets.set_waker(Arc::new(move || {
            let _ = send.send(());
        }));
        let pool = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: 32,
                max_bytes: 1 << 20,
            },
        );
        let mut registry = ResourceRegistry::new(1, 65536).unwrap();
        let handle = registry
            .register_image(
                ResourceVersion {
                    key: AssetKey("no-such-test-resource.png".into()),
                    version: ContentVersion([3; 32]),
                },
                OwnedBytes::new(vec![1; 4], pool.reserve(4).unwrap()).unwrap(),
                [1, 1],
                ImageFormat::Rgba8,
                ImageAlphaType::Alpha,
                ColorEncoding::Srgb,
            )
            .unwrap();
        let source = registry.acquire(handle).unwrap();
        source.permit_pixel_reload();
        assert!(source.discard_uploaded_pixels());
        let session = crate::session::tests::session();
        let session_id = session.identity();
        let ui = crate::session::SessionUi::new(session).unwrap();
        let (mut bridge, _) = crate::session::tests::mount(&ui);
        let recorded = bridge.record(FrameId(1)).unwrap();
        let key = recorded.key();
        let capture_id = reel_engine::aside::capture::CaptureId {
            session: session_id,
            serial: 2,
        };
        let demand = || {
            let values = vec![source.clone()];
            let bytes = values.capacity() * size_of::<ResourceLease>();
            PixelDemand::new(values, pool.reserve(bytes as u64).unwrap()).unwrap()
        };
        let mut recovery = PixelRecovery::default();
        recovery
            .request(None, key.clone(), 7, demand(), &pool)
            .unwrap();
        recovery
            .request(Some(capture_id), key.clone(), 7, demand(), &pool)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match recovery.poll(&assets, 7, Some(&key), Some((capture_id, &key))) {
                Err(failure) => {
                    assert_eq!(failure.source.handle(), handle);
                    assert_eq!(failure.frame, Some(key.clone()));
                    assert_eq!(failure.capture, Some((capture_id, key.clone())));
                    assert!(!failure.error.is_empty());
                    break;
                }
                Ok(_) => {
                    assert!(Instant::now() < deadline);
                    let _ = wake.recv_timeout(Duration::from_millis(50));
                }
            }
        }
        assert!(!source.pixels_resident());
        assert!(!recovery.active());
        assert_eq!(pool.usage().bytes, 0);
    }

    #[test]
    fn missing_frame_and_capture_share_reload_and_cancel_on_device_change() {
        let assets = Assets::directory(AssetOptions {
            root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
            ..Default::default()
        });
        let (send, wake) = mpsc::channel();
        assets.set_waker(Arc::new(move || {
            let _ = send.send(());
        }));
        let id = assets.request("assets/panels.png", AssetKind::Image);
        let deadline = Instant::now() + Duration::from_secs(10);
        let source = loop {
            if let Some(source) = assets.poll(id).unwrap() {
                break source;
            }
            assert!(Instant::now() < deadline);
            let _ = wake.recv_timeout(Duration::from_millis(50));
        };
        let original = source.handle();
        let source_id = source.read_pixels().unwrap().blob().id();
        assert!(source.discard_uploaded_pixels());
        let session = crate::session::tests::session();
        let session_id = session.identity();
        let ui = crate::session::SessionUi::new(session).unwrap();
        let (mut bridge, _) = crate::session::tests::mount(&ui);
        let recorded = bridge.record(FrameId(1)).unwrap();
        let key = recorded.key();
        let capture_id = reel_engine::aside::capture::CaptureId {
            session: session_id,
            serial: 1,
        };
        let pool = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: 32,
                max_bytes: 1 << 20,
            },
        );
        let demand = || {
            let values = vec![source.clone()];
            let bytes = values.capacity() * size_of::<ResourceLease>();
            PixelDemand::new(values, pool.reserve(bytes as u64).unwrap()).unwrap()
        };
        let mut recovery = PixelRecovery::default();
        recovery
            .request(None, key.clone(), 7, demand(), &pool)
            .unwrap();
        recovery
            .request(Some(capture_id), key.clone(), 7, demand(), &pool)
            .unwrap();
        assert_eq!(recovery.entries.len(), 1, "same handle must share one load");
        recovery.cancel_frame(&assets);
        assert_eq!(
            recovery.entries.len(),
            1,
            "capture retains the shared request"
        );
        recovery
            .request(None, key.clone(), 7, demand(), &pool)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let ready = recovery
                .poll(&assets, 7, Some(&key), Some((capture_id, &key)))
                .unwrap();
            if ready.frame.is_some() && ready.capture.is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "asynchronous restore must wake the host"
            );
            let _ = wake.recv_timeout(Duration::from_millis(50));
        }
        assert_eq!(source.handle(), original);
        let pixels = source.read_pixels().unwrap();
        assert_eq!(pixels.blob().id(), source_id);
        assert_eq!(&pixels.bytes()[..4], &[224, 64, 48, 255]);
        drop(pixels);
        assert!(!recovery.active());
        assert_eq!(pool.usage().bytes, 0);
        assert!(source.discard_uploaded_pixels());
        recovery
            .request(None, key.clone(), 7, demand(), &pool)
            .unwrap();
        recovery.poll(&assets, 7, Some(&key), None).unwrap();
        assert!(
            recovery.entries[0].load.is_some(),
            "reload must be in flight"
        );
        let stale = recovery.poll(&assets, 8, Some(&key), None).unwrap();
        assert!(stale.cancel_frame);
        assert!(
            !recovery.active(),
            "device generation change cancels stale work"
        );
        assert!(!source.pixels_resident());
    }
}
