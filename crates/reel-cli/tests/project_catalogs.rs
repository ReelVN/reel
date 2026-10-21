//! Project languages share one bundler and keep code and translation identities separate.
use reel_cli::{
    BuildProfile, build, build_export,
    distribution::{self, ExportOptions, Mount},
    localization::{check_project, prepare_project},
    project,
};
use reel_package::{archive::ArchiveIndex, distribution::LooseResources};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
};

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn translation(archive: &Path, key: &str) -> String {
    let mut file = File::open(archive).unwrap();
    let index = ArchiveIndex::read(&mut file).unwrap();
    let entry = index.get(key).unwrap();
    file.seek(SeekFrom::Start(entry.offset)).unwrap();
    let mut text = String::new();
    file.take(entry.length).read_to_string(&mut text).unwrap();
    text
}

#[test]
fn project_check_reuses_fluent_namespace_and_source_diagnostics_without_writes() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(root, "translations/en/a.ftl", "-brand = Reel\n");
    write(
        root,
        "translations/en/ui/b.ftl",
        "opening = Welcome to { -brand }.\n",
    );
    check_project(root).unwrap();
    assert!(!root.join("build").exists());
    write(
        root,
        "translations/en/ui/b.ftl",
        "opening = Welcome.\nbrand = Duplicate\n",
    );
    let error = check_project(root)
        .unwrap_err()
        .to_string()
        .replace('\\', "/");
    assert!(error.contains("ui/b.ftl:2:1"), "{error}");
    assert!(error.contains("a.ftl:1:2"), "{error}");
    write(root, "translations/en/ui/b.ftl", "opening = { @ }\n");
    let error = check_project(root)
        .unwrap_err()
        .to_string()
        .replace('\\', "/");
    assert!(error.contains("ui/b.ftl:1:"), "{error}");
    assert!(!root.join("build").exists());
    let canonical = tempfile::tempdir().unwrap();
    write(
        canonical.path(),
        "translations/en-us/a.ftl",
        "opening = Hello\n",
    );
    assert!(
        check_project(canonical.path())
            .unwrap_err()
            .to_string()
            .contains("canonical tag en-US")
    );
}

#[test]
fn content_addressed_catalogs_preserve_old_asbs_and_bytecode_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(root, "src/main.aside", "fn main() { 1 }");
    write(root, "resources/marker", "original asset");
    write(
        root,
        "translations/en-US/ui/opening.ftl",
        "opening = Old localized body.\n",
    );
    let loaded = project::load(root).unwrap_or_else(|error| panic!("{:?}", error.failure));
    let artifact = build(&loaded.check().unwrap(), Some("main::main")).unwrap();
    let output = root.join("build/debug");
    let first = prepare_project(root, &output).unwrap();
    let archive = first.archive.as_ref().unwrap();
    let original_metadata = fs::metadata(archive).unwrap().modified().unwrap();
    let unchanged = prepare_project(root, &output).unwrap();
    assert_eq!(first.archive, unchanged.archive);
    assert_eq!(
        fs::metadata(archive).unwrap().modified().unwrap(),
        original_metadata
    );
    let old_bytes = distribution::with_catalogs(&artifact, &first).unwrap();
    let old = output.join("old.asb");
    fs::write(&old, &old_bytes).unwrap();
    let before = distribution::load(&old).unwrap();
    assert!(before.package.program().package().text_pool.is_none());
    assert_eq!(
        before.loose,
        Some(root.join("resources").canonicalize().unwrap())
    );
    assert_eq!(before.catalogs, first.catalogs);
    assert_eq!(
        before.package.distribution().unwrap().loose,
        Some(LooseResources::Project)
    );
    let identity = before.package.metadata().bytecode_blake3.clone();
    assert!(!String::from_utf8_lossy(&old_bytes).contains("Old localized body."));
    assert_eq!(
        translation(archive, &first.catalogs["en-US"]),
        "opening = Old localized body.\n\n\n"
    );
    let mut invalid = before.package.distribution().unwrap().clone();
    invalid
        .catalogs
        .insert("en-US".into(), "__reel/catalogs/missing.ftl".into());
    let program =
        aside_vm::bytecode::encode(before.package.program().package(), Default::default()).unwrap();
    let invalid_bytes = reel_package::encode(
        &program,
        before.package.metadata(),
        Some(&invalid),
        Default::default(),
    )
    .unwrap();
    let invalid_asb = output.join("missing-catalog.asb");
    fs::write(&invalid_asb, invalid_bytes).unwrap();
    assert!(
        distribution::load(&invalid_asb)
            .err()
            .unwrap()
            .to_string()
            .contains("text archive lacks indexed catalog en-US")
    );

    write(
        root,
        "translations/en-US/ui/opening.ftl",
        "opening = Updated localized body.\n",
    );
    let second = prepare_project(root, &output).unwrap();
    assert_ne!(first.archive, second.archive);
    let updated_bytes = distribution::with_catalogs(&artifact, &second).unwrap();
    // A failed ASB publication leaves both the previous ASB and its archive usable.
    assert!(reel_cli::output::write(&old, &loaded.input, &updated_bytes, false).is_err());
    assert_eq!(fs::read(&old).unwrap(), old_bytes);
    assert!(translation(archive, &first.catalogs["en-US"]).contains("Old localized body."));
    let updated = output.join("new.asb");
    reel_cli::output::write(&updated, &loaded.input, &updated_bytes, false).unwrap();
    let after = distribution::load(&updated).unwrap();
    assert_eq!(after.package.metadata().bytecode_blake3, identity);
    assert!(
        translation(second.archive.as_ref().unwrap(), &after.catalogs["en-US"])
            .contains("Updated localized body.")
    );
    assert_eq!(
        distribution::load(&old)
            .unwrap()
            .package
            .metadata()
            .bytecode_blake3,
        identity
    );
}

