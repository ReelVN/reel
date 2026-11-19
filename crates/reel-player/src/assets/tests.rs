use super::*;
use reel_core::{
    identity::ContentVersion,
    result::Rejected,
    test_support::{MemoryAsset, MemoryAssetIo},
};

use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

const PNG: &[u8] = include_bytes!("../../../../examples/assets/panels.png");
const JPEG: &[u8] = include_bytes!("../../../../examples/assets/sun.jpg");
mod cost;
pub(crate) mod memory_reader;
mod packed;
mod streaming;
use memory_reader::MemoryReader;

/// Supply compiled source/ASB definitions through the ordinary asset IO and
/// shader-envelope admission path used by the exported player.
pub(crate) fn prepared_effect_assets(
    products: &reel_engine::aside::source::EffectProducts,
) -> Assets {
    use reel_render::effect::{Language, compile_fragment};
    use reel_ui::resources::{EffectShaderDescriptor, EffectShaderFormat};
    Assets::with_adapters(AssetOptions::default(), |wake| {
        let limits = QueueLimits {
            max_requests: 32,
            max_bytes: 64 << 20,
        };
        let input = BudgetPool::new(BudgetCategory::CpuInput, limits);
        let mut io = MemoryAssetIo::new(limits, limits, wake.clone());
        for definition in products.definitions() {
            {
                let (language, format) = (Language::Wgsl, EffectShaderFormat::Wgsl);
                let fragment = compile_fragment(&definition.program, language, 4 << 20).unwrap();
                let descriptor = EffectShaderDescriptor::with_textures(
                    definition.id,
                    format,
                    definition
                        .program
                        .parameters()
                        .iter()
                        .map(|p| p.ty)
                        .collect(),
                    definition.program.textures().len() as u32,
                    fragment.source_sampler,
                    fragment.output_type,
                    fragment.source.as_bytes(),
                )
                .unwrap();
                let bytes = descriptor
                    .encode_payload(fragment.source.as_bytes())
                    .unwrap();
                let version = ResourceVersion {
                    key: AssetKey(format.asset_path(definition.id)),
                    version: ContentVersion(*blake3::hash(&bytes).as_bytes()),
                };
                let bytes =
                    OwnedBytes::new(bytes.clone(), input.reserve(bytes.len() as u64).unwrap())
                        .unwrap();
                io.insert_asset(
                    version,
                    MemoryAsset {
                        bytes,
                        media_type: None,
                    },
                )
                .unwrap();
            }
        }
        (
            Box::new(MemoryReader::new(io, wake.clone(), true, None)),
            Box::new(ImageExecutor::new(limits, wake)),
        )
    })
}

#[test]
fn prepared_texture_preserves_blocks_and_uses_bounded_workspace() {
    use reel_core::texture::*;
    for format in [
        TextureFormat::Rgba8,
        TextureFormat::Bc1Rgb,
        TextureFormat::Bc3Rgba,
    ] {
        let descriptor = TextureDescriptor {
            storage_layout: reel_core::texture::TextureStorageLayout::Linear,
            mip_count: 1,
            format,
            source_encoding: ColorEncoding::Srgb,
            layout: TextureLayout {
                logical_size: [192, 108],
                content_size: [96, 54],
                storage_size: [128, 64],
            },
        };
        let blocks = vec![0; descriptor.byte_len().unwrap() as usize];
        for codec in [TextureCodec::Raw, TextureCodec::BcCrunch] {
            if codec == TextureCodec::BcCrunch && !format.is_compressed() {
                continue;
            }
            let payload = if codec == TextureCodec::Raw {
                blocks.clone()
            } else {
                bc_crunch::compress(
                    128,
                    64,
                    if format == TextureFormat::Bc1Rgb {
                        bc_crunch::Format::Bc1
                    } else {
                        bc_crunch::Format::Bc3
                    },
                    &blocks,
                )
                .unwrap()
            };
            let mut bytes = TextureHeader::new(descriptor, codec, payload.len() as u64)
                .unwrap()
                .encode()
                .to_vec();
            bytes.extend(payload);
            let assets = Assets::directory(AssetOptions {
                resident_bytes: 65536,
                input_bytes: 65536,
                decode_bytes: if codec == TextureCodec::Raw { 0 } else { 65536 },
                ..Default::default()
            });
            let id = assets
                .request_encoded("prepared.reel-texture".into(), &bytes)
                .unwrap();
            let lease = load_until_done(&assets, id).unwrap();
            let actual = lease.texture_descriptor().expect("prepared texture");
            let data = lease.read_pixels().expect("prepared texture bytes");
            assert_eq!(actual, descriptor);
            assert_eq!(data.bytes(), blocks);
            assert_eq!(lease.image_size(), Some([192, 108]));
            assert!(assets.temporary_pool().usage().peak_bytes <= 65536);
            assert_eq!(
                assets.output_pool().usage().bytes,
                if codec == TextureCodec::Raw {
                    bytes.capacity() as u64
                } else {
                    blocks.len() as u64
                }
            );
            drop(data);
            drop(lease);
            assets.cancel(id);
            assets.maintain();
            assert_eq!(assets.output_pool().usage().bytes, 0);
        }
    }
}

