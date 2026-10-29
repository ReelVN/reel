use super::*;
use reel_core::{identity::Priority, io::IoErrorCode, result::RejectCode};
use reel_package::archive::{ArchiveKind, ArchiveWriter};
use std::{
    fs::File,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Instant,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
pub(super) struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "reel-pack-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn pack(&self, name: &str, kind: ArchiveKind, entries: &[(&str, &[u8])]) -> PathBuf {
        let path = self.0.join(name);
        let mut writer = ArchiveWriter::new(File::create(&path).unwrap(), kind).unwrap();
        for &(key, mut bytes) in entries {
            writer.add(key, &mut bytes).unwrap();
        }
        writer.finish().unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
pub(super) fn packaged_assets(input_bytes: u64) -> (Assets, Fixture) {
    let fixture = Fixture::new();
    let path = fixture.pack(
        "assets.rpk",
        ArchiveKind::Resources,
        &[("assets/panels.png", PNG), ("assets/sun.jpg", JPEG)],
    );
    let assets = Assets::packaged(
        AssetOptions {
            input_bytes,
            ..Default::default()
        },
        [path],
        None,
    )
    .unwrap();
    (assets, fixture)
}

const LIMITS: QueueLimits = QueueLimits {
    max_requests: 16,
    max_bytes: 64 << 10,
};
fn source(key: &str, version: Option<ContentVersion>) -> AssetSource {
    AssetSource {
        key: AssetKey(key.into()),
        version,
    }
}
struct Requests {
    completion: BudgetPool,
    output: BudgetPool,
    temporary: BudgetPool,
    metadata: BudgetPool,
}
impl Requests {
    fn new() -> Self {
        Self {
            completion: BudgetPool::new(BudgetCategory::Completion, LIMITS),
            output: BudgetPool::new(BudgetCategory::CpuInput, LIMITS),
            temporary: BudgetPool::new(
                BudgetCategory::Temporary,
                QueueLimits {
                    max_requests: 1,
                    max_bytes: reel_package::archive::SCRATCH_BYTES as u64,
                },
            ),
            metadata: BudgetPool::new(
                BudgetCategory::Metadata,
                QueueLimits {
                    max_requests: 64,
                    max_bytes: 32 << 20,
                },
            ),
        }
    }
    fn read(
        &self,
        key: &str,
        offset: u64,
        len: u32,
        version: Option<ContentVersion>,
        priority: Priority,
    ) -> ReadRequest {
        ReadRequest {
            deadline: None,
            resource: source(key, version),
            offset,
            len,
            priority,
            scope: Scope::Process,
            completion: self.completion.reserve(512).unwrap(),
            output: self.output.reserve(u64::from(len)).unwrap(),
        }
    }
    fn describe(&self, key: &str) -> DescribeRequest {
        DescribeRequest {
            resource: source(key, None),
            scope: Scope::Process,
            priority: Priority::Normal,
            completion: self.completion.reserve(512).unwrap(),
        }
    }
    fn settled(&self) {
        assert_eq!(self.completion.usage().bytes, 0);
        assert_eq!(self.output.usage().bytes, 0);
    }
}
fn events(
    io: &mut PackIo,
    wake: &WakeSignal,
    receiver: &mpsc::Receiver<()>,
    count: usize,
) -> Vec<IoEvent> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut out = Vec::new();
    while out.len() < count {
        wake.acknowledge();
        io.poll(&mut out, count);
        if out.len() < count {
            receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
        }
    }
    out
}
fn bytes(event: &IoEvent) -> &[u8] {
    let Terminal::Completed(IoPayload::Read(read)) = &event.result else {
        panic!("{:?}", event.result)
    };
    read.bytes.as_slice()
}

