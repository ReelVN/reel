use super::*;
use crate::assets::AssetOptions;
use reel_ui::resources::{ColorEncoding, ResourceData};
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    time::Duration,
};

fn loader() -> (Images, Receiver<()>) {
    configured(256 << 20, 256 << 20)
}
fn configured(resident_bytes: u64, decode_bytes: u64) -> (Images, Receiver<()>) {
    let mut images = Images::new(Assets::directory(AssetOptions {
        root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
        resident_bytes,
        decode_bytes,
        ..Default::default()
    }));
    let (send, receive) = mpsc::channel();
    images.set_waker(Arc::new(move || {
        let _ = send.send(());
    }));
    (images, receive)
}
fn wanted(sources: &[&str]) -> BTreeMap<String, Option<String>> {
    sources
        .iter()
        .map(|source| (source.to_string(), Some("main:17:9".into())))
        .collect()
}
fn ready(images: &mut Images, wake: &Receiver<()>) -> Result<(), String> {
    loop {
        if images.poll()? {
            return Ok(());
        }
        wake.recv_timeout(Duration::from_secs(10))
            .expect("decoder must wake its host");
    }
}
fn pixels(lease: &ResourceLease) -> &reel_ui::peniko::ImageData {
    let ResourceData::Image { image, encoding } = lease.data() else {
        panic!("expected image")
    };
    assert_eq!(*encoding, ColorEncoding::Srgb);
    image
}

#[test]
fn real_images_share_storage_replace_atomically_and_release_after_frame_leases() {
    let (mut images, wake) = loader();
    images.request(wanted(&["assets/panels.png", "assets/panels.png"]));
    ready(&mut images, &wake).unwrap();
    images.commit();
    let submitted_frame = images.loaded["assets/panels.png"].clone();
    let panel = pixels(&submitted_frame);
    assert_eq!([panel.width, panel.height], [160, 90]);
    assert_eq!(&panel.data.as_ref()[0..4], &[224, 64, 48, 255]);
    assert_eq!(&panel.data.as_ref()[80 * 4..81 * 4], &[32, 112, 224, 128]);
    images.request(wanted(&["assets/panels.png"]));
    ready(&mut images, &wake).unwrap();
    assert_eq!(images.loads, 1);
    assert_eq!(
        pixels(&images.loaded["assets/panels.png"]).data.id(),
        panel.data.id()
    );
    assert_eq!(images.pool.usage().bytes, 160 * 90 * 4);

    images.request(wanted(&["assets/sun.jpg"]));
    assert!(!images.poll().unwrap());
    assert!(
        !submitted_frame.is_retired(),
        "old complete scene remains publishable while loading"
    );
    assert!(images.loaded.contains_key("assets/panels.png"));
    ready(&mut images, &wake).unwrap();
    assert!(
        !submitted_frame.is_retired(),
        "readiness alone must not retire the visible scene"
    );
    let sun = pixels(&images.loaded["assets/sun.jpg"]);
    assert_eq!([sun.width, sun.height], [96, 96]);
    for (offset, expected) in [
        (0, [20_i16, 48, 88, 255]),
        ((48 * 96 + 48) * 4, [255, 200, 48, 255]),
    ] {
        let actual = &sun.data.as_ref()[offset..offset + 4];
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(actual, expected)| (i16::from(*actual) - expected).abs() <= 8),
            "JPEG pixel={actual:?}, expected={expected:?}"
        );
    }
    images.commit();
    assert_eq!(images.loads, 2);
    assert!(!submitted_frame.is_retired());
    assert!(!images.loaded.contains_key("assets/panels.png"));
    assert_eq!(
        &pixels(&submitted_frame).data.as_ref()[0..4],
        &[224, 64, 48, 255]
    );
    let mut other = Images::new(images.assets.clone());
    other.request(wanted(&["assets/panels.png"]));
    ready(&mut other, &wake).unwrap();
    other.commit();
    assert_eq!(
        other.loaded["assets/panels.png"].handle(),
        submitted_frame.handle()
    );
    images.close();
    assert!(!other.loaded["assets/panels.png"].is_retired());
    assert_eq!(images.pool.usage().bytes, 160 * 90 * 4);
    other.close();
    assert_eq!(images.pool.usage().bytes, 160 * 90 * 4);
    drop(submitted_frame);
    assert_eq!(images.pool.usage().bytes, 0);
    assert_eq!(images.work.usage().bytes, 0);
    eprintln!(
        "decodes={}, resident={:?}, decoding={:?}",
        images.loads,
        images.pool.usage(),
        images.work.usage()
    );
}