#[test]
fn prepared_texture_fast_path_avoids_probe_jobs_and_raw_payload_copies() {
    use reel_core::{texture::*, work::*};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counted(ImageExecutor, Arc<AtomicUsize>);
    impl WorkExecutor for Counted {
        fn capabilities(&self) -> WorkCaps {
            self.0.capabilities()
        }
        fn submit(&mut self, request: WorkRequest) -> Result<WorkId, Rejected<WorkRequest>> {
            self.1.fetch_add(1, Ordering::Relaxed);
            self.0.submit(request)
        }
        fn promote_visible(&mut self, id: WorkId) {
            self.0.promote_visible(id);
        }
        fn cancel(&mut self, id: WorkId) {
            self.0.cancel(id);
        }
        fn advance(&mut self, budget: WorkBudget) -> WorkProgress {
            self.0.advance(budget)
        }
        fn drain(&mut self, out: &mut Vec<WorkEvent>, limit: usize) {
            self.0.drain(out, limit);
        }
    }
    let descriptor = TextureDescriptor {
        storage_layout: reel_core::texture::TextureStorageLayout::Linear,
        mip_count: 1,
        format: TextureFormat::Bc3Rgba,
        source_encoding: ColorEncoding::Linear,
        layout: TextureLayout::identity([64, 64]),
    };
    let blocks = vec![0; descriptor.byte_len().unwrap() as usize];
    for (codec, exact_budget, expected_jobs) in [
        (TextureCodec::Raw, false, 0),
        (TextureCodec::Raw, true, 1),
        (TextureCodec::BcCrunch, false, 1),
    ] {
        let payload = if codec == TextureCodec::Raw {
            blocks.clone()
        } else {
            bc_crunch::compress(64, 64, bc_crunch::Format::Bc3, &blocks).unwrap()
        };
        let mut file = TextureHeader::new(descriptor, codec, payload.len() as u64)
            .unwrap()
            .encode()
            .to_vec();
        file.extend(payload);
        let jobs = Arc::new(AtomicUsize::new(0));
        let assets = Assets::with_adapters(
            AssetOptions {
                input_bytes: 65536,
                resident_bytes: if exact_budget {
                    blocks.len() as u64
                } else {
                    65536
                },
                decode_bytes: 65536,
                ..Default::default()
            },
            |wake| {
                let limits = QueueLimits {
                    max_requests: 32,
                    max_bytes: 1 << 20,
                };
                (
                    Box::new(MemoryReader::new(
                        MemoryAssetIo::new(limits, limits, wake.clone()),
                        wake.clone(),
                        true,
                        None,
                    )),
                    Box::new(Counted(ImageExecutor::new(limits, wake), jobs.clone())),
                )
            },
        );
        let id = assets
            .request_encoded("image.reel-texture".into(), &file)
            .unwrap();
        let address = assets.0.borrow().loads[&id]
            .input
            .as_ref()
            .unwrap()
            .as_slice()
            .as_ptr();
        let lease = load_until_done(&assets, id).unwrap();
        let bytes = lease.read_pixels().expect("prepared texture bytes");
        assert_eq!(bytes.bytes(), blocks);
        assert_eq!(jobs.load(Ordering::Relaxed), expected_jobs);
        if codec == TextureCodec::Raw && !exact_budget {
            assert_eq!(
                bytes.bytes().as_ptr(),
                address.wrapping_add(TextureHeader::LEN),
                "raw payload retains the original read allocation"
            );
            assert_eq!(assets.0.borrow().input.usage().bytes, 0);
        }
        eprintln!(
            "prepared texture {codec:?} exact_budget={exact_budget}: worker_jobs={expected_jobs} retained_bytes={}",
            assets.output_pool().usage().bytes
        );
    }
}

