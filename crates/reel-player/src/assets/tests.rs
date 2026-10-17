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

// A deterministic in-memory host drives the contract fixture at admission.
// Image work still uses the production codec adapter, without a filesystem.
struct MemoryHost(MemoryAssetIo);
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
        self.0.poll(out, limit);
    }
}

pub(crate) fn memory_assets() -> Assets {
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
            Box::new(MemoryHost(io)),
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
