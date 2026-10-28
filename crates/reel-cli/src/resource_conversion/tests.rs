use super::*;
use reel_core::texture::{TextureFormat, TextureHeader};

#[test]
fn ordered_dither_is_validated_and_part_of_the_texture_cache_recipe() {
    let (directory, tool, target) = fixture();
    let root = directory.path();
    let config = "[[targets.vita]]\nmatch = 'images/**'\nconvert = 'texture'\nformat = 'rgba8'\n";
    fs::write(root.join("resources.toml"), config).unwrap();
    let plain = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    fs::write(
        root.join("resources.toml"),
        format!("{config}dither = 'none'\n"),
    )
    .unwrap();
    let explicit_none = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    assert_eq!(plain.files, explicit_none.files);
    fs::write(
        root.join("resources.toml"),
        format!("{config}dither = 'ordered'\n"),
    )
    .unwrap();
    let ordered = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    assert_ne!(
        plain.files["images/room.png"],
        ordered.files["images/room.png"]
    );
    let repeated = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    assert_eq!(ordered.files, repeated.files);
    assert_eq!(repeated.report.converted, 0);
    assert_eq!(repeated.report.reused, 2);
    for (convert, format, expected) in [
        ("texture", "auto", "requires format rgba8"),
        ("texture", "bc1", "requires format rgba8"),
        ("texture", "bc3", "requires format rgba8"),
        ("tiles", "rgba8", "belongs to texture rules"),
    ] {
        fs::write(root.join("resources.toml"), format!(
            "[[targets.vita]]\nmatch = 'images/**'\nconvert = '{convert}'\nformat = '{format}'\ndither = 'ordered'\n"
        )).unwrap();
        let error = prepare(root, &tool, &target, MEMORY_BYTES).err().unwrap();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn font_coverage_combines_input_ranges_translations_and_escaped_story_characters() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("src/main.aside"),
        r#"fn main() { tr("文字", en: "Words"); "\u{20BB7}"; }"#,
    )
    .unwrap();
    let characters = font_characters(
        root,
        vec!["U+4E00-9FFF".into()],
        vec!["src/**/*.aside".into()],
    )
    .unwrap();
    for character in ['文', '字', 'W', '𠮷', '龘'] {
        assert!(characters.contains(&u32::from(character)));
    }
    assert!(!characters.contains(&0xd800));
    assert!(font_characters(root, vec![], vec!["src/missing.aside".into()]).is_err());
}

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
    let identity = hash_file(tool)?.to_hex();
    prepare_with_identity(
        root,
        target,
        target.collect(&root.join("resources"))?,
        memory,
        |_| identity.as_str(),
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
        "convert = 'video'",
        "convert = 'video'\nsize = [960, 544]\nscale = 0.5",
        "convert = 'video'\nsize = [960, 544]\nbitrate = 1",
        "convert = 'copy'\nalpha_layout = 'top_bottom'",
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
                    header.descriptor().storage_layout,
                    reel_core::texture::TextureStorageLayout::GxmBcMorton
                );
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
    assert_eq!(image.image_size(), Some([12, 6]));
    assert_eq!(
        image.texture_descriptor().unwrap().layout.content_size,
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

#[test]
fn mip_opt_in_changes_recipe_and_default_remains_single_level() {
    let (directory, tool, target) = fixture();
    let root = directory.path();
    let config = "[[targets.vita]]\nmatch = 'images/**'\nconvert = 'texture'\nformat = 'rgba8'\n";
    fs::write(root.join("resources.toml"), config).unwrap();
    let plain = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    fs::write(
        root.join("resources.toml"),
        format!("{config}mipmaps = false\n"),
    )
    .unwrap();
    let default = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    assert_eq!(plain.files, default.files);
    fs::write(
        root.join("resources.toml"),
        format!("{config}mipmaps = true\n"),
    )
    .unwrap();
    let mips = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    assert_ne!(plain.files, mips.files);
    let cached = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    assert_eq!(cached.report.converted, 0);
    fs::write(
        root.join("resources.toml"),
        "[[targets.vita]]\nmatch = 'images/**'\nconvert = 'tiles'\nmipmaps = true\n",
    )
    .unwrap();
    assert!(
        prepare(root, &tool, &target, MEMORY_BYTES)
            .err()
            .unwrap()
            .to_string()
            .contains("mipmaps belongs to texture rules")
    );
}

#[test]
fn atlas_cohort_publishes_one_page_distinct_views_and_reuses_cache() {
    use reel_core::prepared_atlas::PreparedAtlasView;
    let (directory, tool, target) = fixture();
    let root = directory.path();
    fs::write(root.join("resources.toml"), "[[targets.vita]]\nmatch = 'images/**'\nconvert = 'atlas'\nformat = 'rgba8'\npage_size = [64,64]\nscale = 0.5\n").unwrap();
    let first = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    let room =
        PreparedAtlasView::decode(&fs::read(&first.files["images/room.png"]).unwrap()).unwrap();
    let ui = PreparedAtlasView::decode(&fs::read(&first.files["images/ui.png"]).unwrap()).unwrap();
    assert_eq!(room.page, ui.page);
    assert_eq!(room.image.logical_size, [12, 6]);
    assert_eq!(room.image.content_size, [6, 3]);
    assert_eq!(ui.image.logical_size, [4, 2]);
    assert_ne!(room.image.content_origin, ui.image.content_origin);
    let page_bytes = fs::read(&first.files[&room.page]).unwrap();
    let (header, payload) = TextureHeader::parse(&page_bytes).unwrap();
    let size = header.descriptor().layout.storage_size;
    assert!(size[0] < 64 && size[1] < 64);
    assert_eq!(header.descriptor().mip_count, 1);
    for view in [&room, &ui] {
        view.image.validate(size).unwrap();
        assert!(view.image.content_origin.iter().all(|n| n % 4 == 0));
        let [x, y] = view.image.content_origin;
        let at = |x: u32, y: u32| {
            &payload[((y * size[0] + x) * 4) as usize..((y * size[0] + x + 1) * 4) as usize]
        };
        assert_eq!(at(x, y), at(x - 1, y));
        assert_eq!(at(x, y), at(x, y - 1));
        assert_eq!(
            at(x + view.image.content_size[0] - 1, y),
            at(x + view.image.content_size[0], y)
        );
    }
    assert_eq!(first.report.converted, 2);
    let second = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    assert_eq!(first.files, second.files);
    assert_eq!(second.report.reused, 2);
    assert_eq!(second.report.converted, 0);
    let tiny_cache_reuse = prepare(root, &tool, &target, 1).unwrap();
    assert_eq!(tiny_cache_reuse.files, first.files);
    image::RgbaImage::from_fn(13, 9, |x, y| {
        image::Rgba([
            ((x * 19 + y * 7) % 256) as u8,
            ((x * 11 + y * 23) % 256) as u8,
            ((x * 31 + y * 13) % 256) as u8,
            ((x * 17 + y * 29) % 256) as u8,
        ])
    })
    .save(root.join("resources/images/noise.png"))
    .unwrap();
    fs::write(
        root.join("resources.toml"),
        "[[targets.vita]]\nmatch='images/**'\nconvert='atlas'\nformat='bc3'\npage_size=[64,64]\n",
    )
    .unwrap();
    let compressed = prepare(root, &tool, &target, MEMORY_BYTES).unwrap();
    let room_bc =
        PreparedAtlasView::decode(&fs::read(&compressed.files["images/room.png"]).unwrap())
            .unwrap();
    let ui_bc =
        PreparedAtlasView::decode(&fs::read(&compressed.files["images/ui.png"]).unwrap()).unwrap();
    let noise_bc =
        PreparedAtlasView::decode(&fs::read(&compressed.files["images/noise.png"]).unwrap())
            .unwrap();
    let file = fs::read(&compressed.files[&room_bc.page]).unwrap();
    let (header, payload) = TextureHeader::parse(&file).unwrap();
    let descriptor = header.descriptor();
    assert_eq!(descriptor.format, TextureFormat::Bc3Rgba);
    let [w, h] = descriptor.layout.storage_size;
    let mut blocks = vec![0; payload.len()];
    for y in 0..h / 4 {
        for x in 0..w / 4 {
            let source = descriptor.storage_layout.block_offset([w, h], 16, x, y);
            let dest = ((y * w / 4 + x) * 16) as usize;
            blocks[dest..dest + 16].copy_from_slice(&payload[source..source + 16]);
        }
    }
    let pixels = rgbcx::decode(w, h, &blocks, rgbcx::Format::Bc3).unwrap();
    let sample = |view: &PreparedAtlasView| {
        let [x, y] = view.image.content_origin;
        let offset = ((y * w + x) * 4) as usize;
        <[u8; 4]>::try_from(&pixels[offset..offset + 4]).unwrap()
    };
    for (name, view, source) in [
        (
            "room",
            &room_bc,
            root.join("resources/@vita/images/room.png"),
        ),
        ("ui", &ui_bc, root.join("resources/images/ui.png")),
        ("noise", &noise_bc, root.join("resources/images/noise.png")),
    ] {
        let standalone_path = root.join(format!("{name}-standalone.reel-texture"));
        crate::prepare_texture::prepare(
            &source,
            &standalone_path,
            crate::prepare_texture::Options {
                format: crate::prepare_texture::Format::Bc3,
                ..Default::default()
            },
        )
        .unwrap();
        let file = fs::read(standalone_path).unwrap();
        let (standalone_header, standalone) = TextureHeader::parse(&file).unwrap();
        let [sw, sh] = standalone_header.descriptor().layout.storage_size;
        for y in 0..sh / 4 {
            for x in 0..sw / 4 {
                let source = ((y * sw / 4 + x) * 16) as usize;
                let target = descriptor.storage_layout.block_offset(
                    [w, h],
                    16,
                    view.image.content_origin[0] / 4 + x,
                    view.image.content_origin[1] / 4 + y,
                );
                assert_eq!(
                    &payload[target..target + 16],
                    &standalone[source..source + 16],
                    "{name} block {x},{y}"
                );
            }
        }
    }
    let green = sample(&room_bc);
    let blue = sample(&ui_bc);
    assert!(
        green[1] > 110 && green[3] > 110 && green[0] < 8 && green[2] < 8,
        "{green:?}"
    );
    assert!(
        blue[2] > 240 && blue[3] > 240 && blue[0] < 8 && blue[1] < 8,
        "{blue:?}"
    );
    fs::write(
        root.join("resources.toml"),
        "[[targets.vita]]\nmatch='images/**'\nconvert='atlas'\nmipmaps=true\n",
    )
    .unwrap();
    assert!(
        prepare(root, &tool, &target, MEMORY_BYTES)
            .err()
            .unwrap()
            .to_string()
            .contains("one level")
    );
}