#[test]
fn worker_errors_keep_constructor_location_and_cancelled_completions_cannot_resurrect() {
    for (source, resident, decode, expected) in [
        ("assets/missing.png", 1 << 20, 1 << 20, "image"),
        (
            "author_images/src/main.aside",
            1 << 20,
            1 << 20,
            "expected PNG or JPEG",
        ),
        ("assets/panels.png", 1, 1 << 20, "resident budget"),
        ("assets/panels.png", 1 << 20, 1, "limit"),
    ] {
        let (mut images, wake) = configured(resident, decode);
        images.request(wanted(&[source]));
        let error = ready(&mut images, &wake).unwrap_err();
        assert!(
            error.contains("main:17:9") && error.contains(source) && error.contains(expected),
            "{error}"
        );
        assert_eq!(images.pool.usage().bytes, 0);
        assert_eq!(images.work.usage().bytes, 0);
    }
    let (mut images, wake) = loader();
    images.request(wanted(&["assets/panels.png"]));
    assert!(!images.poll().unwrap());
    // Leave a successful decoded lease queued, then withdraw its request before
    // polling. This tests an actual late result, rather than only early IO cancellation.
    loop {
        wake.recv_timeout(Duration::from_secs(10)).unwrap();
        if images.pool.usage().bytes == 160 * 90 * 4 && images.work.usage().bytes == 0 {
            break;
        }
        assert!(!images.poll().unwrap());
    }
    assert_eq!(images.pool.usage().bytes, 160 * 90 * 4);
    assert_eq!(images.work.usage().bytes, 0);
    images.request(BTreeMap::new());
    assert!(images.poll().unwrap());
    assert!(images.loaded.is_empty());
    assert_eq!(images.pool.usage().bytes, 0);
    images.request(wanted(&["assets/sun.jpg"]));
    assert!(!images.poll().unwrap());
    let resident = images.pool.clone();
    let working = images.work.clone();
    images.close();
    // Exit notification follows receiver destruction and dropping any late result.
    loop {
        wake.recv_timeout(Duration::from_secs(10)).unwrap();
        if resident.usage().bytes == 0 && working.usage().bytes == 0 {
            break;
        }
    }
    assert!(!images.poll().unwrap());
    assert!(images.loaded.is_empty());
    assert_eq!(resident.usage().bytes, 0);
    assert_eq!(working.usage().bytes, 0);
}

pub(crate) struct ControlledIo(
    pub(crate) std::rc::Rc<std::cell::RefCell<reel_core::test_support::MemoryAssetIo>>,
);
impl reel_core::io::AssetIo for ControlledIo {
    fn capabilities(&self) -> reel_core::io::IoCaps {
        self.0.borrow().capabilities()
    }
    fn describe(
        &mut self,
        request: reel_core::io::DescribeRequest,
    ) -> Result<
        reel_core::identity::IoId,
        reel_core::result::Rejected<reel_core::io::DescribeRequest>,
    > {
        self.0.borrow_mut().describe(request)
    }
    fn read(
        &mut self,
        request: reel_core::io::ReadRequest,
    ) -> Result<reel_core::identity::IoId, reel_core::result::Rejected<reel_core::io::ReadRequest>>
    {
        self.0.borrow_mut().read(request)
    }
    fn replace_save(
        &mut self,
        request: reel_core::io::SaveWrite,
    ) -> Result<reel_core::identity::IoId, reel_core::result::Rejected<reel_core::io::SaveWrite>>
    {
        self.0.borrow_mut().replace_save(request)
    }
    fn cancel(&mut self, id: reel_core::identity::IoId) {
        self.0.borrow_mut().cancel(id);
    }
    fn poll(&mut self, out: &mut Vec<reel_core::io::IoEvent>, limit: usize) {
        self.0.borrow_mut().poll(out, limit);
    }
}

