//! Prepared effects travel through the same archives as other game resources.
use reel_cli::{
    prepare_effects::{self, Target},
    project,
};
use reel_package::archive::{ArchiveIndex, ReadScratch};
use reel_ui::resources::{EffectShaderDescriptor, EffectShaderFormat};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::Path,
    process::Command,
};

const SOURCE: &str = r#"
fn reveal(pixel, progress: float = 0.25) -> float {
    let (r, g, b, a) = pixel.sample(pixel.uv);
    if progress > 0.5 { return a; }
    r * progress
}
fn tint(pixel, amount: float = 0.2) -> (float, float, float, float) {
    let (r, g, b, a) = pixel.sample(pixel.uv);
    (r * amount, g, b, a)
}
fn main() {
    let coverage = effect(reveal, {progress: 0.4});
    let color = effect(tint);
    tr("原文", {["en-US"]: "Localized"});
}
"#;

#[test]
fn compiled_box_sampling_exports_its_logical_footprint_and_warp_stays_unknown() {
    let project = project(
        r#"
use std::math::sin;
fn horizontal(pixel) -> (float, float, float, float) {
    let (x,y) = pixel.uv;
    let (w,h) = pixel.size;
    let mut red=0.0; let mut green=0.0; let mut blue=0.0; let mut alpha=0.0;
    for i in 0..61 {
        let (r,g,b,a) = pixel.sample((x+(i-30)/w,y));
        red+=r; green+=g; blue+=b; alpha+=a;
    }
    (red/61.0,green/61.0,blue/61.0,alpha/61.0)
}
fn warp(pixel) -> (float, float, float, float) {
    let (x,y)=pixel.uv;
    pixel.sample((x+sin(y),y))
}
fn main() { let h=effect(horizontal); let w=effect(warp); }
"#,
    );
    let checked = checked(project.path());
    let prepared =
        prepare_effects::prepare(project.path(), &checked.effects, Target::Desktop).unwrap();
    let descriptors = descriptors(&prepared.files);
    for definition in checked.effects.definitions() {
        let name = checked
            .effects
            .origin(definition.id)
            .unwrap()
            .function
            .as_ref();
        let descriptor = &descriptors[&EffectShaderFormat::Wgsl.asset_path(definition.id)];
        if name.ends_with("horizontal") {
            assert_eq!(
                descriptor.source_footprint.unwrap().logical_halo,
                [30, 0, 30, 0]
            );
        } else {
            assert!(name.ends_with("warp"));
            assert!(descriptor.source_footprint.is_none());
        }
    }
}

fn project(source: &str) -> tempfile::TempDir {
    let temporary = tempfile::tempdir().unwrap();
    fs::create_dir_all(temporary.path().join("src")).unwrap();
    fs::create_dir_all(temporary.path().join("resources")).unwrap();
    fs::write(temporary.path().join("src/main.aside"), source).unwrap();
    fs::write(
        temporary.path().join("resources/marker"),
        b"ordinary resource",
    )
    .unwrap();
    temporary
}
fn checked(root: &Path) -> reel_cli::Checked {
    project::load(root)
        .unwrap_or_else(|error| panic!("{:?}", error.failure))
        .check()
        .unwrap()
}
fn descriptors(
    files: &BTreeMap<String, std::path::PathBuf>,
) -> BTreeMap<String, EffectShaderDescriptor> {
    files
        .iter()
        .map(|(name, path)| {
            let bytes = fs::read(path).unwrap();
            let (descriptor, raw) =
                EffectShaderDescriptor::decode_payload(&bytes, prepare_effects::MAX_SHADER_BYTES)
                    .unwrap();
            assert!(descriptor.matches_bytes(&bytes[raw]));
            assert_eq!(descriptor.format.asset_path(descriptor.definition), *name);
            (name.clone(), descriptor)
        })
        .collect()
}