fn wait_for_pool_release(pool: &BudgetPool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while pool.usage().bytes != 0 {
        assert!(
            Instant::now() < deadline,
            "closed IO worker retained its physical storage"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn raw_archive_metadata_is_admitted_and_retained_until_the_io_worker_exits() {
    let fixture = Fixture::new();
    let path = fixture.pack("raw.rpk", ArchiveKind::Resources, &[("bytes", b"raw")]);
    let requests = Requests::new();
    assert_eq!(requests.metadata.usage().bytes, 0);
    let io = PackIo::open(
        [path],
        None,
        "vita".into(),
        &requests.temporary,
        &requests.metadata,
        LIMITS,
        WakeSignal::new(|| {}),
    )
    .unwrap();
    let usage = requests.metadata.usage();
    assert!(usage.bytes > 0);
    assert_eq!(usage.tickets, 2, "one index and one mount collection");
    assert!(
        usage.peak_bytes > usage.bytes,
        "the parser releases its bounded input buffer"
    );
    assert_eq!(
        requests.temporary.usage().bytes,
        0,
        "raw mounts need no decoder workspace"
    );
    drop(io);
    wait_for_pool_release(&requests.metadata);
    assert_eq!(requests.metadata.usage().tickets, 0);
}

#[test]
fn rejected_metadata_and_later_invalid_mounts_release_every_prior_reservation() {
    use reel_package::archive::ArchiveIndex;
    let fixture = Fixture::new();
    let path = fixture.pack("valid.rpk", ArchiveKind::Resources, &[("bytes", b"raw")]);
    let index_bytes = ArchiveIndex::read(&mut File::open(&path).unwrap())
        .unwrap()
        .capacity_bytes();
    let requests = Requests::new();
    let insufficient = BudgetPool::new(
        BudgetCategory::Metadata,
        QueueLimits {
            max_requests: 8,
            max_bytes: index_bytes - 1,
        },
    );
    assert!(
        PackIo::open(
            [path.clone()],
            None,
            "vita".into(),
            &requests.temporary,
            &insufficient,
            LIMITS,
            WakeSignal::new(|| {})
        )
        .is_err()
    );
    assert_eq!(insufficient.usage().bytes, 0);
    assert_eq!(insufficient.usage().tickets, 0);
    let invalid = fixture.0.join("invalid.rpk");
    std::fs::write(&invalid, b"not an archive").unwrap();
    assert!(
        PackIo::open(
            [path, invalid],
            None,
            "vita".into(),
            &requests.temporary,
            &requests.metadata,
            LIMITS,
            WakeSignal::new(|| {})
        )
        .is_err()
    );
    assert_eq!(requests.metadata.usage().bytes, 0);
    assert_eq!(requests.metadata.usage().tickets, 0);
    assert_eq!(requests.temporary.usage().bytes, 0);
}

#[test]
fn pack_mount_order_explicit_overlay_and_range_versions_use_one_io_contract() {
    let fixture = Fixture::new();
    let base = fixture.pack(
        "base.rpk",
        ArchiveKind::Resources,
        &[("asset", b"original"), ("base-only", b"base")],
    );
    let patch = fixture.pack(
        "patch.rtp",
        ArchiveKind::Text,
        &[
            ("asset", b"patched"),
            ("locale/chapter.ftl", b"chapter = translation"),
        ],
    );
    std::fs::write(fixture.0.join("asset"), b"loose-file").unwrap();
    for overlay in [None, Some(fixture.0.clone())] {
        let (sender, receiver) = mpsc::channel();
        let wake = WakeSignal::new(move || {
            let _ = sender.send(());
        });
        let requests = Requests::new();
        let mut io = PackIo::open(
            [base.clone(), patch.clone()],
            overlay.clone(),
            std::env::consts::OS.into(),
            &requests.temporary,
            &requests.metadata,
            LIMITS,
            wake.clone(),
        )
        .unwrap();
        let expected: &[u8] = if overlay.is_some() {
            b"loose-file"
        } else {
            b"patched"
        };
        io.describe(requests.describe("asset")).unwrap();
        let described = events(&mut io, &wake, &receiver, 1);
        let Terminal::Completed(IoPayload::Described(info)) = &described[0].result else {
            panic!()
        };
        let version = info.version.expect("selected fixed source has an identity");
        if overlay.is_none() {
            assert_eq!(version, ContentVersion(*blake3::hash(expected).as_bytes()));
        }
        assert_eq!(info.length, Some(expected.len() as u64));
        assert!(info.range_read && info.seekable);
        drop(described);
        io.read(requests.read("asset", 2, 3, Some(version), Priority::Normal))
            .unwrap();
        io.read(requests.read("base-only", 0, 100, None, Priority::Normal))
            .unwrap();
        io.read(requests.read("locale/chapter.ftl", 0, 100, None, Priority::Normal))
            .unwrap();
        let reads = events(&mut io, &wake, &receiver, 3);
        assert_eq!(bytes(&reads[0]), &expected[2..5]);
        assert_eq!(bytes(&reads[1]), b"base");
        assert_eq!(bytes(&reads[2]), b"chapter = translation");
        drop(reads);
        // A pinned version never selects an older pack or bypasses an overriding loose file.
        io.read(requests.read(
            "asset",
            0,
            1,
            Some(ContentVersion(*blake3::hash(b"original").as_bytes())),
            Priority::Normal,
        ))
        .unwrap();
        io.read(requests.read("../asset", 0, 1, None, Priority::Normal))
            .unwrap();
        io.read(requests.read(
            "asset",
            expected.len() as u64 + 1,
            1,
            None,
            Priority::Normal,
        ))
        .unwrap();
        io.read(requests.read(
            "asset",
            expected.len() as u64,
            1,
            Some(version),
            Priority::Normal,
        ))
        .unwrap();
        let failures = events(&mut io, &wake, &receiver, 4);
        assert!(
            matches!(&failures[0].result, Terminal::Failed(e) if e.code == IoErrorCode::VersionUnavailable)
        );
        assert!(matches!(&failures[1].result, Terminal::Failed(_)));
        assert!(matches!(&failures[2].result, Terminal::Failed(_)));
        assert!(
            matches!(&failures[3].result, Terminal::Completed(IoPayload::Read(r)) if r.eof && r.bytes.as_slice().is_empty())
        );
        drop(failures);
        requests.settled();
        assert_eq!(
            requests.temporary.usage().bytes,
            0,
            "raw archives allocate no workspace"
        );
    }
}

#[test]
fn packed_reads_preserve_priority_cancel_queue_backpressure_and_budget_ownership() {
    let fixture = Fixture::new();
    let path = fixture.pack("assets.rpk", ArchiveKind::Resources, &[("asset", b"data")]);
    let (sender, receiver) = mpsc::channel();
    let (resume, gate) = mpsc::channel();
    let gate = Mutex::new(gate);
    let first = AtomicBool::new(true);
    let wake = WakeSignal::new(move || {
        let _ = sender.send(());
        // Gate the actual IO worker after its first completion, so all subsequent
        // priorities/cancellation are queued before it chooses the next request.
        if first.swap(false, Ordering::AcqRel) {
            gate.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
    });
    let requests = Requests::new();
    let mut io = PackIo::open(
        [path],
        None,
        std::env::consts::OS.into(),
        &requests.temporary,
        &requests.metadata,
        QueueLimits {
            max_requests: 5, // Three ordinary owners plus two reserved media slots.
            ..LIMITS
        },
        wake.clone(),
    )
    .unwrap();
    io.describe(requests.describe("asset")).unwrap();
    receiver.recv_timeout(Duration::from_secs(10)).unwrap();
    drop(events(&mut io, &wake, &receiver, 1));
    let low = io
        .read(requests.read("asset", 0, 4, None, Priority::Prefetch))
        .unwrap();
    let normal = io
        .read(requests.read("asset", 0, 4, None, Priority::Normal))
        .unwrap();
    let visible = io
        .read(requests.read("asset", 0, 4, None, Priority::Visible))
        .unwrap();
    let rejected = io
        .read(requests.read("asset", 0, 4, None, Priority::Normal))
        .unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    drop(rejected);
    let media_a = io
        .read(requests.read("asset", 0, 4, None, Priority::Realtime))
        .unwrap();
    let media_b = io
        .read(requests.read("asset", 0, 4, None, Priority::Realtime))
        .unwrap();
    let rejected = io
        .read(requests.read("asset", 0, 4, None, Priority::Realtime))
        .unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    drop(rejected);
    io.cancel(low);
    resume.send(()).unwrap();
    let completed = events(&mut io, &wake, &receiver, 5);
    assert_eq!(
        completed.iter().map(|e| e.id).collect::<Vec<_>>(),
        [media_a, media_b, visible, normal, low]
    );
    assert_eq!(bytes(&completed[0]), b"data");
    assert_eq!(bytes(&completed[1]), b"data");
    assert_eq!(bytes(&completed[2]), b"data");
    assert_eq!(bytes(&completed[3]), b"data");
    assert!(matches!(completed[4].result, Terminal::Cancelled));
    assert_eq!(
        requests.output.usage().bytes,
        16,
        "completed consumers own all four read tickets"
    );
    drop(completed);
    requests.settled();
}

#[test]
fn media_preempts_large_packed_reads_between_quanta_without_reallocating_output() {
    let fixture = Fixture::new();
    let payload: Vec<u8> = (0..3 * reel_package::archive::BLOCK_BYTES + 17)
        .map(|i| (i / 51) as u8)
        .collect();
    let path = fixture.pack(
        "fair.rpk",
        ArchiveKind::Resources,
        &[("large", &payload), ("media", b"music")],
    );
    verify_preemption(path, "large", "media");
}

fn verify_preemption(path: PathBuf, large_key: &str, media_key: &str) {
    let index = reel_package::archive::ArchiveIndex::read(&mut File::open(&path).unwrap()).unwrap();
    let large_entry = index.get(large_key).unwrap();
    let media_entry = index.get(media_key).unwrap();
    let large_length = usize::try_from(large_entry.length).unwrap();
    let media_length = media_entry.length.min(64 << 10) as usize;
    let mut expected_media = vec![0; media_length];
    media_entry
        .read_range(
            &mut File::open(&path).unwrap(),
            0,
            &mut expected_media,
            &mut reel_package::archive::ReadScratch::new(),
            || false,
        )
        .unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let gate = Mutex::new(resume_rx);
    let observations = Arc::new(Mutex::new(Vec::new()));
    let seen = observations.clone();
    let first = AtomicBool::new(true);
    let (sender, receiver) = mpsc::channel();
    let wake = WakeSignal::new(move || {
        let _ = sender.send(());
    });
    let limits = QueueLimits {
        max_requests: 4,
        max_bytes: 24 << 20,
    };
    let output = BudgetPool::new(BudgetCategory::CpuInput, limits);
    let completion = BudgetPool::new(BudgetCategory::Completion, limits);
    let temporary = BudgetPool::new(BudgetCategory::Temporary, limits);
    let metadata = BudgetPool::new(BudgetCategory::Metadata, limits);
    let mut io = PackIo::observed(
        [path],
        &temporary,
        &metadata,
        limits,
        wake.clone(),
        Arc::new(move |id, length, pointer, seeks| {
            seen.lock().unwrap().push((id, length, pointer, seeks));
            if first.swap(false, Ordering::AcqRel) {
                started_tx.send(()).unwrap();
                gate.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
            }
        }),
    )
    .unwrap();
    let read = |key: &str, length: usize, priority| ReadRequest {
        deadline: None,
        resource: source(key, None),
        offset: 0,
        len: length as u32,
        priority,
        scope: Scope::Process,
        output: output.reserve(length as u64).unwrap(),
        completion: completion.reserve(512).unwrap(),
    };
    let large = io
        .read(read(large_key, large_length, Priority::Visible))
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    // Fill all ordinary slots while the first large quantum owns its one buffer.
    let other = io
        .read(read(media_key, media_length, Priority::Visible))
        .unwrap();
    let rejected = io
        .read(read(media_key, media_length, Priority::Visible))
        .unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    drop(rejected);
    let media = io
        .read(read(media_key, media_length, Priority::Realtime))
        .unwrap();
    resume_tx.send(()).unwrap();
    let result = events(&mut io, &wake, &receiver, 3);
    assert_eq!(
        result.iter().map(|event| event.id).collect::<Vec<_>>(),
        [media, other, large]
    );
    assert_eq!(bytes(&result[0]), expected_media);
    assert_eq!(
        blake3::hash(bytes(&result[2])).as_bytes(),
        &large_entry.version
    );
    let observations = observations.lock().unwrap();
    let media_at = observations
        .iter()
        .position(|(id, ..)| *id == media)
        .unwrap();
    let second_large = observations
        .iter()
        .position(|(id, length, ..)| *id == large && *length > 65536)
        .unwrap();
    assert!(media_at < second_large);
    let large_steps: Vec<_> = observations
        .iter()
        .filter(|(id, ..)| *id == large)
        .collect();
    assert_eq!(
        large_steps.iter().map(|(_, n, ..)| *n).collect::<Vec<_>>(),
        (1..=large_length.div_ceil(65536))
            .map(|step| (step * 65536).min(large_length))
            .collect::<Vec<_>>()
    );
    assert!(
        large_steps
            .iter()
            .all(|(_, _, pointer, _)| *pointer == large_steps[0].2)
    );
    assert_eq!(
        large_steps.last().unwrap().3 - large_steps[1].3,
        0,
        "after returning from media, contiguous image quanta issue no extra native seeks"
    );
    drop(result);
    assert_eq!(output.usage().bytes, 0);
    assert_eq!(completion.usage().bytes, 0);
    eprintln!(
        "packed read {large_key}: media finishes between large blocks1/2; ordinary full preserves reserved admission; {} quanta share one output allocation; input peak={} cursor={} scratch={} native_seeks first={},resumed={},last={}",
        large_steps.len(),
        output.usage().peak_bytes,
        size_of::<reel_package::archive::ReadCursor>(),
        reel_package::archive::SCRATCH_BYTES,
        large_steps[0].3,
        large_steps[1].3,
        large_steps.last().unwrap().3
    );
    drop(io);
    wait_for_pool_release(&temporary);
}

#[test]
#[ignore = "uses an existing real RPK; set REEL_RPK_PATH"]
fn real_rpk_media_preempts_large_reads_through_the_actual_io_worker() {
    let path = PathBuf::from(std::env::var("REEL_RPK_PATH").expect("set REEL_RPK_PATH"));
    let index = reel_package::archive::ArchiveIndex::read(&mut File::open(&path).unwrap()).unwrap();
    let large = index
        .entries()
        .filter(|entry| entry.path.ends_with(".png"))
        .max_by_key(|entry| entry.length)
        .unwrap();
    let media = index
        .entries()
        .find(|entry| {
            [".mp3", ".ogg", ".wav"]
                .iter()
                .any(|suffix| entry.path.ends_with(suffix))
                && entry.length >= 65536
        })
        .unwrap();
    verify_preemption(path, large.path, media.path);
}

#[test]
fn compressed_ranges_share_one_workspace_and_release_it_with_the_worker() {
    use reel_package::archive::{BLOCK_BYTES, SCRATCH_BYTES};
    let fixture = Fixture::new();
    let content: Vec<u8> = (0..3 * BLOCK_BYTES).map(|n| (n / 47) as u8).collect();
    let path = fixture.pack("assets.rpk", ArchiveKind::Resources, &[("data", &content)]);
    let requests = Requests::new();
    let (sender, receiver) = mpsc::channel();
    let wake = WakeSignal::new(move || {
        let _ = sender.send(());
    });
    let insufficient = BudgetPool::new(
        BudgetCategory::Temporary,
        QueueLimits {
            max_requests: 1,
            max_bytes: SCRATCH_BYTES as u64 - 1,
        },
    );
    assert!(
        PackIo::open(
            [path.clone()],
            None,
            "vita".into(),
            &insufficient,
            &requests.metadata,
            LIMITS,
            wake.clone()
        )
        .is_err()
    );
    assert_eq!(insufficient.usage().bytes, 0);
    let mut io = PackIo::open(
        [path],
        None,
        "vita".into(),
        &requests.temporary,
        &requests.metadata,
        LIMITS,
        wake.clone(),
    )
    .unwrap();
    assert_eq!(requests.temporary.usage().bytes, SCRATCH_BYTES as u64);
    let version = ContentVersion(*blake3::hash(&content).as_bytes());
    for offset in [BLOCK_BYTES - 31, 2 * BLOCK_BYTES + 7, 17] {
        io.read(requests.read("data", offset as u64, 1024, Some(version), Priority::Normal))
            .unwrap();
        let read = events(&mut io, &wake, &receiver, 1);
        assert_eq!(bytes(&read[0]), &content[offset..offset + 1024]);
    }
    requests.settled();
    assert_eq!(requests.temporary.usage().peak_bytes, SCRATCH_BYTES as u64);
    drop(io);
    wait_for_pool_release(&requests.temporary);
}

#[test]
fn compressed_image_decoding_stays_within_the_shared_temporary_allowance() {
    use image::ImageEncoder;
    use reel_package::archive::SCRATCH_BYTES;
    let fixture = Fixture::new();
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
    let path = fixture.pack(
        "assets.rpk",
        ArchiveKind::Resources,
        &[("image.png", &encoded)],
    );
    for (decoder_bytes, succeeds) in [(64 << 10, true), (1024, false)] {
        let limit = SCRATCH_BYTES as u64 + decoder_bytes;
        let assets = Assets::packaged(
            AssetOptions {
                decode_bytes: limit,
                ..Default::default()
            },
            [path.clone()],
            None,
        )
        .unwrap();
        let pool = assets.temporary_pool();
        let id = assets.request("image.png", AssetKind::Image);
        let result = load_until_done(&assets, id);
        if succeeds {
            let lease = result.unwrap();
            let image = crate::test_image::data(&lease);
            assert_eq!(image.data.as_ref(), pixels);
        } else {
            assert!(
                result.is_err(),
                "archive and decoder must share the configured allowance"
            );
        }
        assets.cancel(id);
        assert!(pool.usage().peak_bytes <= limit);
        assert_eq!(pool.usage().bytes, SCRATCH_BYTES as u64);
        drop(assets);
        wait_for_pool_release(&pool);
    }
}