#[test]
fn raw_texture_and_font_move_one_full_group_allocation_without_copy() {
    use reel_core::texture::*;
    let descriptor = TextureDescriptor {
        storage_layout: TextureStorageLayout::Linear,
        mip_count: 1,
        format: TextureFormat::Rgba8,
        source_encoding: ColorEncoding::Srgb,
        layout: TextureLayout::identity([8, 8]),
    };
    let payload = vec![3; descriptor.byte_len().unwrap() as usize];
    let mut encoded = TextureHeader::new(descriptor, TextureCodec::Raw, payload.len() as u64)
        .unwrap()
        .encode()
        .to_vec();
    encoded.extend_from_slice(&payload);
    let limit = encoded.len() as u64;
    let group = BudgetGroup::new(limit);
    let limits = QueueLimits {
        max_requests: 32,
        max_bytes: limit,
    };
    let assets = memory_assets();
    {
        let mut service = assets.0.borrow_mut();
        service.input = group.pool(BudgetCategory::CpuInput, limits);
        service.output = group.pool(BudgetCategory::CpuOutput, limits);
    }
    let id = assets.request_encoded("raw".into(), &encoded).unwrap();
    let address = assets.0.borrow().loads[&id]
        .input
        .as_ref()
        .unwrap()
        .as_slice()
        .as_ptr();
    let lease = load_until_done(&assets, id).unwrap();
    let bytes = lease.read_pixels().expect("expected texture");
    assert_eq!(
        bytes.bytes().as_ptr(),
        address.wrapping_add(TextureHeader::LEN)
    );
    assert_eq!(group.usage().bytes, limit);
    assert_eq!(group.usage().peak_bytes, limit);
    drop(bytes);
    drop(lease);
    assert_eq!(group.usage().bytes, 0);

    let mut service = assets.0.borrow_mut();
    let content = vec![5; 128];
    let address = content.as_ptr();
    let mut load = Load {
        source: AssetSource {
            key: AssetKey("font".into()),
            version: Some(ContentVersion([1; 32])),
        },
        reloadable: false,
        kind: AssetKind::Font,
        priority: Priority::Visible,
        prefetch_limit: u64::MAX,
        phase: Phase::Decode,
        input: Some(Arc::new(
            OwnedBytes::new(content, service.input.reserve(128).unwrap()).unwrap(),
        )),
        size: [0, 0],
        texture: None,
        output: 0,
        temporary: 0,
    };
    service.advance(&mut load).unwrap();
    let Phase::Done(Ok(lease)) = load.phase else {
        panic!("font was not prepared")
    };
    let reel_ui::resources::ResourceView::Font(font) = lease.view() else {
        panic!("expected font")
    };
    assert_eq!(font.data().as_ptr(), address);
    assert_eq!(group.usage().bytes, 128);
    drop(lease);
    assert_eq!(group.usage().bytes, 0);
}

