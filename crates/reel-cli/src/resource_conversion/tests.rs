use super::*;
use reel_core::texture::{TextureFormat, TextureHeader};

fn fixture() -> (tempfile::TempDir, PathBuf, ResourceTarget) {
    let artifacts = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/reel-artifacts/resource-conversion");
    fs::create_dir_all(&artifacts).unwrap();
    let directory = tempfile::tempdir_in(artifacts).unwrap();
    let root = directory.path();
    fs::create_dir_all(root.join("resources/images")).unwrap();
    fs::create_dir_all(root.join("resources/@vita/images")).unwrap();
    fs::create_dir_all(root.join("resources/@other")).unwrap();
    fs::write(root.join("resources/plain.txt"), "common text").unwrap();
    fs::write(root.join("resources/@vita/only.txt"), "target only").unwrap();
    fs::write(root.join("resources/@other/excluded.txt"), "excluded").unwrap();
    image::RgbaImage::from_pixel(8, 4, image::Rgba([255, 0, 0, 255]))
        .save(root.join("resources/images/room.png"))
        .unwrap();
    image::RgbaImage::from_pixel(12, 6, image::Rgba([0, 255, 0, 128]))
        .save(root.join("resources/@vita/images/room.png"))
        .unwrap();
    image::RgbaImage::from_pixel(4, 2, image::Rgba([0, 0, 255, 255]))
        .save(root.join("resources/images/ui.png"))
        .unwrap();
    fs::write(
        root.join("resources.toml"),
        r#"
[[targets.vita]]
match = "images/ui.png"
convert = "copy"

[[targets.vita]]
match = "images/**"
convert = "texture"
scale = 0.5
format = "auto"
power_of_two = true
"#,
    )
    .unwrap();
    let tool = root.join("tool");
    fs::write(&tool, "converter one").unwrap();
    (directory, tool, ResourceTarget::parse("vita").unwrap())
}

fn prepare(root: &Path, tool: &Path, target: &ResourceTarget, memory: u64) -> io::Result<Prepared> {
    prepare_with_tool(
        root,
        target,
        target.collect(&root.join("resources"))?,
        tool,
        memory,
    )
}

#[test]
fn target_selection_precedes_rules_and_scaled_textures_keep_author_coordinates() {
    let (directory, tool, target) = fixture();
    let prepared = prepare(directory.path(), &tool, &target, MEMORY_BYTES).unwrap();
    assert_eq!(prepared.files.len(), 4);
    assert_eq!(
        (
            prepared.report.converted,
            prepared.report.reused,
            prepared.report.unchanged
        ),
        (1, 0, 3)
    );
    assert_eq!(
        fs::read(&prepared.files["only.txt"]).unwrap(),
        b"target only"
    );
    assert_eq!(
        prepared.files["images/ui.png"],
        directory
            .path()
            .canonicalize()
            .unwrap()
            .join("resources/images/ui.png")
    );
    let bytes = fs::read(&prepared.files["images/room.png"]).unwrap();
    let (header, _) = TextureHeader::parse(&bytes).unwrap();
    assert_eq!(header.descriptor().layout.logical_size, [12, 6]);
    assert_eq!(header.descriptor().layout.content_size, [6, 3]);
    assert_eq!(header.descriptor().layout.storage_size, [8, 8]);
    assert_eq!(header.descriptor().format, TextureFormat::Bc3Rgba);
    let published: BTreeMap<String, PathBuf> =
        serde_json::from_slice(&fs::read(prepared.index.unwrap()).unwrap()).unwrap();
    assert_eq!(published, prepared.files);
    assert!(prepared.report.workspace_bytes > 0 && prepared.report.workspace_bytes <= MEMORY_BYTES);
}

#[test]
fn cache_tracks_source_recipe_and_tool_content_and_reuses_under_a_smaller_limit() {
    let (directory, tool, target) = fixture();
    let root = directory.path();
    let first = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    let next = prepare(root, &tool, &target, 1).unwrap();
    assert_eq!(first.files, next.files);
    assert_eq!((next.report.converted, next.report.reused), (0, 1));
    fs::write(root.join("resources/plain.txt"), "new live content").unwrap();
    assert_eq!(
        prepare(root, &tool, &target, MEMORY_BYTES).unwrap().files["images/room.png"],
        first.files["images/room.png"]
    );
    image::RgbaImage::from_pixel(12, 6, image::Rgba([255, 0, 255, 128]))
        .save(root.join("resources/@vita/images/room.png"))
        .unwrap();
    let edited = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    assert_ne!(
        first.files["images/room.png"],
        edited.files["images/room.png"]
    );
    fs::write(&tool, "converter two").unwrap();
    let changed_tool = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    assert_ne!(
        edited.files["images/room.png"],
        changed_tool.files["images/room.png"]
    );
    let config = fs::read_to_string(root.join("resources.toml"))
        .unwrap()
        .replace("scale = 0.5", "scale = 0.25");
    fs::write(root.join("resources.toml"), config).unwrap();
    let changed_recipe = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    assert_ne!(
        changed_tool.files["images/room.png"],
        changed_recipe.files["images/room.png"]
    );
    fs::remove_file(root.join("resources/@vita/only.txt")).unwrap();
    assert!(
        !prepare(root, &tool, &target, MEMORY_BYTES)
            .unwrap()
            .files
            .contains_key("only.txt")
    );
}

