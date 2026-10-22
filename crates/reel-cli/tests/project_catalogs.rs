//! Inline source translations share the seekable package protocol in every profile.
use reel_cli::{
    BuildProfile, build, build_export, build_with_profile,
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
fn source(body: &str) -> String {
    format!(
        r#"fn main() {{ localization.set_locale("en-US"); stage.replace(text(tr("原文", {{["en-US"]:"{body}"}}))); }}"#
    )
}

#[test]
fn project_check_reports_translation_source_diagnostics_without_writes() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(root, "src/main.aside", &source("Valid translation"));
    let loaded = project::load(root).unwrap_or_else(|e| panic!("{:?}", e.failure));
    let checked = loaded.check().unwrap();
    check_project(root, &checked.catalogs).unwrap();
    assert_eq!(checked.catalogs.catalogs.len(), 1);
    assert!(!root.join("build").exists());
    write(
        root,
        "src/main.aside",
        r#"fn main() { tr("原文", {en:42}); }"#,
    );
    let loaded = project::load(root).unwrap_or_else(|e| panic!("{:?}", e.failure));
    let failure = loaded
        .check()
        .err()
        .expect("invalid inline translation accepted");
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.source.as_deref() == Some("src/main.aside")
                && d.diagnostic.range.start() < d.diagnostic.range.end()),
        "{failure:?}"
    );
    assert!(!root.join("build").exists());
}

#[test]
fn content_addressed_catalogs_preserve_old_asbs_and_capture_source_revisions() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(root, "src/main.aside", &source("Old localized body."));
    write(root, "resources/marker", "original asset");
    let loaded = project::load(root).unwrap_or_else(|e| panic!("{:?}", e.failure));
    let checked = loaded.check().unwrap();
    let artifact =
        build_with_profile(&checked, Some(project::ENTRY), BuildProfile::Release).unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &checked.catalogs,
        &artifact.catalogs
    ));
    let output = root.join("build/debug");
    let first = prepare_project(root, &artifact.catalogs, &output).unwrap();
    let archive = first.archive.as_ref().unwrap();
    let modified = fs::metadata(archive).unwrap().modified().unwrap();
    let unchanged = prepare_project(root, &artifact.catalogs, &output).unwrap();
    assert_eq!(first.archive, unchanged.archive);
    assert_eq!(fs::metadata(archive).unwrap().modified().unwrap(), modified);
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
    assert!(translation(archive, &first.catalogs["en-US"]).contains("Old localized body."));
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
    write(root, "src/main.aside", &source("Updated localized body."));
    let loaded = project::load(root).unwrap_or_else(|e| panic!("{:?}", e.failure));
    let next = build_with_profile(
        &loaded.check().unwrap(),
        Some(project::ENTRY),
        BuildProfile::Release,
    )
    .unwrap();
    let second = prepare_project(root, &next.catalogs, &output).unwrap();
    assert_ne!(first.archive, second.archive);
    let updated_bytes = distribution::with_catalogs(&next, &second).unwrap();
    assert!(reel_cli::output::write(&old, &loaded.input, &updated_bytes, false).is_err());
    assert_eq!(fs::read(&old).unwrap(), old_bytes);
    let updated = output.join("new.asb");
    reel_cli::output::write(&updated, &loaded.input, &updated_bytes, false).unwrap();
    let after = distribution::load(&updated).unwrap();
    assert_ne!(after.package.metadata().bytecode_blake3, identity);
    assert_eq!(
        distribution::load(&old)
            .unwrap()
            .package
            .metadata()
            .bytecode_blake3,
        identity
    );
    assert_eq!(
        before
            .package
            .program()
            .package()
            .functions
            .iter()
            .map(|f| &f.code)
            .collect::<Vec<_>>(),
        after
            .package
            .program()
            .package()
            .functions
            .iter()
            .map(|f| &f.code)
            .collect::<Vec<_>>()
    );
    assert!(
        translation(second.archive.as_ref().unwrap(), &after.catalogs["en-US"])
            .contains("Updated localized body.")
    );
    assert!(translation(archive, &first.catalogs["en-US"]).contains("Old localized body."));
}