#[test]
fn small_png_metadata_uses_the_configured_decoder_budget() {
    use image::ImageEncoder;
    let pixels = vec![128; 19 * 5 * 4];
    let mut encoded = Vec::new();
    image::codecs::png::PngEncoder::new(&mut encoded)
        .write_image(&pixels, 19, 5, image::ExtendedColorType::Rgba8)
        .unwrap();
    let mut comment = b"tEXtComment\0".to_vec();
    comment.resize(16 << 10, b'x');
    let mut crc = flate2::Crc::new();
    crc.update(&comment);
    let mut chunk = ((comment.len() - 4) as u32).to_be_bytes().to_vec();
    chunk.extend(comment);
    chunk.extend(crc.sum().to_be_bytes());
    encoded.splice(33..33, chunk);
    for (limit, succeeds) in [(64 << 10, true), (1024, false)] {
        let assets = Assets::directory(AssetOptions {
            decode_bytes: limit,
            ..Default::default()
        });
        let id = assets.request_encoded("tiny.png".into(), &encoded).unwrap();
        let result = load_until_done(&assets, id);
        if succeeds {
            let lease = result.unwrap();
            let image = crate::test_image::data(&lease);
            assert_eq!([image.width, image.height], [19, 5]);
            assert_eq!(image.data.as_ref(), pixels);
            drop(lease);
        } else {
            assert!(
                result.is_err(),
                "metadata must remain within the configured budget"
            );
        }
        assets.cancel(id);
        assert_eq!(assets.temporary_pool().usage().bytes, 0);
        assert_eq!(assets.0.borrow().input.usage().bytes, 0);
        assert_eq!(assets.0.borrow().output.usage().bytes, 0);
    }
}

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
#[test]
fn rgba8_png_requests_decode_from_header_without_a_worker_probe() {
    use image::ImageEncoder;
    let pixels = vec![90; 19 * 5 * 4];
    let mut encoded = Vec::new();
    image::codecs::png::PngEncoder::new(&mut encoded)
        .write_image(&pixels, 19, 5, image::ExtendedColorType::Rgba8)
        .unwrap();
    let assets = Assets::directory(AssetOptions::default());
    let id = assets.request_encoded("test".into(), &encoded).unwrap();
    assets.maintain();
    {
        let service = assets.0.borrow();
        let load = &service.loads[&id];
        assert!(matches!(load.phase, Phase::Decoding(_)));
        assert_eq!(load.output, pixels.len() as u64);
    }
    let lease = load_until_done(&assets, id).unwrap();
    assert_eq!(lease.read_pixels().unwrap().bytes(), pixels);
}

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
#[test]
fn independent_pngs_share_decode_workspace_and_cancellation_releases_the_slot() {
    let pixels = [25, 50, 75, 255].repeat(32 * 32);
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, 32, 32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&pixels)
            .unwrap();
    }
    let assets = Assets::directory(AssetOptions {
        decode_bytes: 320 << 10,
        ..Default::default()
    });
    let first = assets.request_encoded("first".into(), &encoded).unwrap();
    let second = assets.request_encoded("second".into(), &encoded).unwrap();
    let same = assets.request_encoded("first".into(), &encoded).unwrap();
    assets.maintain();
    {
        let service = assets.0.borrow();
        assert!(matches!(service.loads[&first].phase, Phase::Decoding(_)));
        assert!(matches!(service.loads[&second].phase, Phase::Decoding(_)));
        assert!(matches!(service.loads[&same].phase, Phase::DecodePlainPng));
        assert_eq!(service.pending_pngs.len(), 2);
    }
    assets.cancel(first);
    for id in [second, same] {
        let image = load_until_done(&assets, id).unwrap();
        assert_eq!(image.read_pixels().unwrap().bytes(), pixels);
    }
    assert!(assets.0.borrow().pending_pngs.is_empty());
    assert_eq!(assets.temporary_pool().usage().bytes, 0);
}

#[test]
fn static_webp_decodes_by_content_with_reserved_workspace() {
    let pixels = [1, 23, 45, 128].repeat(19 * 5);
    let mut encoded = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut encoded)
        .encode(&pixels, 19, 5, image::ExtendedColorType::Rgba8)
        .unwrap();
    for (decode_bytes, succeeds) in [(256 << 10, true), (64 << 10, false)] {
        let assets = Assets::directory(AssetOptions {
            decode_bytes,
            ..Default::default()
        });
        let id = assets
            .request_encoded("logical.png".into(), &encoded)
            .unwrap();
        let result = load_until_done(&assets, id);
        if succeeds {
            let lease = result.unwrap();
            let image = crate::test_image::data(&lease);
            assert_eq!([image.width, image.height], [19, 5]);
            assert_eq!(image.data.as_ref(), pixels);
        } else {
            assert_eq!(result.err().unwrap(), "image decode limit exceeded");
        }
        assets.cancel(id);
        assert_eq!(assets.temporary_pool().usage().bytes, 0);
    }
}