#[test]
fn failed_conversion_keeps_the_complete_previous_index_and_cleans_staging() {
    let (directory, tool, target) = fixture();
    let root = directory.path();
    let first = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    let index = first.index.unwrap();
    let previous = fs::read(&index).unwrap();
    fs::write(root.join("resources/@vita/images/room.png"), b"invalid png").unwrap();
    assert!(prepare(root, &tool, &target, MEMORY_BYTES).is_err());
    assert_eq!(fs::read(&index).unwrap(), previous);
    assert!(
        fs::read_dir(root.join("build/resources/objects"))
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .as_encoded_bytes()
                .starts_with(b".prepare-"))
    );
    image::RgbaImage::from_pixel(12, 6, image::Rgba([20, 30, 40, 255]))
        .save(root.join("resources/@vita/images/room.png"))
        .unwrap();
    let error = prepare(root, &tool, &target, 1024)
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("workspace") && error.contains("1024"),
        "{error}"
    );
    assert_eq!(fs::read(&index).unwrap(), previous);
}

#[test]
fn configuration_diagnostics_reject_typos_and_conflicting_parameters() {
    let (directory, tool, target) = fixture();
    for fields in [
        "convert = 'texture'\nscale = 0.0",
        "convert = 'texture'\nsize = [0, 1]",
        "convert = 'texture'\nscale = 0.5\nsize = [2, 2]",
        "convert = 'texture'\nbitrate = 96000",
        "convert = 'texture'\nfirst_level = 1",
        "convert = 'tiles'\nscale = 0.5",
        "convert = 'tiles'\ntile_size = 0",
        "convert = 'copy'\nscale = 0.5",
        "convert = 'opus'\nformat = 'auto'",
        "convert = 'opus'\nbitrate = 0",
        "convert = 'texture'\nscle = 0.5",
        "convert = 'txture'",
    ] {
        fs::write(
            directory.path().join("resources.toml"),
            format!("[[targets.vita]]\nmatch = '**'\n{fields}\n"),
        )
        .unwrap();
        let error = prepare(directory.path(), &tool, &target, MEMORY_BYTES)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("resources.toml"), "{fields}: {error}");
    }
    assert!(!directory.path().join("build").exists());
}

#[test]
fn tiled_recipe_keeps_logical_names_and_publishes_only_stored_levels() {
    use reel_core::prepared_image::PreparedImage;
    let (directory, tool, target) = fixture();
    let root = directory.path();
    fs::write(root.join("resources.toml"), "[[targets.vita]]\nmatch = 'images/room.png'\nconvert = 'tiles'\ntile_size = 2\nfirst_level = 1\npower_of_two = true\n").unwrap();
    let first = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    let manifest =
        PreparedImage::decode(&fs::read(&first.files["images/room.png"]).unwrap()).unwrap();
    assert_eq!(
        [manifest.width, manifest.height, manifest.first_level],
        [12, 6, 1]
    );
    let mut tiles = 0;
    for level in manifest.first_level..manifest.levels() {
        let [width, height] = manifest.dimensions(level);
        for y in 0..height.div_ceil(manifest.tile_edge) {
            for x in 0..width.div_ceil(manifest.tile_edge) {
                let name = manifest.tile_source("images/room.png", level, x, y);
                let bytes = fs::read(&first.files[&name]).unwrap();
                let (header, _) = TextureHeader::parse(&bytes).unwrap();
                assert_eq!(
                    header.descriptor().layout.logical_size,
                    manifest.tile_dimensions(level, x, y).unwrap()
                );
                assert!(
                    header
                        .descriptor()
                        .layout
                        .storage_size
                        .into_iter()
                        .all(u32::is_power_of_two)
                );
                tiles += 1;
            }
        }
    }
    assert_eq!(first.files.len(), 4 + tiles);
    assert!(
        !first
            .files
            .keys()
            .any(|name| name.starts_with("images/room.png.tiles/0/"))
    );
    let cached = prepare(root, &tool, &target, 1).unwrap();
    assert_eq!(first.files, cached.files);
    assert_eq!((cached.report.converted, cached.report.reused), (0, 1));
    let collision = root
        .join("resources")
        .join(manifest.tile_source("images/room.png", 1, 0, 0));
    fs::create_dir_all(collision.parent().unwrap()).unwrap();
    fs::write(collision, b"author asset").unwrap();
    assert!(
        prepare(root, &tool, &target, MEMORY_BYTES)
            .err()
            .unwrap()
            .to_string()
            .contains("conflicts")
    );
}