#[test]
fn batch_image_reads_fill_existing_queue_and_superseded_results_retire() {
    use reel_core::{
        budget::{BudgetCategory, OwnedBytes, QueueLimits},
        identity::{AssetKey, ContentVersion, ResourceVersion},
        test_support::{MemoryAsset, MemoryAssetIo},
    };
    use std::{cell::RefCell, rc::Rc, time::Instant};
    const PNG: &[u8] = include_bytes!("../../../../examples/assets/panels.png");
    for supersede in [false, true] {
        let mut host = None;
        let assets = Assets::with_adapters(
            AssetOptions {
                input_bytes: PNG.len() as u64 * 2,
                ..Default::default()
            },
            |wake| {
                let limits = QueueLimits {
                    max_requests: 32,
                    max_bytes: 768 << 20,
                };
                let storage = BudgetPool::new(
                    BudgetCategory::CpuInput,
                    QueueLimits {
                        max_requests: 40,
                        ..limits
                    },
                );
                let mut io = MemoryAssetIo::new(limits, limits, wake.clone());
                for index in 0..40 {
                    io.insert_asset(
                        ResourceVersion {
                            key: AssetKey(format!("{index:02}.png")),
                            version: ContentVersion([index; 32]),
                        },
                        MemoryAsset {
                            bytes: OwnedBytes::new(
                                PNG.to_vec(),
                                storage.reserve(PNG.len() as u64).unwrap(),
                            )
                            .unwrap(),
                            media_type: None,
                        },
                    )
                    .unwrap();
                }
                let io = Rc::new(RefCell::new(io));
                host = Some(io.clone());
                (
                    Box::new(ControlledIo(io)),
                    Box::new(crate::assets::ImageExecutor::new(limits, wake)),
                )
            },
        );
        let host = host.unwrap();
        let mut images = Images::new(assets);
        images.request(
            (0..40)
                .map(|n| (format!("{n:02}.png"), Some("batch:1:1".into())))
                .collect(),
        );
        assert!(!images.poll().unwrap());
        assert_eq!(
            host.borrow().read_usage().requests,
            32,
            "first batch must occupy the existing queue before any completion"
        );
        assert!(!images.poll().unwrap());
        assert_eq!(
            host.borrow().read_usage().requests,
            32,
            "repeated polling must preserve bounded admission"
        );
        while host.borrow_mut().complete_next_read() {}
        if supersede {
            // Descriptions have completed, but their original consumer is already gone.
            images.request(wanted(&["00.png", "39.png"]));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            while host.borrow_mut().complete_next_read() {}
            if images.poll().unwrap() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "bounded batch stalled under input contention"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(images.loaded.len(), if supersede { 2 } else { 40 });
        if supersede {
            assert!(images.loaded.contains_key("00.png"));
            assert!(images.loaded.contains_key("39.png"));
        } else {
            assert_eq!(
                images.loads, 40,
                "each source is decoded once while filling later batches"
            );
        }
        assert_eq!(
            images.pool.usage().bytes,
            images.loaded.len() as u64 * 160 * 90 * 4
        );
        images.close();
        images.assets.maintain();
        assert_eq!(host.borrow().read_usage().requests, 0);
        assert_eq!(images.pool.usage().bytes, 0);
        assert_eq!(images.work.usage().bytes, 0);
    }
}

#[test]
fn encoded_image_batch_waits_for_input_capacity_without_failing() {
    const PNG: &[u8] = include_bytes!("../../../../examples/assets/panels.png");
    let mut images = Images::new(Assets::directory(AssetOptions {
        input_bytes: PNG.len() as u64,
        ..Default::default()
    }));
    let (send, wake) = mpsc::channel();
    images.set_waker(Arc::new(move || {
        let _ = send.send(());
    }));
    images.encoded = ["first", "second", "third"]
        .into_iter()
        .map(|key| (key.into(), Arc::from(PNG)))
        .collect();
    images.request(wanted(&["first", "second", "third"]));
    ready(&mut images, &wake).unwrap();
    assert_eq!(images.loaded.len(), 3);
    assert_eq!(images.loads, 3);
    assert_eq!(images.pool.usage().bytes, 3 * 160 * 90 * 4);
    images.close();
    assert_eq!(images.pool.usage().bytes, 0);
}