#[test]
fn untranslated_projects_create_no_archive_and_generated_keys_reject_resources() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let output = root.join("build/debug");
    write(root, "src/main.aside", "fn main() {1}");
    let checked = project::load(root)
        .unwrap_or_else(|e| panic!("{:?}", e.failure))
        .check()
        .unwrap();
    check_project(root, &checked.catalogs).unwrap();
    let prepared = prepare_project(root, &checked.catalogs, &output).unwrap();
    assert!(prepared.catalogs.is_empty() && prepared.archive.is_none());
    assert!(!output.exists());
    write(root, "src/main.aside", &source("Translation"));
    let checked = project::load(root)
        .unwrap_or_else(|e| panic!("{:?}", e.failure))
        .check()
        .unwrap();
    write(
        root,
        "resources/__reel/catalogs/en-US.ftl",
        "colliding resource",
    );
    assert!(
        check_project(root, &checked.catalogs)
            .unwrap_err()
            .to_string()
            .contains("resource conflicts")
    );
    assert!(
        prepare_project(root, &checked.catalogs, &output)
            .err()
            .unwrap()
            .to_string()
            .contains("resource conflicts")
    );
    assert!(!output.exists());
}

#[test]
fn exports_keep_inline_translations_in_editable_source_and_reject_collisions() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let code = source("English menu");
    write(root, "src/main.aside", &code);
    write(root, "platform-player", "fixture player");
    let loaded = project::load(root).unwrap_or_else(|e| panic!("{:?}", e.failure));
    let checked = loaded.check().unwrap();
    let mut options = ExportOptions {
        output: root.join("build/export/debug"),
        overwrite: false,
        resources: vec![],
        text: vec![],
        pack_bytes: 4096.try_into().unwrap(),
        player: Some(root.join("platform-player")),
    };
    for profile in [BuildProfile::Debug, BuildProfile::Release] {
        options.output = root.join(format!("build/export/{}", profile.name()));
        let artifact = build_export(&checked, project::ENTRY, profile).unwrap();
        distribution::export(&loaded, &artifact, &options).unwrap();
        let decoded = distribution::load(&options.output.join("game.asb")).unwrap();
        assert!(
            translation(&options.output.join("game.rtp"), &decoded.catalogs["en-US"])
                .contains("English menu")
        );
        assert_eq!(
            options.output.join("src/main.aside").exists(),
            profile == BuildProfile::Debug
        );
        if profile == BuildProfile::Debug {
            assert_eq!(
                fs::read_to_string(options.output.join("src/main.aside")).unwrap(),
                code
            );
            let copied =
                project::load(&options.output).unwrap_or_else(|e| panic!("{:?}", e.failure));
            let copied = copied.check().unwrap();
            assert_eq!(copied.catalogs.catalogs, checked.catalogs.catalogs);
        }
        assert!(!options.output.join("translations").exists());
        assert!(!options.output.join("__reel/catalogs/en-US.ftl").exists());
    }
    let original = fs::read(options.output.join("game.asb")).unwrap();
    write(root, "explicit/__reel/catalogs/en-US.ftl", "collision");
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
        error.contains("conflicts") && error.contains("__reel/catalogs/en-US.ftl"),
        "{error}"
    );
    assert_eq!(fs::read(options.output.join("game.asb")).unwrap(), original);
    options.text.clear();
    options.output = root.join("src");
    assert!(
        distribution::export(&loaded, &artifact, &options)
            .err()
            .unwrap()
            .to_string()
            .contains("input source file")
    );
    assert_eq!(
        fs::read_to_string(root.join("src/main.aside")).unwrap(),
        code
    );
    write(root, "resources/retained", "original resource");
    fs::write(root.join("resources/game.asb"), &original).unwrap();
    options.resources = vec![Mount {
        prefix: ".".into(),
        directory: root.join("resources"),
    }];
    options.output = root.join("resources");
    assert!(
        distribution::export(&loaded, &artifact, &options)
            .err()
            .unwrap()
            .to_string()
            .contains("overlaps a resource directory")
    );
    assert_eq!(
        fs::read_to_string(root.join("resources/retained")).unwrap(),
        "original resource"
    );
    assert_eq!(fs::read(root.join("resources/game.asb")).unwrap(), original);
    let _ = build(&checked, Some(project::ENTRY)).unwrap();
}
