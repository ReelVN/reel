//! Catalog IO shares author assets; parsing uses the existing bounded resource worker.
use crate::assets::{AssetKind, Assets, LoadId, worker::Worker};
use reel_core::{budget::OwnedBytes, identity::Priority};
use reel_engine::localization::{Catalog, CatalogLoader};
use std::{collections::BTreeMap, sync::Arc};

/// Validated canonical language tags mapped to logical catalog asset keys.
/// Sessions and their loaders share this immutable index across replacements.
pub type CatalogIndex = Arc<BTreeMap<String, String>>;

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
    Done(Result<Box<Catalog>, String>),
}
pub struct AssetCatalogLoader {
    assets: Assets,
    catalogs: CatalogIndex,
    worker: Worker<Parse, Parsed>,
    pending: BTreeMap<u64, Pending>,
    events: Vec<Parsed>,
    next: u64,
}
impl AssetCatalogLoader {
    pub fn new(assets: Assets, catalogs: CatalogIndex) -> Self {
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
            catalogs,
            worker,
            pending: BTreeMap::new(),
            events: Vec::new(),
            next: 0,
        }
    }
}
impl CatalogLoader for AssetCatalogLoader {
    fn resolve(&self, locale: &str) -> Option<&str> {
        self.catalogs.get(locale).map(String::as_str)
    }

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
                *pending = Pending::Done(event.result.map(Box::new));
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
            Some(result.map(|catalog| *catalog))
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
    use super::CatalogIndex;
    use crate::{
        aside_session::{AsideSessionUi, StoryState},
        assets::{AssetOptions, Assets},
        story_runtime::StoryRuntime,
    };
    use reel_engine::{
        Epoch, Session, SessionId,
        aside::{SliceBudget, VmLimits},
    };
    use std::{fs, path::Path, sync::Arc};