pub(super) fn load_until_done(assets: &Assets, id: LoadId) -> Result<ResourceLease, String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(lease) = assets.poll(id)? {
            return Ok(lease);
        }
        assert!(std::time::Instant::now() < deadline, "asset worker stalled");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn retained_assets_preserve_provenance_and_fixed_versions() {
    let assets = memory_assets();
    let file = load_until_done(
        &assets,
        assets.request("assets/panels.png", AssetKind::Image),
    )
    .unwrap();
    let saved = assets.retained_asset(&file).unwrap();
    assert_eq!(saved.version, *file.version());
    assert_eq!(saved.kind, RetainedAssetKind::Image);
    let reloaded = load_until_done(
        &assets,
        assets.request_version(saved.version.clone(), AssetKind::Image),
    )
    .unwrap();
    assert_eq!(reloaded.version(), file.version());
    let encoded = load_until_done(
        &assets,
        assets
            .request_encoded("assets/panels.png".into(), PNG)
            .unwrap(),
    )
    .unwrap();
    assert!(
        assets.retained_asset(&encoded).is_none(),
        "a matching key cannot turn author bytes into a reloadable asset"
    );
    let wrong = ResourceVersion {
        version: ContentVersion([0x55; 32]),
        ..saved.version
    };
    assert!(
        load_until_done(&assets, assets.request_version(wrong, AssetKind::Image)).is_err(),
        "restore silently substituted the current asset"
    );
    let invalid = memory_assets_with_description(Some(None));
    assert!(
        load_until_done(
            &invalid,
            invalid.request_version(file.version().clone(), AssetKind::Image),
        )
        .unwrap_err()
        .contains("described a different version"),
        "the host erased the fixed version when Describe omitted it"
    );
}

#[test]
fn prepared_versions_share_only_live_matching_kinds_and_respect_explicit_retirement() {
    let assets = memory_assets();
    let first = load_until_done(
        &assets,
        assets.request("assets/panels.png", AssetKind::Image),
    )
    .unwrap();
    let weak = first.downgrade();
    let shared = load_until_done(
        &assets,
        assets.request("assets/panels.png", AssetKind::Image),
    )
    .unwrap();
    assert_eq!(first.handle(), shared.handle());
    assert_eq!(assets.output_pool().usage().bytes, 160 * 90 * 4);
    let font = load_until_done(
        &assets,
        assets.request("assets/panels.png", AssetKind::Font),
    )
    .unwrap();
    assert!(matches!(
        font.view(),
        reel_ui::resources::ResourceView::Font(_)
    ));
    assert_ne!(first.handle(), font.handle());
    first.prohibit_new_leases();
    assert!(weak.upgrade().is_none());
    let replacement = load_until_done(
        &assets,
        assets.request("assets/panels.png", AssetKind::Image),
    )
    .unwrap();
    assert_ne!(first.handle(), replacement.handle());
    assert_eq!(first.version(), replacement.version());
    assert!(!replacement.is_retired());
    drop((first, shared, font, replacement));
    assert!(weak.is_released());
    assert_eq!(assets.output_pool().usage().bytes, 0);
}

