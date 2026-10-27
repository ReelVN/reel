use super::*;
use std::{
    io::Read,
    sync::atomic::{AtomicUsize, Ordering},
};

#[test]
fn streams_reject_adapters_without_an_independent_background_reader() {
    let limits = QueueLimits {
        max_requests: 4,
        max_bytes: 1 << 20,
    };
    let assets = Assets::with_adapters(AssetOptions::default(), |wake| {
        (
            Box::new(MemoryAssetIo::new(limits, limits, wake.clone())),
            Box::new(ImageExecutor::new(limits, wake)),
        )
    });
    let error = assets.stream("assets/panels.png").err().unwrap();
    assert!(error.contains("background reader"), "{error}");
    assert_eq!(assets.0.borrow().stream_input.usage().bytes, 0);
    assert_eq!(assets.0.borrow().completion.usage().bytes, 0);
    assert!(!assets.needs_poll());
}

#[test]
fn decoder_reads_do_not_wake_an_idle_primary_io_consumer() {
    let assets = Assets::directory(AssetOptions {
        root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
        input_bytes: 16,
        ..Default::default()
    });
    let wakes = Arc::new(AtomicUsize::new(0));
    let count = wakes.clone();
    assets.set_waker(Arc::new(move || {
        count.fetch_add(1, Ordering::Relaxed);
    }));
    let (control, mut reader) = assets.stream("assets/panels.png").unwrap();
    let (send, receive) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        send.send((reader, bytes))
            .unwrap_or_else(|_| panic!("reader receiver closed"));
    });
    let (reader, bytes) = receive.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(bytes, PNG);
    assert!(control.version().is_some());
    assert_eq!(
        wakes.load(Ordering::Relaxed),
        0,
        "reader completions belong to the decoder"
    );
    assert!(!assets.needs_poll());
    drop(reader);
    drop(control);
    worker.join().unwrap();
    assets.close();
}

#[test]
fn cancel_and_close_release_in_flight_readers_without_a_host_pump() {
    for close in [false, true] {
        let limits = QueueLimits {
            max_requests: 4,
            max_bytes: 1 << 20,
        };
        let mut control = None;
        let assets = Assets::with_adapters(AssetOptions::default(), |wake| {
            let io = MemoryReader::new(
                MemoryAssetIo::new(limits, limits, wake.clone()),
                wake.clone(),
                false,
                None,
            );
            control = Some(io.control());
            (Box::new(io), Box::new(ImageExecutor::new(limits, wake)))
        });
        let io = control.unwrap();
        let (control, mut reader) = assets.stream("pending").unwrap();
        let (send, receive) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            send.send(reader.open()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while io.lock().unwrap().read_usage().pending == 0 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        if close {
            assets.close();
        } else {
            control.cancel();
        }
        assert_eq!(
            receive
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::BrokenPipe
        );
        worker.join().unwrap();
        assert_eq!(io.lock().unwrap().read_usage().requests, 0);
        assert_eq!(assets.0.borrow().completion.usage().bytes, 0);
        assert_eq!(assets.0.borrow().stream_input.usage().bytes, 0);
        assets.close();
    }
}

#[test]
fn admitted_reader_reuses_its_completion_while_the_configurable_host_pool_is_full() {
    let limits = QueueLimits {
        max_requests: 4096,
        max_bytes: 4 << 20,
    };
    let assets = Assets::with_adapters(AssetOptions::default(), |wake| {
        let input = BudgetPool::new(BudgetCategory::CpuInput, limits);
        let mut io = MemoryAssetIo::new(limits, limits, wake.clone());
        io.insert_asset(
            ResourceVersion {
                key: AssetKey("asset".into()),
                version: ContentVersion([7; 32]),
            },
            MemoryAsset {
                bytes: OwnedBytes::new(PNG.to_vec(), input.reserve(PNG.len() as u64).unwrap())
                    .unwrap(),
                media_type: None,
            },
        )
        .unwrap();
        (
            Box::new(MemoryReader::new(io, wake.clone(), true, None)),
            Box::new(ImageExecutor::new(limits, wake)),
        )
    });
    let (control, mut reader) = assets.stream("asset").unwrap();
    let completion = assets.0.borrow().completion.clone();
    assert_eq!(completion.usage().bytes, 512);
    let occupied = completion.reserve((1 << 20) - 512).unwrap();
    assert!(assets.stream("asset").err().unwrap().contains("capacity"));
    let (send, receive) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        send.send(bytes).unwrap();
    });
    assert_eq!(receive.recv_timeout(Duration::from_secs(3)).unwrap(), PNG);
    worker.join().unwrap();
    assert_eq!(completion.usage().bytes, (1 << 20) - 512);
    drop(control);
    drop(occupied);
    assert_eq!(completion.usage().bytes, 0);
    assets.close();
}
