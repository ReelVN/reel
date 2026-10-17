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
}
impl Requests {
    fn new() -> Self {
        Self {
            completion: BudgetPool::new(BudgetCategory::Completion, LIMITS),
            output: BudgetPool::new(BudgetCategory::CpuInput, LIMITS),
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
        let mut io = PackIo::open(
            [base.clone(), patch.clone()],
            overlay.clone(),
            LIMITS,
            wake.clone(),
        )
        .unwrap();
        let requests = Requests::new();
        let expected: &[u8] = if overlay.is_some() {
            b"loose-file"
        } else {
            b"patched"
        };
        let version = ContentVersion(*blake3::hash(expected).as_bytes());
        io.describe(requests.describe("asset")).unwrap();
        let described = events(&mut io, &wake, &receiver, 1);
        let Terminal::Completed(IoPayload::Described(info)) = &described[0].result else {
            panic!()
        };
        assert_eq!(info.version, Some(version));
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
    let mut io = PackIo::open(
        [path],
        None,
        QueueLimits {
            max_requests: 3,
            ..LIMITS
        },
        wake.clone(),
    )
    .unwrap();
    let requests = Requests::new();
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
    io.cancel(low);
    resume.send(()).unwrap();
    let completed = events(&mut io, &wake, &receiver, 3);
    assert_eq!(
        completed.iter().map(|e| e.id).collect::<Vec<_>>(),
        [visible, normal, low]
    );
    assert_eq!(bytes(&completed[0]), b"data");
    assert_eq!(bytes(&completed[1]), b"data");
    assert!(matches!(completed[2].result, Terminal::Cancelled));
    assert_eq!(
        requests.output.usage().bytes,
        8,
        "completed consumers own the two read tickets"
    );
    drop(completed);
    requests.settled();
}