#[test]
fn loose_resources_share_within_a_launch_and_reopen_on_the_next_launch() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/reel-artifacts/asset-version-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("current.png");
    std::fs::write(&path, PNG).unwrap();
    let assets = Assets::directory(AssetOptions {
        root: root.clone(),
        ..Default::default()
    });
    let first = load_until_done(&assets, assets.request("current.png", AssetKind::Image)).unwrap();
    let shared = load_until_done(&assets, assets.request("current.png", AssetKind::Image)).unwrap();
    assert_eq!(first.handle(), shared.handle());
    let identity = first.version().clone();
    drop((first, shared));
    assert_eq!(assets.output_pool().usage().bytes, 0);
    assets.close();
    drop(assets);
    std::fs::write(&path, JPEG).unwrap();
    let assets = Assets::directory(AssetOptions {
        root: root.clone(),
        ..Default::default()
    });
    let changed = load_until_done(
        &assets,
        assets.request_version(identity.clone(), AssetKind::Image),
    )
    .unwrap();
    assert_eq!(*changed.version(), identity);
    assert_eq!(changed.image_size(), Some([96, 96]));
    drop(changed);
    assert_eq!(assets.output_pool().usage().bytes, 0);
    drop(assets);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(root).unwrap();
}

pub(crate) fn memory_assets() -> Assets {
    memory_assets_with_description(None)
}
fn memory_assets_with_description(version: Option<Option<ContentVersion>>) -> Assets {
    Assets::with_adapters(AssetOptions::default(), |wake| {
        let limits = QueueLimits {
            max_requests: 32,
            max_bytes: 768 << 20,
        };
        let pool = BudgetPool::new(BudgetCategory::CpuInput, limits);
        let mut io = MemoryAssetIo::new(limits, limits, wake.clone());
        for (key, bytes) in [("assets/panels.png", PNG), ("assets/sun.jpg", JPEG)] {
            io.insert_asset(
                ResourceVersion {
                    key: AssetKey(key.into()),
                    version: ContentVersion(*blake3::hash(bytes).as_bytes()),
                },
                MemoryAsset {
                    bytes: OwnedBytes::new(
                        bytes.to_vec(),
                        pool.reserve(bytes.len() as u64).unwrap(),
                    )
                    .unwrap(),
                    media_type: None,
                },
            )
            .unwrap();
        }
        (
            Box::new(MemoryReader::new(io, wake.clone(), true, version)),
            Box::new(ImageExecutor::new(limits, wake)),
        )
    })
}

#[test]
fn concurrent_images_share_decode_capacity_and_source_identities() {
    let mut versions = Vec::new();
    let (packaged, _fixture) = packed::packaged_assets(64 << 20);
    for assets in [
        Assets::directory(AssetOptions {
            root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
            ..Default::default()
        }),
        memory_assets(),
        packaged,
    ] {
        let (send, receive) = mpsc::channel();
        assets.set_waker(Arc::new(move || {
            let _ = send.send(());
        }));
        let ids = [
            assets.request("assets/panels.png", AssetKind::Image),
            assets.request("assets/sun.jpg", AssetKind::Image),
        ];
        let mut leases = [None, None];
        while leases.iter().any(Option::is_none) {
            for (index, id) in ids.into_iter().enumerate() {
                if leases[index].is_none() {
                    leases[index] = assets.poll(id).unwrap();
                }
            }
            if leases.iter().any(Option::is_none) {
                receive
                    .recv_timeout(Duration::from_secs(10))
                    .expect("resource progress must wake host");
            }
        }
        let leases = leases.map(Option::unwrap);
        let actual = leases
            .iter()
            .map(|lease| lease.version().clone())
            .collect::<Vec<_>>();
        versions.push(actual);
        for (lease, size) in leases.iter().zip([[160, 90], [96, 96]]) {
            let image = crate::test_image::data(lease);
            assert_eq!([image.width, image.height], size);
        }
        let output = assets.output_pool();
        assert_eq!(output.usage().bytes, (160 * 90 + 96 * 96) * 4);
        assert_eq!(
            assets.temporary_pool().usage().bytes,
            assets.0.borrow().io_workspace_bytes
        );
        assert_eq!(assets.0.borrow().input.usage().bytes, 0);
        assert_eq!(assets.0.borrow().completion.usage().bytes, 0);
        assert!(!assets.needs_poll());
        assets.close();
        assets.0.borrow().wake.wake(); // A worker may finish after service shutdown.
        assert!(
            !assets.needs_poll(),
            "a closed service must not keep the host polling"
        );
        assert!(
            output.usage().bytes > 0,
            "closing IO must preserve frame-owned pixels"
        );
        drop(leases);
        assert_eq!(output.usage().bytes, 0);
        let late = assets.request("assets/panels.png", AssetKind::Image);
        assert_eq!(assets.poll(late).unwrap_err(), "asset service closed");
    }
    assert_eq!(
        versions[1], versions[2],
        "published digests agree across memory and archives"
    );
    assert_eq!(
        versions[0].iter().map(|v| &v.key).collect::<Vec<_>>(),
        versions[1].iter().map(|v| &v.key).collect::<Vec<_>>()
    );
}

