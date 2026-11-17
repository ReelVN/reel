//! Rebuild validation code while retaining a frozen distribution's resource input.
use reel_package::{
    archive::{ArchiveKind, ArchiveWriter},
    distribution::TEXT_POOL_ENTRY,
};
use std::{fs, path::PathBuf};

#[test]
#[ignore = "requires REEL_FIXTURE_PROJECT, REEL_FIXTURE_TEMPLATE and a fresh REEL_FIXTURE_OUTPUT"]
fn rebuild_code_with_frozen_resource_archives() {
    let path = |name| PathBuf::from(std::env::var_os(name).expect(name));
    let project = path("REEL_FIXTURE_PROJECT");
    let template = path("REEL_FIXTURE_TEMPLATE");
    let output = path("REEL_FIXTURE_OUTPUT");
    let loaded =
        reel_cli::project::load(&project).unwrap_or_else(|error| panic!("{:?}", error.failure));
    let target =
        std::env::var("REEL_FIXTURE_TARGET").unwrap_or_else(|_| std::env::consts::OS.into());
    let checked = loaded.check_with_progress(&target, &mut |_| {}).unwrap();
    let artifact = reel_cli::build_export(
        &checked,
        reel_cli::project::ENTRY,
        reel_cli::BuildProfile::Release,
    )
    .unwrap();
    let limits = reel_package::Limits::default();
    let original = reel_package::decode(
        &fs::read(&template).unwrap(),
        reel_engine::aside::source::types(),
        limits,
    )
    .unwrap();
    let package =
        reel_package::decode(&artifact.bytes, reel_engine::aside::source::types(), limits).unwrap();
    let identities = |effects: &reel_package::EffectProducts| {
        let mut identities = effects
            .definitions
            .iter()
            .map(|definition| definition.id)
            .collect::<Vec<_>>();
        identities.sort_unstable();
        identities
    };
    assert_eq!(
        identities(original.effects()),
        identities(package.effects()),
        "frozen resources require identical effect definitions"
    );
    let mut manifest = original.distribution().unwrap().clone();
    assert_eq!(
        manifest.catalogs.keys().collect::<Vec<_>>(),
        artifact.catalogs.catalogs.keys().collect::<Vec<_>>()
    );
    manifest.text = Some("game.rtp".into());
    manifest.loose = None;
    fs::create_dir(&output).expect("fixture output must be fresh");
    let mut disassembly = String::new();
    aside_vm::bytecode::disassemble(&checked.compilation.program, &mut disassembly).unwrap();
    fs::write(output.join("source-disassembly.txt"), &disassembly).unwrap();
    disassembly.clear();
    aside_vm::bytecode::disassemble(package.program(), &mut disassembly).unwrap();
    fs::write(output.join("release-disassembly.txt"), &disassembly).unwrap();
    let mut writer = ArchiveWriter::new(
        fs::File::create(output.join("game.rtp")).unwrap(),
        ArchiveKind::Text,
    )
    .unwrap();
    writer
        .add(
            TEXT_POOL_ENTRY,
            &mut artifact.text_pool.as_ref().unwrap().as_bytes(),
        )
        .unwrap();
    for (locale, key) in &manifest.catalogs {
        writer
            .add(key, &mut artifact.catalogs.catalogs[locale].as_bytes())
            .unwrap();
    }
    writer.finish().unwrap().sync_all().unwrap();
    let bytecode = package.program().encode(limits.bytecode).unwrap();
    let bytes = reel_package::encode(
        &bytecode,
        package.metadata(),
        Some(&manifest),
        package.effects(),
        package.resume().table(),
        reel_engine::aside::source::types(),
        limits,
    )
    .unwrap();
    fs::write(output.join("game.asb"), &bytes).unwrap();
    eprintln!(
        "fixture ASB={}, blake3={}, frozen resources={:?}",
        output.join("game.asb").display(),
        reel_package::digest(&bytes),
        manifest.resources
    );
}
