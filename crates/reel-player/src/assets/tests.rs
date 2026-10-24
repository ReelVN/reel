use super::*;
use reel_core::{
    identity::ContentVersion,
    io::{IoCaps, SaveWrite},
    result::Rejected,
    test_support::{MemoryAsset, MemoryAssetIo},
};
use reel_ui::resources::ResourceData;
use std::{sync::mpsc, time::Duration};

const PNG: &[u8] = include_bytes!("../../../../examples/assets/panels.png");
const JPEG: &[u8] = include_bytes!("../../../../examples/assets/sun.jpg");
mod packed;

#[test]
fn prepared_texture_worker_preserves_blocks_and_uses_bounded_workspace() {
    use reel_core::texture::*;
    for format in [
        TextureFormat::Rgba8,
        TextureFormat::Bc1Rgb,
        TextureFormat::Bc3Rgba,
    ] {
        let descriptor = TextureDescriptor {
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
            let ResourceData::Texture {
                descriptor: actual,
                bytes: data,
            } = lease.data()
            else {
                panic!("prepared texture must retain its block resource kind")
            };
            assert_eq!(*actual, descriptor);
            assert_eq!(data.data(), blocks);
            assert_eq!(lease.data().image_size(), Some([192, 108]));
            assert!(assets.temporary_pool().usage().peak_bytes <= 65536);
            assert_eq!(assets.output_pool().usage().bytes, blocks.len() as u64);
            drop(lease);
            assets.cancel(id);
            assets.maintain();
            assert_eq!(assets.output_pool().usage().bytes, 0);
        }
    }
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
            let ResourceData::Image { image, .. } = lease.data() else {
                panic!("image resource");
            };
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

fn load_until_done(assets: &Assets, id: LoadId) -> Result<ResourceLease, String> {
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
    assert!(matches!(font.data(), ResourceData::Font(_)));
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
fn loose_resource_reuse_resolves_the_current_content_before_sharing() {
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
    std::fs::write(&path, JPEG).unwrap();
    let changed =
        load_until_done(&assets, assets.request("current.png", AssetKind::Image)).unwrap();
    assert_ne!(first.version(), changed.version());
    assert_eq!(changed.data().image_size(), Some([96, 96]));
    assert_eq!(first.data().image_size(), Some([160, 90]));
    drop((first, shared, changed));
    assert_eq!(assets.output_pool().usage().bytes, 0);
    drop(assets);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(root).unwrap();
}

// A deterministic in-memory host drives the contract fixture at admission.
// Image work still uses the production codec adapter, without a filesystem.
struct MemoryHost(MemoryAssetIo, Option<Option<ContentVersion>>);
impl AssetIo for MemoryHost {
    fn capabilities(&self) -> IoCaps {
        self.0.capabilities()
    }
    fn describe(&mut self, request: DescribeRequest) -> Result<IoId, Rejected<DescribeRequest>> {
        let id = self.0.describe(request)?;
        assert!(self.0.complete_next_read());
        Ok(id)
    }
    fn read(&mut self, request: ReadRequest) -> Result<IoId, Rejected<ReadRequest>> {
        let id = self.0.read(request)?;
        assert!(self.0.complete_next_read());
        Ok(id)
    }
    fn replace_save(&mut self, request: SaveWrite) -> Result<IoId, Rejected<SaveWrite>> {
        self.0.replace_save(request)
    }
    fn cancel(&mut self, id: IoId) {
        self.0.cancel(id);
    }
    fn poll(&mut self, out: &mut Vec<IoEvent>, limit: usize) {
        let first = out.len();
        self.0.poll(out, limit);
        if let Some(version) = self.1 {
            for event in &mut out[first..] {
                if let Terminal::Completed(IoPayload::Described(info)) = &mut event.result {
                    info.version = version;
                }
            }
        }
    }
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
            Box::new(MemoryHost(io, version)),
            Box::new(ImageExecutor::new(limits, wake)),
        )
    })
}

#[test]
fn concurrent_images_share_decode_capacity_and_preserve_versions_across_adapters() {
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
            let ResourceData::Image { image, .. } = lease.data() else {
                panic!("image expected")
            };
            assert_eq!([image.width, image.height], size);
        }
        let output = assets.output_pool();
        assert_eq!(output.usage().bytes, (160 * 90 + 96 * 96) * 4);
        assert_eq!(assets.temporary_pool().usage().bytes, 0);
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
        versions[0], versions[1],
        "resource identity must not include storage location"
    );
    assert_eq!(versions[0], versions[2]);
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
            assets.maintain();
            std::thread::sleep(Duration::from_millis(1));
        }
        worker.join().unwrap();
        drop(control);
        assets.maintain();
        assert_eq!(assets.0.borrow().input.usage().bytes, 0);
        assert_eq!(assets.0.borrow().completion.usage().bytes, 0);
        let (control, mut reader) = assets.stream("assets/panels.png").unwrap();
        let waiter = std::thread::spawn(move || reader.open());
        while !assets.needs_poll() {
            std::thread::yield_now();
        }
        control.cancel();
        assert_eq!(
            waiter.join().unwrap().unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
        assets.close();
    }
}
