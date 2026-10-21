//! Execute relocated exports with the real catalog loader and resource service.
use reel_cli::{
    BuildProfile, build_export,
    distribution::{self, ExportOptions, Mount, TEXT_POOL_ENTRY},
    project,
};
use reel_engine::{
    Epoch, InteractionView, Session, SessionId,
    aside::{SliceBudget, VmLimits},
};
use reel_package::archive::{ArchiveIndex, ArchiveKind, ArchiveWriter};
use reel_player::{
    aside_session::{AsideSessionUi, StoryProgress},
    assets::{AssetOptions, Assets},
};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::{Duration, Instant},
};

fn read_line(path: &Path) -> String {
    let loaded = distribution::load(path).unwrap();
    let assets = Assets::packaged(
        AssetOptions {
            root: loaded.root,
            ..Default::default()
        },
        loaded.packages,
        loaded.loose,
    )
    .unwrap();
    let story = reel_cli::story_from_package(loaded.package).unwrap();
    let mut driver = AsideSessionUi::from_compiled(
        Session::new(SessionId(1), Epoch(1)).unwrap(),
        story,
        "zh-CN",
        VmLimits::default(),
    )
    .unwrap()
    .with_assets(assets);
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
        if let Some(InteractionView::Dialogue(dialogue)) = driver.ui().session().view().interaction
        {
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
    let scripts = source.join("src");
    let library = scripts.join("shared");
    let catalogs = root.join("catalogs");
    let resources = root.join("assets");
    let player = root.join("platform-player");
    fs::write(&player, b"platform player fixture").unwrap();
    for directory in [&scripts, &library, &catalogs, &resources] {
        fs::create_dir_all(directory).unwrap();
    }
    let nested = scripts.join("local");
    let unused = scripts.join("unused");
    fs::create_dir(&nested).unwrap();
    fs::create_dir(&unused).unwrap();
    fs::write(unused.join("unused.aside"), "fn invalid(").unwrap();
    fs::write(
        scripts.join("main.aside"),
        r#"
        use game::shared::opening::line;
        use game::local::message::opening_id;
        fn main() {
            localization.load("en", "locales/en.ftl");
            localization.set_locale("en");
            narrator(tr(line(), id: opening_id()));
        }
    "#,
    )
    .unwrap();
    fs::write(
        library.join("opening.aside"),
        "pub fn line() { \"只在文本池保存的正文。\" }",
    )
    .unwrap();
    fs::write(
        nested.join("message.aside"),
        "pub fn opening_id() { \"opening\" }",
    )
    .unwrap();
    fs::write(catalogs.join("en.ftl"), "opening = Packaged translation.\n").unwrap();
    fs::write(resources.join("sample.bin"), b"range-readable resource").unwrap();
    fs::write(resources.join("00-small.bin"), [1; 17]).unwrap();
    fs::write(resources.join("01-large.bin"), [2; 1024]).unwrap();
    let loaded = project::load(&source).unwrap_or_else(|e| panic!("{:?}", e.failure));
    let checked = loaded.check().unwrap();
    let mut options = ExportOptions {
        output: root.join("development"),
        overwrite: false,
        pack_bytes: std::num::NonZeroU64::new(128).unwrap(),
        player: Some(player),
        resources: vec![Mount {
            prefix: "assets".into(),
            directory: resources.clone(),
        }],
        text: vec![Mount {
            prefix: "locales".into(),
            directory: catalogs,
        }],
    };
    for (profile, name) in [
        (BuildProfile::Debug, "development"),
        (BuildProfile::Release, "release"),
    ] {
        options.output = root.join(name);
        let artifact = build_export(&checked, project::ENTRY, profile).unwrap();
        distribution::export(&loaded, &artifact, &options).unwrap();
        let relocated = root.join(format!("relocated-{name}"));
        fs::rename(&options.output, &relocated).unwrap();
        assert!(!relocated.join("release.json").exists());
        let manifest = distribution::read_manifest(&relocated.join("game.asb")).unwrap();
        if profile == BuildProfile::Debug {
            let copied = project::load(&relocated).unwrap_or_else(|e| panic!("{:?}", e.failure));
            copied.check().unwrap();
            assert!(relocated.join("src/shared/opening.aside").exists());
            assert!(relocated.join("src/local/message.aside").exists());
            assert!(!relocated.join("src/unused").exists());
            assert!(relocated.join("resources/assets/sample.bin").exists());
            assert_eq!(
                manifest.loose,
                Some(reel_package::distribution::LooseResources::Directory(
                    "resources".into()
                ))
            );
            // dev follows the relocated editable source; run still uses the ASB.
            fs::write(relocated.join("src/main.aside"), "fn main(").unwrap();
            let failed = std::process::Command::new(env!("CARGO_BIN_EXE_reel"))
                .arg("dev")
                .current_dir(&relocated)
                .output()
                .unwrap();
            assert!(!failed.status.success());
            assert!(
                String::from_utf8_lossy(&failed.stderr).contains("main.aside"),
                "{}",
                String::from_utf8_lossy(&failed.stderr)
            );
        } else {
            assert_eq!(
                fs::read(relocated.join(distribution::exported_player_name())).unwrap(),
                b"platform player fixture"
            );
            assert!(manifest.loose.is_none());
            assert!(!relocated.join("src").exists());
            assert!(!relocated.join("locales").exists());
            assert!(!relocated.join("assets").exists());
            assert_eq!(
                manifest.resources,
                ["resources.rpk", "resources-002.rpk", "resources-003.rpk"]
            );
            for name in &manifest.resources {
                let mut file = File::open(relocated.join(name)).unwrap();
                let index = ArchiveIndex::read(&mut file).unwrap();
                assert_eq!(index.entries().len(), 1);
                for entry in index.entries() {
                    if entry.length > options.pack_bytes.get() {
                        assert_eq!(entry.path, "assets/01-large.bin");
                    } else {
                        assert!(file.metadata().unwrap().len() <= options.pack_bytes.get());
                    }
                    file.seek(SeekFrom::Start(entry.offset)).unwrap();
                    let mut content = vec![];
                    (&mut file)
                        .take(entry.length)
                        .read_to_end(&mut content)
                        .unwrap();
                    assert_eq!(
                        content,
                        fs::read(resources.join(entry.path.strip_prefix("assets/").unwrap()))
                            .unwrap()
                    );
                }
            }
            let decoded = distribution::load(&relocated.join("game.asb")).unwrap();
            let program = decoded.package.program();
            assert!(program.package().sources.is_empty());
            assert!(
                program
                    .package()
                    .constants
                    .iter()
                    .all(|value| !matches!(value, aside_vm::bytecode::Const::Utf8(_)))
            );
            let identity = decoded.package.metadata().bytecode_blake3.clone();
            // Stray files must not silently override a published translation.
            fs::create_dir(relocated.join("locales")).unwrap();
            fs::write(
                relocated.join("locales/en.ftl"),
                "opening = Wrong loose translation.\n",
            )
            .unwrap();
            assert_eq!(
                read_line(&relocated.join("game.asb")),
                "Packaged translation."
            );
            drop(decoded);
            // A translation-only package rebuild keeps the original text pool
            // and execution identity, while changing the next evaluated text.
            let mut writer = ArchiveWriter::new(
                File::create(relocated.join("game.rtp")).unwrap(),
                ArchiveKind::Text,
            )
            .unwrap();
            writer
                .add(
                    TEXT_POOL_ENTRY,
                    &mut artifact.text_pool.as_ref().unwrap().as_bytes(),
                )
                .unwrap();
            writer
                .add(
                    "locales/en.ftl",
                    &mut b"opening = Updated translation.\n".as_slice(),
                )
                .unwrap();
            writer.finish().unwrap();
            assert_eq!(
                distribution::load(&relocated.join("game.asb"))
                    .unwrap()
                    .package
                    .metadata()
                    .bytecode_blake3,
                identity
            );
            assert_eq!(
                read_line(&relocated.join("game.asb")),
                "Updated translation."
            );
            fs::remove_file(relocated.join("game.rtp")).unwrap();
            assert!(distribution::load(&relocated.join("game.asb")).is_err());
            continue;
        }
        assert_eq!(
            read_line(&relocated.join("game.asb")),
            "Packaged translation."
        );
    }
    let archive = root.join("relocated-release/resources-003.rpk");
    let index = ArchiveIndex::read(&mut File::open(archive).unwrap()).unwrap();
    assert_eq!(index.get("assets/sample.bin").unwrap().length, 23);
}

#[test]
fn failed_player_copy_preserves_previous_export_and_overwrite_requires_embedded_manifest() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let source = root.join("source");
    fs::create_dir_all(source.join("src")).unwrap();
    fs::write(
        source.join("src/main.aside"),
        "fn main() { narrator(\"original\"); }",
    )
    .unwrap();
    let loaded = project::load(&source).unwrap_or_else(|error| panic!("{:?}", error.failure));
    let artifact = build_export(
        &loaded.check().unwrap(),
        "main::main",
        BuildProfile::Release,
    )
    .unwrap();
    let player = root.join("player");
    fs::write(&player, "player fixture").unwrap();
    let mut options = ExportOptions {
        output: root.join("release"),
        overwrite: false,
        pack_bytes: std::num::NonZeroU64::new(1024).unwrap(),
        resources: vec![],
        text: vec![],
        player: Some(player),
    };
    distribution::export(&loaded, &artifact, &options).unwrap();
    let original = fs::read(options.output.join("game.asb")).unwrap();
    options.overwrite = true;
    options.player = Some(root.join("missing-player"));
    assert!(distribution::export(&loaded, &artifact, &options).is_err());
    assert_eq!(fs::read(options.output.join("game.asb")).unwrap(), original);
    assert_eq!(read_line(&options.output.join("game.asb")), "original");
    let legacy = root.join("legacy");
    fs::create_dir(&legacy).unwrap();
    fs::write(legacy.join("release.json"), r#"{"code":"game.asb"}"#).unwrap();
    options.output = legacy.clone();
    assert!(distribution::export(&loaded, &artifact, &options).is_err());
    assert!(legacy.join("release.json").exists());
}

#[cfg(feature = "window")]
#[test]
#[ignore = "requires a native window and GPU; launches the exported executable"]
fn exported_player_launches_beside_its_executable_from_foreign_cwd() {
    use reel_player::saves::{DirectorySaves, SaveStorage};
    use std::process::{Command, Stdio};
    fn run(mut command: Command) {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "player did not exit: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let source = root.join("source");
    fs::create_dir_all(source.join("src")).unwrap();
    fs::write(
        source.join("src/main.aside"),
        r#"fn main() {
        game.configure(width: 320, height: 180, fonts: ["fonts/400.ttf"]);
        stage.replace(text("A", font_size: 32));
        await stage.presented();
        await persistent.write("launched", true);
        game.exit();
    }"#,
    )
    .unwrap();
    let output = root.join("export");
    let fonts =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../reel-player/tests/fixtures/font-weight");
    let exported = Command::new(env!("CARGO_BIN_EXE_reel"))
        .arg("export")
        .current_dir(&source)
        .arg("--release")
        .arg("--output")
        .arg(&output)
        .arg("--resource")
        .arg(format!("fonts={}", fonts.display()))
        .output()
        .unwrap();
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let relocated = root.join("moved-game");
    fs::rename(&output, &relocated).unwrap();
    let cwd = root.join("unrelated");
    fs::create_dir(&cwd).unwrap();
    fs::write(cwd.join("game.asb"), "this is not the game").unwrap();
    let executable = relocated.join(distribution::exported_player_name());
    let mut default = Command::new(&executable);
    default.current_dir(&cwd);
    run(default);
    assert!(
        DirectorySaves::new(relocated.join("saves"), 1 << 20)
            .read_data("launched")
            .unwrap()
            .is_some()
    );
    assert!(!cwd.join("saves").exists());
    let custom = root.join("custom-saves");
    let mut overridden = Command::new(&executable);
    overridden
        .current_dir(&cwd)
        .arg("--save-directory")
        .arg(&custom);
    run(overridden);
    assert!(
        DirectorySaves::new(&custom, 1 << 20)
            .read_data("launched")
            .unwrap()
            .is_some()
    );
    let cli_saves = root.join("cli-saves");
    let mut cli = Command::new(env!("CARGO_BIN_EXE_reel"));
    cli.current_dir(&cwd)
        .arg("run")
        .arg(relocated.join("game.asb"))
        .arg("--save-directory")
        .arg(&cli_saves);
    run(cli);
    assert!(
        DirectorySaves::new(cli_saves, 1 << 20)
            .read_data("launched")
            .unwrap()
            .is_some()
    );
    let names: Vec<_> = fs::read_dir(&relocated)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!names.iter().any(|name| name == "release.json"
        || name.ends_with(".cmd")
        || name == "src"
        || name == "fonts"));
}