#[test]
fn unconfigured_projects_pass_selected_files_directly_without_cache_writes() {
    let (directory, tool, target) = fixture();
    fs::remove_file(directory.path().join("resources.toml")).unwrap();
    let prepared = prepare(directory.path(), &tool, &target, MEMORY_BYTES).unwrap();
    assert_eq!(prepared.report.unchanged, 4);
    assert!(prepared.index.is_none());
    assert!(!directory.path().join("build").exists());
}

#[test]
fn development_without_selected_target_rules_keeps_lazy_directory_resolution() {
    let (directory, _, _) = fixture();
    let target = ResourceTarget::parse("desktop").unwrap();
    let prepared = prepare_directory_if_configured(
        directory.path(),
        &directory.path().join("resources"),
        &target,
    )
    .unwrap();
    assert!(prepared.is_none());
    assert!(!directory.path().join("build").exists());
    let (assets, files) =
        crate::playback::prepare_assets(directory.path(), None, "desktop").unwrap();
    assert_eq!(assets, directory.path().join("resources"));
    assert!(files.is_none());
}

#[test]
fn prepared_logical_paths_reach_the_player_and_both_export_profiles() {
    use reel_player::assets::{AssetKind, AssetOptions, Assets};
    use std::time::{Duration, Instant};

    fn bytes(assets: &Assets, name: &str) -> Vec<u8> {
        let request = assets.request(name, AssetKind::Bytes);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assets.maintain();
            if let Some((_, bytes)) = assets.poll_bytes(request).unwrap() {
                return bytes.as_slice().to_vec();
            }
            assert!(
                Instant::now() < deadline,
                "resource worker timed out: {name}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    let (directory, tool, target) = fixture();
    let root = directory.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.aside"), "fn main() { 1 }").unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/assets/chime.wav"),
        root.join("resources/chime.wav"),
    )
    .unwrap();
    let mut config = fs::read_to_string(root.join("resources.toml")).unwrap();
    config.push_str("\n[[targets.vita]]\nmatch = '*.wav'\nconvert = 'opus'\nbitrate = 64000\n");
    fs::write(root.join("resources.toml"), config).unwrap();
    let prepared = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    assert_eq!(prepared.report.converted, 2);
    let expected_image = fs::read(&prepared.files["images/room.png"]).unwrap();
    let expected_audio = fs::read(&prepared.files["chime.wav"]).unwrap();
    assert!(expected_audio.starts_with(b"OggS"));
    let live = Assets::selected(AssetOptions::default(), prepared.files, None).unwrap();
    assert_eq!(bytes(&live, "images/room.png"), expected_image);
    assert_eq!(bytes(&live, "chime.wav"), expected_audio);
    let request = live.request("images/room.png", AssetKind::Image);
    let deadline = Instant::now() + Duration::from_secs(10);
    let image = loop {
        if let Some(image) = live.poll(request).unwrap() {
            break image;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(image.data().image_size(), Some([12, 6]));
    assert_eq!(
        image
            .data()
            .texture_descriptor()
            .unwrap()
            .layout
            .content_size,
        [6, 3]
    );
    live.close();

    let project = crate::project::load(root).unwrap_or_else(|error| panic!("{:?}", error.failure));
    let checked = project.check().unwrap();
    let player = root.join("platform-player");
    fs::write(&player, b"test player").unwrap();
    for profile in [crate::BuildProfile::Debug, crate::BuildProfile::Release] {
        let artifact = crate::build_export(&checked, crate::project::ENTRY, profile).unwrap();
        let options = crate::distribution::ExportOptions {
            output: root.join("build/export").join(profile.name()),
            overwrite: false,
            resources: vec![crate::distribution::Mount {
                prefix: ".".into(),
                directory: root.join("resources"),
            }],
            text: Vec::new(),
            resource_target: target.as_str().into(),
            pack_bytes: (4 << 20).try_into().unwrap(),
            runtime: Some(crate::distribution::Runtime::Executable(player.clone())),
        };
        crate::distribution::export(&project, &artifact, &options).unwrap();
        let distribution =
            reel_player::distribution::load(&options.output.join("game.asb"), None).unwrap();
        let assets = Assets::packaged(
            AssetOptions::default(),
            distribution.packages,
            distribution.loose,
        )
        .unwrap();
        assert_eq!(bytes(&assets, "images/room.png"), expected_image);
        assert_eq!(bytes(&assets, "chime.wav"), expected_audio);
        assert!(bytes(&assets, "plain.txt").starts_with(b"common text"));
        assets.close();
    }
}