#[test]
fn desktop_cache_reuses_code_across_defaults_and_rebuilds_damaged_products() {
    let temporary = project(SOURCE);
    let root = temporary.path();
    let first = checked(root);
    assert!(
        !root.join("build").exists(),
        "checking source performs no shader IO"
    );
    let prepared = prepare_effects::prepare(root, &first.effects, Target::Desktop).unwrap();
    assert_eq!(prepared.report.converted, 2);
    let original = descriptors(&prepared.files);
    let unchanged = prepare_effects::prepare(root, &first.effects, Target::Desktop).unwrap();
    assert_eq!(
        (unchanged.report.converted, unchanged.report.reused),
        (0, 2)
    );
    assert_eq!(unchanged.files, prepared.files);
    let corrupt = prepared.files.values().next().unwrap();
    fs::write(corrupt, b"interrupted cache payload").unwrap();
    let repaired = prepare_effects::prepare(root, &first.effects, Target::Desktop).unwrap();
    assert_eq!((repaired.report.converted, repaired.report.reused), (1, 1));
    assert_eq!(descriptors(&repaired.files), original);
    fs::write(
        root.join("src/main.aside"),
        SOURCE
            .replace("progress: float = 0.25", "renamed: float = 0.75")
            .replace("progress >", "renamed >")
            .replace("r * progress", "r * renamed")
            .replace("{progress: 0.4}", "{renamed: 0.4}"),
    )
    .unwrap();
    let changed = checked(root);
    let rebuilt = prepare_effects::prepare(root, &changed.effects, Target::Desktop).unwrap();
    assert_eq!((rebuilt.report.converted, rebuilt.report.reused), (1, 1));
    let keys = |items: &BTreeMap<String, EffectShaderDescriptor>| {
        let mut keys = items.values().map(|d| d.shader_key).collect::<Vec<_>>();
        keys.sort();
        keys
    };
    assert_eq!(
        keys(&descriptors(&rebuilt.files)),
        keys(&original),
        "names/defaults change definitions while keeping compiled code identity"
    );
    let (_, files) =
        reel_cli::playback::prepare_story_assets(root, None, "vita", &changed.effects).unwrap();
    let files = files.unwrap();
    assert!(files.contains_key("marker"));
    assert_eq!(
        files
            .keys()
            .filter(|name| name.ends_with(".reel-effect"))
            .count(),
        2
    );
    assert!(
        files
            .keys()
            .filter(|name| name.ends_with(".reel-effect"))
            .all(|name| name.ends_with("/wgsl.reel-effect")),
        "preview assets cannot change the desktop shader backend"
    );
}

fn archive_products(archive: &Path) -> Vec<EffectShaderDescriptor> {
    let mut file = File::open(archive).unwrap();
    let index = ArchiveIndex::read(&mut file).unwrap();
    index
        .entries()
        .filter(|entry| entry.path.ends_with(".reel-effect"))
        .map(|entry| {
            let mut bytes = vec![0; entry.length as usize];
            entry
                .read_range(&mut file, 0, &mut bytes, &mut ReadScratch::new(), || false)
                .unwrap();
            let (descriptor, raw) =
                EffectShaderDescriptor::decode_payload(&bytes, bytes.len()).unwrap();
            assert!(descriptor.matches_bytes(&bytes[raw]));
            descriptor
        })
        .collect()
}

#[test]
fn build_publishes_movable_asb_and_shader_archive_with_catalogs() {
    let temporary = project(SOURCE);
    let root = temporary.path();
    for profile in ["debug", "release"] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_reel"));
        command
            .current_dir(root)
            .args(["build", "--format", "json"]);
        if profile == "release" {
            command.arg("--release");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let package = root.join(format!("build/{profile}/game.asb"));
        let loaded = reel_player::distribution::load(&package, None).unwrap();
        assert_eq!(loaded.catalogs.len(), 1);
        assert_eq!(loaded.package.distribution().unwrap().resources.len(), 1);
        let descriptors = loaded
            .packages
            .iter()
            .flat_map(|path| archive_products(path))
            .collect::<Vec<_>>();
        assert_eq!(descriptors.len(), 2);
        let relocated = tempfile::tempdir().unwrap();
        fs::copy(&package, relocated.path().join("game.asb")).unwrap();
        for archive in &loaded.packages {
            fs::copy(archive, relocated.path().join(archive.file_name().unwrap())).unwrap();
        }
        let detached =
            reel_player::distribution::load(&relocated.path().join("game.asb"), None).unwrap();
        let story = reel_player::distribution::story_from_package(detached.package).unwrap();
        assert_eq!(story.effects.definitions().len(), 2);
        for descriptor in &descriptors {
            assert!(
                story
                    .effects
                    .definitions()
                    .any(|d| d.id == descriptor.definition)
            );
        }
    }
}

