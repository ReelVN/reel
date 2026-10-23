//! Raw script data shares the image/media asset service and its IO budget.
use super::{AssetKind, Assets, LoadId};
use reel_core::identity::{AssetKey, ContentVersion, ResourceVersion};
use reel_engine::assets::{AssetData, AssetLoader};
use std::collections::BTreeMap;

/// Session-local request identities over the shared loose or packaged IO lane.
pub struct AssetDataLoader {
    assets: Assets,
    next: u64,
    pending: BTreeMap<u64, LoadId>,
}
impl AssetDataLoader {
    pub fn new(assets: Assets) -> Self {
        Self {
            assets,
            next: 0,
            pending: BTreeMap::new(),
        }
    }
}
impl AssetLoader for AssetDataLoader {
    fn maintain(&mut self) {
        self.assets.maintain();
    }
    fn read(&mut self, path: &str, version: Option<ContentVersion>) -> Result<u64, String> {
        let id = self
            .next
            .checked_add(1)
            .ok_or("asset data request identity exhausted")?;
        let load = if let Some(version) = version {
            self.assets.request_version(
                ResourceVersion {
                    key: AssetKey(path.into()),
                    version,
                },
                AssetKind::Bytes,
            )
        } else {
            self.assets.request(path, AssetKind::Bytes)
        };
        self.next = id;
        self.pending.insert(id, load);
        Ok(id)
    }
    fn poll(&mut self, id: u64) -> Option<Result<AssetData, String>> {
        let load = *self.pending.get(&id)?;
        match self.assets.poll_bytes(load) {
            Ok(None) => None,
            result => {
                self.pending.remove(&id);
                Some(result.map(|value| {
                    let (version, bytes) = value.expect("completed asset bytes");
                    AssetData {
                        version: version.version,
                        bytes,
                    }
                }))
            }
        }
    }
    fn cancel(&mut self, id: u64) {
        if let Some(load) = self.pending.remove(&id) {
            self.assets.cancel(load);
        }
    }
}
impl Drop for AssetDataLoader {
    fn drop(&mut self) {
        for (_, load) in std::mem::take(&mut self.pending) {
            self.assets.cancel(load);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::AssetOptions;
    use reel_package::archive::{ArchiveKind, ArchiveWriter};
    use std::{
        fs::File,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "reel-asset-data-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let root = self.0.canonicalize().unwrap();
            assert_eq!(
                root.parent().unwrap(),
                std::env::temp_dir().canonicalize().unwrap()
            );
            assert!(
                root.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("reel-asset-data-")
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    fn poll(loader: &mut AssetDataLoader, id: u64) -> Result<AssetData, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            loader.maintain();
            if let Some(result) = loader.poll(id) {
                return result;
            }
            assert!(Instant::now() < deadline, "asset data worker stalled");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn raw_data_uses_loose_and_packaged_io_with_version_and_budget_ownership() {
        let bytes: Vec<u8> = (0..8193).map(|i| (i % 256) as u8).collect();
        for packaged in [false, true] {
            let fixture = Fixture::new();
            std::fs::create_dir(fixture.0.join("data")).unwrap();
            std::fs::write(fixture.0.join("data/sample.bin"), &bytes).unwrap();
            let options = AssetOptions {
                root: fixture.0.clone(),
                input_bytes: 16 << 10,
                ..Default::default()
            };
            let assets = if packaged {
                let path = fixture.0.join("resources.rpk");
                let mut writer =
                    ArchiveWriter::new(File::create(&path).unwrap(), ArchiveKind::Resources)
                        .unwrap();
                writer
                    .add("data/sample.bin", &mut bytes.as_slice())
                    .unwrap();
                writer.finish().unwrap();
                Assets::packaged(options, [path], None).unwrap()
            } else {
                Assets::directory(options)
            };
            let mut loader = AssetDataLoader::new(assets.clone());
            let request = loader.read("data/sample.bin", None).unwrap();
            let result = poll(&mut loader, request).unwrap();
            assert_eq!(result.bytes.as_slice(), bytes);
            assert_eq!(assets.0.borrow().input.usage().bytes, bytes.len() as u64);
            let version = result.version;
            drop(result);
            assert_eq!(assets.0.borrow().input.usage().bytes, 0);
            let pinned = loader.read("data/sample.bin", Some(version)).unwrap();
            assert_eq!(poll(&mut loader, pinned).unwrap().bytes.as_slice(), bytes);
            let wrong = loader
                .read("data/sample.bin", Some(ContentVersion([7; 32])))
                .unwrap();
            assert!(poll(&mut loader, wrong).is_err());
            let missing = loader.read("missing.bin", None).unwrap();
            assert!(poll(&mut loader, missing).is_err());
            let cancelled = loader.read("data/sample.bin", None).unwrap();
            loader.maintain();
            loader.cancel(cancelled);
            assert!(loader.poll(cancelled).is_none());
            let deadline = Instant::now() + Duration::from_secs(10);
            while assets.pending_io() {
                assets.maintain();
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(assets.0.borrow().input.usage().bytes, 0);
            let limited = Assets::directory(AssetOptions {
                root: fixture.0.clone(),
                input_bytes: 1024,
                ..Default::default()
            });
            let mut limited_loader = AssetDataLoader::new(limited.clone());
            let over_budget = limited_loader.read("data/sample.bin", None).unwrap();
            let error = poll(&mut limited_loader, over_budget).err().unwrap();
            assert!(error.contains("configured capacity"), "{error}");
            assert_eq!(limited.0.borrow().input.usage().bytes, 0);
        }
    }

    #[test]
    fn asset_worker_wake_resumes_maintenance_beside_waiting_dialogue() {
        use crate::{aside_session::AsideSessionUi, story_runtime::StoryRuntime};
        use reel_core::{
            budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
            test_support::{MemoryAsset, MemoryAssetIo},
        };
        use reel_engine::{
            Epoch, Session, SessionId,
            aside::{SliceBudget, VmLimits},
        };
        use std::{
            cell::RefCell,
            rc::Rc,
            sync::{Arc, mpsc},
        };
        let limits = QueueLimits {
            max_requests: 32,
            max_bytes: 1 << 20,
        };
        let storage = BudgetPool::new(BudgetCategory::CpuInput, limits);
        let mut host = None;
        let assets = Assets::with_adapters(AssetOptions::default(), |wake| {
            let mut io = MemoryAssetIo::new(limits, limits, wake.clone());
            io.insert_asset(
                ResourceVersion {
                    key: AssetKey("data/sample.bin".into()),
                    version: ContentVersion([1; 32]),
                },
                MemoryAsset {
                    bytes: OwnedBytes::new(vec![7; 8193], storage.reserve(8193).unwrap()).unwrap(),
                    media_type: None,
                },
            )
            .unwrap();
            let io = Rc::new(RefCell::new(io));
            host = Some(io.clone());
            (
                Box::new(crate::images::tests::ControlledIo(io)),
                Box::new(super::super::ImageExecutor::new(limits, wake)),
            )
        });
        let source = r#"fn main() {
            stage.replace(text("waiting"));
            let request = assets.read("data/sample.bin");
            spawn(|| {
                let data = await request;
                if data.len() != 8193 || data[8192] != 7 { throw "asset bytes"; }
                stage.replace(text("loaded"));
            });
            narrator("waiting for input");
        }"#;
        let mut driver = AsideSessionUi::from_source(
            Session::new(SessionId(74), Epoch(1)).unwrap(),
            source,
            "asset_worker_wake",
            1,
            "main",
            "en",
            VmLimits::default(),
        )
        .unwrap()
        .with_assets(assets);
        let (send, receive) = mpsc::channel();
        driver.set_resource_waker(Arc::new(move || {
            let _ = send.send(());
        }));
        let mut story = StoryRuntime::new(driver);
        let clock = std::cell::Cell::new(0u64);
        let settle = |story: &mut StoryRuntime| {
            for _ in 0..10000 {
                if !story.needs_tick() {
                    return;
                }
                clock.set(clock.get() + 1_000_000_000);
                story.tick(SliceBudget::default(), || clock.get()).unwrap();
            }
            panic!("asset IO must sleep without a polling timer");
        };
        settle(&mut story);
        let root = story.driver().adapter().stage().root();
        assert!(root.is_some());
        assert!(story.driver().ui().session().view().interaction.is_some());
        assert!(story.driver().adapter().asset_reads_pending());
        let host = host.unwrap();
        for _ in 0..2 {
            assert!(host.borrow_mut().complete_next_read(), "describe then read");
            receive.recv_timeout(Duration::from_secs(1)).unwrap();
            story.wake_input();
            assert!(
                story.needs_tick(),
                "resource completion wakes beside dialogue"
            );
            settle(&mut story);
        }
        assert!(!story.driver().adapter().asset_reads_pending());
        assert_ne!(
            story.driver().adapter().stage().root(),
            root,
            "asset waiter resumed"
        );
        assert!(story.driver().ui().session().view().interaction.is_some());
    }
}