#[test]
fn untranslated_projects_create_no_archive_or_generated_directory() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let output = root.join("build/debug");
    check_project(root).unwrap();
    let prepared = prepare_project(root, &output).unwrap();
    assert!(prepared.catalogs.is_empty());
    assert!(prepared.archive.is_none());
    assert!(!output.exists());
    write(root, "translations/en/menu.ftl", "title = Generated\n");
    write(
        root,
        "resources/__reel/catalogs/en.ftl",
        "title = Conflicting\n",
    );
    assert!(
        check_project(root)
            .unwrap_err()
            .to_string()
            .contains("resource conflicts")
    );
    assert!(
        prepare_project(root, &output)
            .err()
            .unwrap()
            .to_string()
            .contains("resource conflicts")
    );
    assert!(!output.exists());
}

#[test]
fn exports_keep_editable_translations_and_reject_colliding_text_mounts() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(root, "src/main.aside", "fn main() { 1 }");
    write(
        root,
        "translations/en/ui/menu.ftl",
        "title = English menu\n",
    );
    write(root, "platform-player", "fixture player");
    let loaded = project::load(root).unwrap_or_else(|error| panic!("{:?}", error.failure));
    let checked = loaded.check().unwrap();
    let mut options = ExportOptions {
        output: root.join("build/export/debug"),
        overwrite: false,
        resources: Vec::new(),
        text: Vec::new(),
        pack_bytes: 4096.try_into().unwrap(),
        player: Some(root.join("platform-player")),
    };
    for profile in [BuildProfile::Debug, BuildProfile::Release] {
        options.output = root.join(format!("build/export/{}", profile.name()));
        let artifact = build_export(&checked, project::ENTRY, profile).unwrap();
        distribution::export(&loaded, &artifact, &options).unwrap();
        let decoded = distribution::load(&options.output.join("game.asb")).unwrap();
        assert!(
            translation(&options.output.join("game.rtp"), &decoded.catalogs["en"])
                .contains("title = English menu")
        );
        let editable = options.output.join("translations/en/ui/menu.ftl");
        assert_eq!(editable.exists(), profile == BuildProfile::Debug);
        if profile == BuildProfile::Debug {
            assert!(options.output.join("src/main.aside").is_file());
            assert!(options.output.join("resources").is_dir());
            let copied = project::load(&options.output)
                .unwrap_or_else(|error| panic!("{:?}", error.failure));
            copied.check().unwrap();
        }
        assert!(!options.output.join("__reel/catalogs/en.ftl").exists());
    }
    let original = fs::read(options.output.join("game.asb")).unwrap();
    write(
        root,
        "explicit/__reel/catalogs/en.ftl",
        "title = Colliding\n",
    );
    options.text = vec![Mount {
        prefix: ".".into(),
        directory: root.join("explicit"),
    }];
    options.overwrite = true;
    let artifact = build_export(&checked, project::ENTRY, BuildProfile::Release).unwrap();
    let error = distribution::export(&loaded, &artifact, &options)
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("conflicts") && error.contains("__reel/catalogs/en.ftl"),
        "{error}"
    );
    assert_eq!(fs::read(options.output.join("game.asb")).unwrap(), original);
    options.text.clear();
    options.output = root.join("translations/en/generated-export");
    let error = distribution::export(&loaded, &artifact, &options)
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("translation source directory"), "{error}");
    assert_eq!(
        fs::read_to_string(root.join("translations/en/ui/menu.ftl")).unwrap(),
        "title = English menu\n"
    );
    #[cfg(windows)]
    {
        options.output = root.join("SRC");
        let error = distribution::export(&loaded, &artifact, &options)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("input source file"), "{error}");
        assert_eq!(
            fs::read_to_string(root.join("src/main.aside")).unwrap(),
            "fn main() { 1 }"
        );

        write(root, "resources/retained", "original resource");
        fs::write(root.join("resources/game.asb"), &original).unwrap();
        options.resources = vec![Mount {
            prefix: ".".into(),
            directory: root.join("resources"),
        }];
        options.output = root.join("RESOURCES");
        let error = distribution::export(&loaded, &artifact, &options)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("overlaps a resource directory"), "{error}");
        assert_eq!(
            fs::read_to_string(root.join("resources/retained")).unwrap(),
            "original resource"
        );
        assert_eq!(fs::read(root.join("resources/game.asb")).unwrap(), original);
    }
}