#[test]
fn concurrent_font_requests_share_one_resident_allocation() {
    const FONT: &[u8] = include_bytes!(
        "../../../../examples/author_arcade/resources/fonts/AtkinsonHyperlegible-Regular.ttf"
    );
    let assets = Assets::with_adapters(
        AssetOptions {
            resident_bytes: FONT.len() as u64,
            ..Default::default()
        },
        |wake| {
            let limits = QueueLimits {
                max_requests: 32,
                max_bytes: 1 << 20,
            };
            let pool = BudgetPool::new(BudgetCategory::CpuInput, limits);
            let mut io = MemoryAssetIo::new(limits, limits, wake.clone());
            io.insert_asset(
                ResourceVersion {
                    key: AssetKey("font.ttf".into()),
                    version: ContentVersion(*blake3::hash(FONT).as_bytes()),
                },
                MemoryAsset {
                    bytes: OwnedBytes::new(FONT.to_vec(), pool.reserve(FONT.len() as u64).unwrap())
                        .unwrap(),
                    media_type: None,
                },
            )
            .unwrap();
            (
                Box::new(MemoryReader::new(io, wake.clone(), true, None)),
                Box::new(ImageExecutor::new(limits, wake)),
            )
        },
    );
    let first = assets.request("font.ttf", AssetKind::Font);
    let second = assets.request("font.ttf", AssetKind::Font);
    for _ in 0..3 {
        assets.maintain();
    }
    let first = assets.take_ready(first).unwrap().unwrap();
    let second = assets.take_ready(second).unwrap().unwrap();
    assert_eq!(first.handle(), second.handle());
    assert_eq!(assets.output_pool().usage().bytes, FONT.len() as u64);
    assert_eq!(assets.0.borrow().input.usage().bytes, 0);
    drop((first, second));
    assert_eq!(assets.output_pool().usage().bytes, 0);
}