    #[test]
    fn indexed_archives_select_exact_and_base_catalogs_across_new_sessions() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/indexed-catalog-player");
        fs::create_dir_all(&root).unwrap();
        let temporary = tempfile_directory(&root);
        let script = r#"
            fn greeting(){tr("source",{en:"Base English",["en-US"]:"US English",fr:"Français"})}
            fn common(){tr("common",{en:"Base fallback"})}
            fn main() {
                localization.set_locale("en");
                if greeting()!="Base English" {throw "initial indexed catalog";}
                game.start(next);
            }
            fn next() {
                localization.set_locale("en-US");
                if greeting()!="US English" {throw "exact indexed catalog";}
                if common()!="Base fallback" {throw "base indexed catalog";}
                localization.set_locale("fr");
                if greeting()!="Français" {throw "new session indexed catalog";}
                localization.set_locale("de");
                if greeting()!="source" {throw "missing catalog fallback";}
            }
        "#;
        let mut story =
            reel_engine::aside::source::compile_story(script, "indexed", 1, "main").unwrap();
        let archive = temporary.join("languages.rtp");
        let mut writer = reel_package::archive::ArchiveWriter::new(
            fs::File::create(&archive).unwrap(),
            reel_package::archive::ArchiveKind::Text,
        )
        .unwrap();
        let catalogs: CatalogIndex = Arc::new(
            story
                .catalogs
                .catalogs
                .keys()
                .map(|locale| (locale.clone(), format!("__reel/catalogs/{locale}.ftl")))
                .collect(),
        );
        for (locale, text) in &story.catalogs.catalogs {
            writer.add(&catalogs[locale], &mut text.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        story.catalogs = Default::default();
        let assets = Assets::packaged(
            AssetOptions {
                root: temporary.clone(),
                ..Default::default()
            },
            [archive],
            None,
        )
        .unwrap();
        let driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(73), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap()
        .with_asset_context(assets, catalogs.clone());
        let mut runtime = StoryRuntime::new(driver);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while runtime.driver().state() != StoryState::Finished || runtime.needs_tick() {
            assert!(
                std::time::Instant::now() < deadline,
                "indexed runtime stalled"
            );
            runtime.tick(SliceBudget::default(), || 0).unwrap();
            assert!(
                matches!(
                    runtime.driver().state(),
                    StoryState::Running | StoryState::Finished
                ),
                "indexed runtime failed: {:?} {:?}",
                runtime.driver().state(),
                runtime.driver().fault().unwrap()
            );
            if runtime.driver().adapter().localization_pending() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        assert_eq!(runtime.driver().ui().session().identity().epoch, Epoch(2));
        assert!(Arc::ptr_eq(&catalogs, &runtime.driver().catalogs()));
        drop(runtime);
        fs::remove_dir_all(temporary).unwrap();
    }

    fn tempfile_directory(root: &Path) -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let directory = root.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        directory
    }

    #[test]
    fn source_catalogs_survive_asset_replacement_and_new_story_sessions() {
        let script = r#"
            fn greeting(){tr("原文",{en:"Source English",fr:"Français"})}
            fn main(){
                localization.set_locale("en-US");
                if greeting()!="Source English" {throw "source base fallback";}
                game.start(next);
            }
            fn next(){
                localization.set_locale("fr");
                if greeting()!="Français" {throw "source index lost on restart";}
                localization.set_locale("de");
                if greeting()!="原文" {throw "original fallback";}
            }
        "#;
        let driver = AsideSessionUi::from_source(
            Session::new(SessionId(74), Epoch(1)).unwrap(),
            script,
            "source_languages",
            1,
            "main",
            "zh-CN",
            VmLimits::default(),
        )
        .unwrap()
        .with_asset_options(AssetOptions::default());
        let mut runtime = StoryRuntime::new(driver);
        for step in 0..10_000 {
            assert!(step < 9_999, "source catalog runtime stalled");
            runtime.tick(SliceBudget::default(), || 0).unwrap();
            assert!(
                matches!(
                    runtime.driver().state(),
                    StoryState::Running | StoryState::Finished
                ),
                "{:?}",
                runtime.driver().fault().unwrap()
            );
            if runtime.driver().state() == StoryState::Finished && !runtime.needs_tick() {
                break;
            }
        }
        assert_eq!(runtime.driver().ui().session().identity().epoch, Epoch(2));
    }

    #[test]
    fn restored_source_locale_resolves_before_reactive_projection_and_later_switches() {
        use reel_engine::{
            SessionIdentity,
            aside::{
                SessionVm,
                snapshot::{SessionSnapshot, SnapshotBudget},
            },
        };
        use reel_ui::stage::NodeKind;
        let script = r#"fn main(){
            localization.set_locale("en");
            let translated=derive(||tr("原文",{en:"English",fr:"Français"}));
            stage.replace(column{text(translated);button("French",on_click:||localization.set_locale("fr"));});
        }"#;
        let driver = AsideSessionUi::from_source(
            Session::new(SessionId(75), Epoch(1)).unwrap(),
            script,
            "source_restore",
            1,
            "main",
            "zh-CN",
            VmLimits::default(),
        )
        .unwrap();
        let mut runtime = StoryRuntime::new(driver);
        for _ in 0..1000 {
            runtime.tick(SliceBudget::default(), || 0).unwrap();
            if runtime.driver().state() == StoryState::Finished && !runtime.needs_tick() {
                break;
            }
        }
        let live = runtime.driver();
        let mut capture = live.snapshot_begin().unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let mut image =
            SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            live.adapter().vm().program().clone(),
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(75),
                epoch: Epoch(2),
            },
            Default::default(),
            None,
            Some(live.catalog_loader()),
        )
        .unwrap()
        .with_source_catalogs(live.adapter().source_catalogs())
        .unwrap()
        .with_effect_products(live.adapter().effect_products())
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut session, mut vm) = restore.finish().unwrap();
        vm.sync_stage(&session).unwrap();
        let button=vm.stage().nodes().find(|node|matches!(&node.kind,NodeKind::Button{label:Some(label),..} if label=="French")).unwrap().id;
        assert!(
            vm.stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("English".into()))
        );
        assert!(vm.click(&mut session, button).unwrap());
        let mut restored = AsideSessionUi::restored(
            session,
            vm,
            "zh-CN".into(),
            live.ui().assets(),
            live.initializer(),
            live.catalogs(),
        )
        .unwrap();
        while !restored.prepare_restored(SliceBudget::default()).unwrap() {}
        let mut runtime = StoryRuntime::new(restored);
        runtime.wake_input();
        for _ in 0..1000 {
            runtime.tick(SliceBudget::default(), || 0).unwrap();
            if !runtime.needs_tick() {
                break;
            }
        }
        assert!(
            runtime
                .driver()
                .adapter()
                .stage()
                .nodes()
                .any(|node| node.kind == NodeKind::Text("Français".into()))
        );
    }

    #[test]
    fn localization_resource_wait_wakes_runtime_and_then_becomes_idle() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/source-catalog-player");
        fs::create_dir_all(&root).unwrap();
        let temporary = tempfile_directory(&root);
        let script = r#"
        fn phrase(){tr("雨停了。",{en:"The rain stopped."})}
        fn main() {
            localization.load("en", "en.ftl");localization.set_locale("en");
            if phrase()!="The rain stopped." {throw "external catalog";}
            let mut failed=false;
            try {localization.load("en","missing.ftl");}catch error{failed=true;}
            if !failed || phrase()!="The rain stopped." {throw "failed load changed catalog";}
        }"#;
        let mut story =
            reel_engine::aside::source::compile_story(script, "localized", 1, "main").unwrap();
        fs::write(temporary.join("en.ftl"), &story.catalogs.catalogs["en"]).unwrap();
        story.catalogs = Default::default();
        let driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(72), Epoch(1)).unwrap(),
            story,
            "zh-CN",
            VmLimits::default(),
        )
        .unwrap()
        .with_asset_options(AssetOptions {
            root: temporary.clone(),
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
        drop(runtime);
        fs::remove_dir_all(temporary).unwrap();
    }
}
