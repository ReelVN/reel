//! Execute relocated exports with the real catalog loader and resource service.
use reel_cli::{BuildProfile, build_export, distribution::{self, ExportOptions, Mount, TEXT_POOL_ENTRY}, project};
use reel_engine::{Epoch, InteractionView, Session, SessionId, aside::{SliceBudget, VmLimits}};
use reel_package::archive::{ArchiveIndex, ArchiveKind, ArchiveWriter};
use reel_player::{aside_session::{AsideSessionUi, StoryProgress}, assets::{AssetOptions, Assets}};
use std::{collections::BTreeMap, fs::{self, File}, path::Path, time::{Duration, Instant}};

fn read_line(path: &Path) -> String {
    let loaded = distribution::load(path).unwrap();
    let assets = Assets::packaged(AssetOptions { root: loaded.root, ..Default::default() }, loaded.packages, loaded.loose).unwrap();
    let story = reel_cli::story_from_package(loaded.package).unwrap();
    let mut driver = AsideSessionUi::from_compiled(Session::new(SessionId(1), Epoch(1)).unwrap(), story, "zh-CN", VmLimits::default()).unwrap().with_assets(assets);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "catalog/story did not complete");
        driver.ui().assets().maintain();
        driver.maintain_localization().unwrap();
        match driver.tick(SliceBudget::default(), 64, || 0).unwrap() {
            StoryProgress::Faulted(_) => panic!("{:?}", driver.fault().unwrap()),
            StoryProgress::HostFailed(_) => panic!("{:?}", driver.host_failure()),
            _ => {}
        }
        if let Some(InteractionView::Dialogue(dialogue)) = driver.ui().session().view().interaction {
            return dialogue.lines()[0].content().fallback().to_owned();
        }
        std::thread::yield_now();
    }
}

#[test]
fn development_and_release_relocate_and_release_uses_only_declared_packages() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let source = root.join("source");
    let library = root.join("library");
    let catalogs = root.join("catalogs");
    let resources = root.join("assets");
    for directory in [&source, &library, &catalogs, &resources] { fs::create_dir(directory).unwrap(); }
    let nested = source.join("nested");
    let unused = source.join("unused");
    fs::create_dir(&nested).unwrap();
    fs::create_dir(&unused).unwrap();
    fs::write(source.join("main.aside"), r#"
        use shared::opening::line;
        use local::message::opening_id;
        fn main() {
            localization.load("en", "locales/en.ftl");
            localization.set_locale("en");
            narrator(tr(line(), id: opening_id()));
        }
    "#).unwrap();
    fs::write(library.join("opening.aside"), "pub fn line() { \"只在文本池保存的正文。\" }").unwrap();
    fs::write(nested.join("message.aside"), "pub fn opening_id() { \"opening\" }").unwrap();
    fs::write(catalogs.join("en.ftl"), "opening = Packaged translation.\n").unwrap();
    fs::write(resources.join("sample.bin"), b"range-readable resource").unwrap();
    let modules = BTreeMap::from([("shared".into(), library), ("local".into(), nested), ("unused".into(), unused)]);
    let loaded = project::load_with_modules(&source, &modules).unwrap_or_else(|e| panic!("{:?}", e.failure));
    let checked = loaded.check().unwrap();
    let mut options = ExportOptions {
        output: root.join("development"), overwrite: false, entry: loaded.entry(None), modules,
        resources: vec![Mount { prefix:"assets".into(), directory:resources }],
        text: vec![Mount { prefix:"locales".into(), directory:catalogs }],
    };
    for (profile, name) in [(BuildProfile::Debug, "development"), (BuildProfile::Release, "release")] {
        options.output = root.join(name);
        let artifact = build_export(&checked, &options.entry, profile).unwrap();
        distribution::export(&loaded, &artifact, &options).unwrap();
        let relocated = root.join(format!("relocated-{name}"));
        fs::rename(&options.output, &relocated).unwrap();
        let manifest = distribution::read_manifest(&relocated.join("release.json")).unwrap();
        if profile == BuildProfile::Debug {
            let author = manifest.source.unwrap();
            let roots = author.modules.iter().map(|(key, value)| (key.clone(), relocated.join(value))).collect();
            let copied = project::load_with_modules(&relocated.join(author.entry), &roots).unwrap_or_else(|e| panic!("{:?}", e.failure));
            copied.check().unwrap();
            assert!(relocated.join("assets/sample.bin").exists());
        } else {
            assert!(manifest.source.is_none());
            assert!(manifest.loose.is_none());
            assert!(!relocated.join("script").exists());
            assert!(!relocated.join("locales").exists());
            assert!(!relocated.join("assets").exists());
            let decoded = distribution::load(&relocated.join("release.json")).unwrap();
            let program = decoded.package.program();
            assert!(program.package().sources.is_empty());
            assert!(program.package().constants.iter().all(|value| !matches!(value, aside_vm::bytecode::Const::Utf8(_))));
            let identity = decoded.package.metadata().bytecode_blake3.clone();
            // Stray files must not silently override a published translation.
            fs::create_dir(relocated.join("locales")).unwrap();
            fs::write(relocated.join("locales/en.ftl"), "opening = Wrong loose translation.\n").unwrap();
            assert_eq!(read_line(&relocated.join("release.json")), "Packaged translation.");
            drop(decoded);
            // A translation-only package rebuild keeps the original text pool
            // and execution identity, while changing the next evaluated text.
            let mut writer = ArchiveWriter::new(File::create(relocated.join("game.rtp")).unwrap(), ArchiveKind::Text).unwrap();
            writer.add(TEXT_POOL_ENTRY, &mut artifact.text_pool.as_ref().unwrap().as_bytes()).unwrap();
            writer.add("locales/en.ftl", &mut b"opening = Updated translation.\n".as_slice()).unwrap();
            writer.finish().unwrap();
            assert_eq!(distribution::load(&relocated.join("release.json")).unwrap().package.metadata().bytecode_blake3, identity);
            assert_eq!(read_line(&relocated.join("release.json")), "Updated translation.");
            fs::remove_file(relocated.join("game.rtp")).unwrap();
            assert!(distribution::load(&relocated.join("release.json")).is_err());
            continue;
        }
        assert_eq!(read_line(&relocated.join("release.json")), "Packaged translation.");
    }
    let archive = root.join("relocated-release/resources.rpk");
    let index = ArchiveIndex::read(&mut File::open(archive).unwrap()).unwrap();
    assert_eq!(index.get("assets/sample.bin").unwrap().length, 23);
}