#[test]
fn worker_stream_reads_ranges_seeks_and_cancels_through_the_shared_asset_service() {
    use std::io::{Read, Seek, SeekFrom};
    let (packaged, _fixture) = packed::packaged_assets(1024);
    for assets in [
        Assets::directory(AssetOptions {
            root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
            input_bytes: 1024,
            ..Default::default()
        }),
        memory_assets(),
        packaged,
    ] {
        let (control, mut reader) = assets.stream("assets/panels.png").unwrap();
        let worker = std::thread::spawn(move || {
            let length = reader.open().unwrap();
            let mut first = [0; 8];
            reader.read_exact(&mut first).unwrap();
            assert_eq!(&first, &PNG[..8]);
            reader.seek(SeekFrom::End(-11)).unwrap();
            let mut tail = Vec::new();
            reader.read_to_end(&mut tail).unwrap();
            assert_eq!(tail, &PNG[PNG.len() - 11..]);
            reader.seek(SeekFrom::Start(0)).unwrap();
            let mut all = Vec::new();
            reader.read_to_end(&mut all).unwrap();
            assert_eq!(all, PNG);
            assert_eq!(length, PNG.len() as u64);
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !worker.is_finished() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        worker.join().unwrap();
        drop(control);
        assets.maintain();
        assert_eq!(assets.0.borrow().input.usage().bytes, 0);
        assert_eq!(assets.0.borrow().completion.usage().bytes, 0);
        let (control, mut reader) = assets.stream("assets/panels.png").unwrap();
        control.cancel();
        assert_eq!(
            reader.open().unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
        drop(reader);
        drop(control);
        assets.maintain();
        let (_, mut reader) = assets.stream("assets/panels.png").unwrap();
        assets.close();
        assert_eq!(
            reader.open().unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
    }
}

#[test]
fn stream_input_headroom_survives_full_ordinary_admission_and_tiny_budgets() {
    use std::io::Read;
    for bytes in 0..=8u64 {
        let (assets, _fixture) = packed::packaged_assets(bytes);
        let ordinary = assets.0.borrow().options.ordinary_input_bytes();
        let held = assets.0.borrow().input.reserve(ordinary).unwrap();
        assert!(
            assets
                .try_request_encoded("too-large".into(), &vec![0; ordinary as usize + 1])
                .unwrap_err()
                .contains("capacity")
        );
        if bytes < 4 {
            assert!(assets.stream("assets/panels.png").is_err());
            continue;
        }
        let mut workers = Vec::new();
        for _ in 0..4 {
            let (_, mut reader) = assets.stream("assets/panels.png").unwrap();
            workers.push(std::thread::spawn(move || {
                let mut prefix = [0; 8];
                reader.read_exact(&mut prefix).unwrap();
                assert_eq!(&prefix, &PNG[..8]);
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while workers.iter().any(|worker| !worker.is_finished()) {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assets.maintain();
        let service = assets.0.borrow();
        assert_eq!(service.input.usage().bytes, ordinary);
        assert_eq!(service.stream_input.usage().bytes, 0);
        assert!(service.stream_input.usage().peak_bytes <= bytes / 4);
        assert_eq!(service.completion.usage().bytes, 0);
        drop(service);
        drop(held);
    }
}

#[test]
fn idle_asset_service_preserves_external_wakes_without_polling_empty_workers() {
    let assets = Assets::directory(AssetOptions::default());
    assets.maintain();
    let before = assets.0.borrow().pumps;
    for _ in 0..4096 {
        assets.maintain();
    }
    assert_eq!(assets.0.borrow().pumps, before);
    println!(
        "idle_assets maintenance_calls=4096 worker_pumps={}",
        assets.0.borrow().pumps - before
    );
    assets.0.borrow().wake.wake();
    assets.maintain();
    assert_eq!(assets.0.borrow().pumps, before + 1);
    let completion = assets.0.borrow().completion.reserve(64).unwrap();
    assets.maintain();
    assert_eq!(assets.0.borrow().pumps, before + 2);
    drop(completion);
    assets.maintain();
    assert_eq!(assets.0.borrow().pumps, before + 2);
}

#[test]
fn settled_images_drain_cancelled_worker_completions_and_clear_wake() {
    let assets = Assets::directory(AssetOptions::default());
    let (send, wake) = mpsc::channel();
    assets.set_waker(Arc::new(move || {
        let _ = send.send(());
    }));
    let mut images = crate::images::Images::new(assets.clone());
    assert!(images.poll().unwrap());
    assert!(!images.has_result_pending());

    // A shared-service consumer starts work after the image demand has settled.
    let request = assets.request_encoded("cancelled.png".into(), PNG).unwrap();
    assets.maintain();
    wake.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(assets.0.borrow().completion.usage().tickets, 1);
    assets.cancel(request);
    assert!(assets.0.borrow().loads.is_empty());
    assert!(assets.pending_io());
    assert!(images.has_result_pending());

    // Draining a worker terminal can itself notify peers about released capacity.
    // Both that wake and the cancelled completion must clear through settled poll.
    let mut polls = 0;
    while images.has_result_pending() || assets.pending_io() {
        assert!(polls < 4, "settled images left resource cleanup runnable");
        assert!(images.poll().unwrap());
        polls += 1;
    }
    let service = assets.0.borrow();
    for pool in [
        &service.completion,
        &service.input,
        &service.output,
        &service.temporary,
    ] {
        assert_eq!(pool.usage().tickets, 0);
        assert_eq!(pool.usage().bytes, 0);
    }
    assert!(images.loaded.is_empty());
    println!(
        "settled_cancelled_worker cleanup_polls={polls} completion_tickets=0 wake_pending=false"
    );
}
