//! Catalog IO shares author assets; parsing uses the existing bounded resource worker.
use crate::assets::{AssetKind, Assets, LoadId, worker::Worker};
use reel_core::{budget::OwnedBytes, identity::Priority};
use reel_engine::localization::{Catalog, CatalogLoader};
use std::{collections::BTreeMap, sync::Arc};

struct Parse {
    id: u64,
    locale: String,
    source: String,
    bytes: Arc<OwnedBytes>,
}
struct Parsed {
    id: u64,
    result: Result<Catalog, String>,
}
enum Pending {
    Reading {
        load: LoadId,
        locale: String,
        source: String,
    },
    Parsing,
    Done(Result<Catalog, String>),
}
pub struct AssetCatalogLoader {
    assets: Assets,
    worker: Worker<Parse, Parsed>,
    pending: BTreeMap<u64, Pending>,
    events: Vec<Parsed>,
    next: u64,
}
impl AssetCatalogLoader {
    pub fn new(assets: Assets) -> Self {
        let worker = Worker::new(
            assets.worker_limits(),
            assets.wake_signal(),
            |request: Parse, _| {
                let result = std::str::from_utf8(request.bytes.as_slice())
                    .map_err(|e| format!("{}: {e}", request.source))
                    .and_then(|text| Catalog::parse(&request.locale, &request.source, text.into()));
                Parsed {
                    id: request.id,
                    result,
                }
            },
            |event| event.result = Err("catalog load cancelled".into()),
        );
        Self {
            assets,
            worker,
            pending: BTreeMap::new(),
            events: Vec::new(),
            next: 0,
        }
    }
}
impl CatalogLoader for AssetCatalogLoader {
    fn load(&mut self, locale: &str, source: &str) -> Result<u64, String> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or("catalog request identity exhausted")?;
        self.pending.insert(
            self.next,
            Pending::Reading {
                load: self.assets.request(source, AssetKind::Bytes),
                locale: locale.into(),
                source: source.into(),
            },
        );
        Ok(self.next)
    }
    fn poll(&mut self, id: u64) -> Option<Result<Catalog, String>> {
        self.assets.maintain();
        self.worker.drain(&mut self.events, 32);
        for event in self.events.drain(..) {
            if let Some(pending) = self.pending.get_mut(&event.id) {
                *pending = Pending::Done(event.result);
            }
        }
        let pending = self.pending.get_mut(&id)?;
        if let Pending::Reading {
            load,
            locale,
            source,
        } = pending
        {
            match self.assets.poll_bytes(*load) {
                Ok(Some((_, bytes))) => {
                    let size = bytes.as_slice().len() as u64;
                    let request = Parse {
                        id,
                        locale: locale.clone(),
                        source: source.clone(),
                        bytes,
                    };
                    *pending = match self.worker.accept(id, request, size, Priority::Normal) {
                        Ok(()) => Pending::Parsing,
                        Err(error) => Pending::Done(Err(error.reason.message)),
                    };
                }
                Err(error) => *pending = Pending::Done(Err(error)),
                Ok(None) => {}
            }
        }
        if matches!(pending, Pending::Done(_)) {
            let Pending::Done(result) = self.pending.remove(&id).unwrap() else {
                unreachable!()
            };
            Some(result)
        } else {
            None
        }
    }
    fn cancel(&mut self, id: u64) {
        match self.pending.remove(&id) {
            Some(Pending::Reading { load, .. }) => self.assets.cancel(load),
            Some(Pending::Parsing) => self.worker.cancel(id),
            _ => {}
        }
    }
}
impl Drop for AssetCatalogLoader {
    fn drop(&mut self) {
        for pending in self.pending.values() {
            if let Pending::Reading { load, .. } = pending {
                self.assets.cancel(*load);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        aside_session::{AsideSessionUi, StoryState},
        assets::AssetOptions,
        story_runtime::StoryRuntime,
    };
    use reel_engine::{
        Epoch, Session, SessionId,
        aside::{SliceBudget, VmLimits},
    };

    #[test]
    fn localization_resource_wait_wakes_runtime_and_then_becomes_idle() {
        let script = r#"fn main() {
            localization.load("en", "locales/en.ftl");
            localization.set_locale("en");
            if tr("雨停了。", id: "opening") != "The rain stopped." { throw "external catalog"; }
            let mut failed = false;
            try { localization.load("en", "locales/missing.ftl"); } catch error { failed = true; }
            if !failed || tr("", id: "opening") != "The rain stopped." { throw "failed load changed catalog"; }
        }"#;
        let driver = AsideSessionUi::from_source(
            Session::new(SessionId(72), Epoch(1)).unwrap(),
            script,
            "localized",
            1,
            "main",
            "zh-CN",
            VmLimits::default(),
        )
        .unwrap()
        .with_asset_options(AssetOptions {
            root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
            ..Default::default()
        });
        let mut runtime = StoryRuntime::new(driver);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut waited = false;
        while runtime.driver().state() != StoryState::Finished || runtime.needs_tick() {
            assert!(
                std::time::Instant::now() < deadline,
                "catalog runtime stalled"
            );
            assert!(matches!(
                runtime.driver().state(),
                StoryState::Running | StoryState::Finished
            ));
            runtime.tick(SliceBudget::default(), || 0).unwrap();
            if runtime.driver().adapter().localization_pending() {
                waited = true;
                assert!(runtime.needs_tick());
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        assert!(waited);
        assert!(!runtime.needs_tick());
    }
}