#[test]
fn export_prepares_destination_formats_and_reuses_gxp_binary_for_new_defaults() {
    let compiler_available = reel_render::effect::gxp_compiler::Compiler::new().is_ok();
    let temporary = project(SOURCE);
    let root = temporary.path();
    let loaded = project::load(root).unwrap_or_else(|error| panic!("{:?}", error.failure));
    let checked = loaded.check().unwrap();
    let artifact =
        reel_cli::build_export(&checked, project::ENTRY, reel_cli::BuildProfile::Release).unwrap();
    let options = reel_cli::distribution::ExportOptions {
        output: root.join("build/export"),
        overwrite: false,
        resources: vec![reel_cli::distribution::Mount {
            prefix: ".".into(),
            directory: loaded.assets(),
        }],
        text: vec![],
        resource_target: if compiler_available {
            "vita"
        } else {
            "desktop"
        }
        .into(),
        pack_bytes: 1_048_576.try_into().unwrap(),
        runtime: None,
    };
    reel_cli::distribution::export(&loaded, &artifact, &options).unwrap();
    let output = reel_player::distribution::load(&options.output.join("game.asb"), None).unwrap();
    let descriptors = output
        .packages
        .iter()
        .flat_map(|path| archive_products(path))
        .collect::<Vec<_>>();
    assert_eq!(descriptors.len(), 2);
    if !compiler_available {
        assert!(
            prepare_effects::prepare(root, &checked.effects, Target::Vita)
                .err()
                .unwrap()
                .to_string()
                .contains("Sony psp2cgc compiler not found")
        );
        return;
    }
    assert!(
        descriptors
            .iter()
            .all(|d| d.format == EffectShaderFormat::Gxp)
    );
    let code_files = || {
        fs::read_dir(root.join("build/resources/objects"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "gxp"))
            .map(|path| {
                let modified = fs::metadata(&path).unwrap().modified().unwrap();
                (path, modified)
            })
            .collect::<BTreeMap<_, _>>()
    };
    let before = code_files();
    assert_eq!(before.len(), 2);
    fs::write(root.join("src/main.aside"), SOURCE.replace("0.25", "0.75")).unwrap();
    let changed = self::checked(root);
    let prepared = prepare_effects::prepare(root, &changed.effects, Target::Vita).unwrap();
    assert_eq!((prepared.report.converted, prepared.report.reused), (1, 1));
    assert_eq!(
        code_files(),
        before,
        "default-only changes reuse the GXP binary without rewriting it"
    );
}

#[test]
fn existing_mask_dag_uses_exported_effect_products_and_reuses_code_after_asb_loading() {
    let source = r#"
fn main() {
    let gray = mask.gray("mask.png", x: mask.param("x"), repeat: "x");
    let alpha = mask.alpha("mask.png");
    let curve = mask.clamp(mask.add(mask.multiply(mask.subtract(mask.param("progress"), gray), 3), 1));
    let coverage = mask.multiply(curve, alpha);
    stage.replace(stack(width: 960, height: 544, mask: coverage, mask_progress: 0.35, mask_x: 0.125));
}
"#;
    let temporary = project(source);
    let checked = checked(temporary.path());
    let (_, definition, mask) = checked
        .effects
        .binding_records()
        .find(|(_, _, mask)| mask.is_some_and(|mask| mask.operations().len() == 12))
        .expect("the existing two-sample DAG is a static template");
    let mask = mask.unwrap();
    assert_eq!(mask.sources().len(), 1);
    let prepared =
        prepare_effects::prepare(temporary.path(), &checked.effects, Target::Desktop).unwrap();
    let products = descriptors(&prepared.files);
    let path = EffectShaderFormat::Wgsl.asset_path(definition);
    assert_eq!(products[&path].textures, 1);
    assert_eq!(products[&path].parameters.len(), 4);
    let envelope = fs::read(&prepared.files[&path]).unwrap();
    let (_, raw) =
        EffectShaderDescriptor::decode_payload(&envelope, prepare_effects::MAX_SHADER_BYTES)
            .unwrap();
    let shader = std::str::from_utf8(&envelope[raw]).unwrap();
    assert!(shader.contains("reel_texture_0"));
    assert!(
        !shader.contains("u_ops"),
        "the prepared graph executes static expressions"
    );

    let artifact = reel_cli::build_with_profile(
        &checked,
        Some("main::main"),
        reel_cli::BuildProfile::Release,
    )
    .unwrap();
    let package = reel_package::decode(
        &artifact.bytes,
        reel_engine::aside::source::types(),
        Default::default(),
    )
    .unwrap();
    assert!(package.effects().bindings.iter().any(|binding| matches!(
        binding.kind,
        reel_package::EffectBindingKind::MaskTemplate { .. }
    )));
    let story = reel_player::distribution::story_from_package(package).unwrap();
    assert_eq!(story.effects.mask_definition(mask).unwrap().id, definition);
    let reused =
        prepare_effects::prepare(temporary.path(), &story.effects, Target::Desktop).unwrap();
    assert_eq!(reused.report.converted, 0);
    assert_eq!(reused.report.reused as usize, prepared.files.len());
    fs::write(
        temporary.path().join("src/main.aside"),
        source.replace("mask.png", "other.png"),
    )
    .unwrap();
    let changed_paths = project::load(temporary.path())
        .unwrap_or_else(|error| panic!("{:?}", error.failure))
        .check()
        .unwrap();
    assert!(
        changed_paths.effects.by_id(definition).is_some(),
        "image names are instance data, outside the compiled definition identity"
    );
}
