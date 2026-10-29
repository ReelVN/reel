use super::*;
use reel_audio::{
    info::MockInfoBuilder,
    sound::{SoundData, static_sound::QueuedStaticSound},
};
use reel_core::budget::{BudgetCategory, QueueLimits};

fn fixture(capacity: usize, length: usize) -> (Arc<Lease>, Writer, BudgetPool) {
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 1,
            max_bytes: 1 << 20,
        },
    );
    let budget = Arc::new(Residency {
        ticket: pool
            .reserve(storage_bytes(capacity).unwrap() + 1024)
            .unwrap(),
        release: Mutex::new(None),
    });
    let window = Arc::new(Window {
        io_deadline: None,
        sample_rate: 48000,
        capacity,
        needs_block: AtomicBool::new(false),
        status: Arc::new(StreamStatus::new(length)),
        wanted: AtomicUsize::new(0),
        serial: AtomicU64::new(0),
        stopped: Arc::new(AtomicBool::new(false)),
        control: Arc::new(Control::new().unwrap()),
        _residency: budget,
    });
    let (reader, writer) = channels(capacity);
    (
        Arc::new(Lease {
            reader: AtomicRefCell::new(SyncWrapper::new(reader)),
            window,
            cancel: Arc::new(|| {}),
        }),
        writer,
        pool,
    )
}
fn frames(index: usize, bias: usize) -> Frame {
    Frame::new((index + bias) as f32, -((index + bias) as f32))
}
fn fill(writer: &mut Writer, source: &Lease, data: &[Frame]) {
    let mut next = 0;
    while next < data.len() {
        let mut block = writer.take().expect("preallocated source block");
        let count = block.frames.len().min(data.len() - next);
        block.frames[..count].copy_from_slice(&data[next..next + count]);
        block.start = next;
        block.len = count;
        block.serial = source.window.serial.load(Ordering::Acquire);
        writer.publish(block);
        next += count;
    }
}
#[test]
fn retained_blocks_are_random_access_and_old_seek_publications_are_rejected() {
    let (source, mut writer, pool) = fixture(260, 1024);
    let data: Vec<_> = (0..256).map(|n| frames(n, 0)).collect();
    fill(&mut writer, &source, &data);
    let mut out = [Frame::ZERO; 128];
    assert_eq!(source.read_frames(0, &mut out), 128);
    assert_eq!(source.read_frames(128, &mut out), 128);
    assert_eq!(source.read_frames(30, &mut out[..16]), 16);
    assert_eq!(&out[..16], &data[30..46]);
    source.seek(30);
    assert_eq!(
        source.window.serial.load(Ordering::Acquire),
        0,
        "cached target reuses its prepared generation"
    );
    source.window.serial.fetch_add(1, Ordering::AcqRel);
    assert_eq!(source.read_frames(64, &mut out), 0);
    let changed: Vec<_> = (0..256).map(|n| frames(n, 10000)).collect();
    fill(&mut writer, &source, &changed);
    let count = source.read_frames(64, &mut out);
    assert!(count > 0);
    assert_eq!(&out[..count], &changed[64..64 + count]);
    let old = source.window.serial.load(Ordering::Acquire);
    source.seek(512);
    let fresh = source.window.serial.load(Ordering::Acquire);
    assert_ne!(fresh, old, "uncached target starts another generation");
    assert_eq!(source.read_frames(512, &mut out), 0);
    for (serial, bias, expected) in [(old, 20000, 0), (fresh, 30000, 1)] {
        let mut block = writer.take().unwrap();
        block.start = 512;
        block.len = 1;
        block.serial = serial;
        block.frames[0] = frames(512, bias);
        writer.publish(block);
        assert_eq!(source.read_frames(512, &mut out[..1]), expected);
        if expected != 0 {
            assert_eq!(out[0], frames(512, bias));
        }
    }
    drop(writer);
    drop(source);
    assert_eq!(pool.usage().bytes, 0);
}
#[test]
fn full_recycling_queue_keeps_the_original_box_until_a_slot_is_reserved() {
    let (mut reader, mut writer) = channels(128);
    let block = writer.take().unwrap();
    let pointer = block.frames.as_ptr();
    reader.current = Some(block);
    // Inject a foreign credit to exercise the full path despite the normal pool invariant.
    reader
        .recycled
        .push(Block {
            start: 0,
            len: 0,
            serial: 0,
            frames: vec![Frame::ZERO; 128].into_boxed_slice(),
        })
        .unwrap_or_else(|_| panic!("empty recycle queue"));
    assert!(!reader.recycle_current());
    assert_eq!(reader.current.as_ref().unwrap().frames.as_ptr(), pointer);
    drop(writer.take().unwrap());
    assert!(reader.recycle_current());
    assert_eq!(writer.take().unwrap().frames.as_ptr(), pointer);
}
#[test]
fn history_returns_space_when_the_worker_needs_another_block() {
    let (source, mut writer, _) = fixture(256, 1024);
    let data: Vec<_> = (0..256).map(|n| frames(n, 0)).collect();
    fill(&mut writer, &source, &data);
    let mut out = [Frame::ZERO; 128];
    assert_eq!(source.read_frames(0, &mut out), 128);
    assert_eq!(source.read_frames(128, &mut out), 128);
    assert!(writer.take().is_none());
    source.window.needs_block.store(true, Ordering::Release);
    assert_eq!(source.read_frames(192, &mut out[..64]), 64);
    let mut block = writer.take().expect("history returned one physical owner");
    for (n, slot) in block.frames.iter_mut().enumerate() {
        *slot = frames(256 + n, 0);
    }
    block.start = 256;
    block.len = 128;
    block.serial = 0;
    writer.publish(block);
    assert_eq!(source.read_frames(256, &mut out), 128);
    assert_eq!(out[0], frames(256, 0));
}
#[test]
fn shared_random_readers_return_correct_samples_or_none_without_waiting() {
    let (source, mut writer, _) = fixture(260, 256);
    let data: Vec<_> = (0..256).map(|n| frames(n, 0)).collect();
    fill(&mut writer, &source, &data);
    let mut out = [Frame::ZERO; 128];
    assert_eq!(source.read_frames(0, &mut out), 128);
    assert_eq!(source.read_frames(128, &mut out), 128);
    let guard = source.reader.borrow_mut();
    let other = source.clone();
    assert!(
        std::thread::spawn(move || other.frame(4).is_none())
            .join()
            .unwrap()
    );
    drop(guard);
    let readers: Vec<_> = (0..2)
        .map(|seed| {
            let source = source.clone();
            std::thread::spawn(move || {
                let mut success = 0;
                for n in 0..4000 {
                    let index = (n * 23 + seed * 19) % 256;
                    if let Some(frame) = source.frame(index) {
                        assert_eq!(frame, frames(index, 0));
                        success += 1;
                    }
                }
                success
            })
        })
        .collect();
    assert!(
        readers
            .into_iter()
            .map(|reader| reader.join().unwrap())
            .sum::<usize>()
            > 0
    );
}
#[test]
fn bulk_playback_matches_static_pcm_through_loop_rate_pause_and_seek() {
    for (rate, reverse) in [
        (0.25, false),
        (1., false),
        (1.5, false),
        (4., false),
        (0.5, true),
        (2., true),
    ] {
        for looping in [false, true] {
            let (source, mut writer, _) = fixture(1024, 513);
            let data: Vec<_> = (0..513).map(|n| frames(n, 0)).collect();
            fill(&mut writer, &source, &data);
            let make = |frames| {
                StaticSoundData {
                    sample_rate: 44100,
                    frames,
                    settings: Default::default(),
                    slice: None,
                }
                .playback_rate(rate)
                .reverse(reverse)
            };
            let mut a = make(FrameStorage::from(data));
            let mut b = make(FrameStorage::Stream(source.clone()));
            if looping {
                a = a.loop_region(31. / 44100. ..481. / 44100.);
                b = b.loop_region(31. / 44100. ..481. / 44100.);
            }
            let (mut a, mut ah) = a.into_sound().unwrap();
            let (mut b, mut bh) = b.into_sound().unwrap();
            let info = MockInfoBuilder::new().build();
            let mut expected = [Frame::ZERO; 64];
            let mut actual = [Frame::ZERO; 64];
            let mut serial = 0;
            for turn in 0..48 {
                match turn {
                    5 => {
                        ah.pause(Tween {
                            duration: Duration::ZERO,
                            ..Default::default()
                        });
                        bh.pause(Tween {
                            duration: Duration::ZERO,
                            ..Default::default()
                        });
                    }
                    8 => {
                        ah.resume(Tween {
                            duration: Duration::ZERO,
                            ..Default::default()
                        });
                        bh.resume(Tween {
                            duration: Duration::ZERO,
                            ..Default::default()
                        });
                    }
                    12 | 20 if !a.finished() => {
                        let position = if turn == 12 {
                            100. / 44100.
                        } else {
                            24. / 44100.
                        };
                        ah.seek_to(position);
                        bh.seek_to(position);
                    }
                    _ => {}
                }
                a.on_start_processing();
                b.on_start_processing();
                let next = source.window.serial.load(Ordering::Acquire);
                if next != serial {
                    let data: Vec<_> = (0..513).map(|n| frames(n, 0)).collect();
                    fill(&mut writer, &source, &data);
                    serial = next;
                }
                a.process(&mut expected, 1. / 48000., &info);
                b.process(&mut actual, 1. / 48000., &info);
                assert_eq!(
                    actual, expected,
                    "rate={rate} looping={looping} turn={turn}"
                );
                assert_eq!(bh.position(), ah.position());
            }
        }
    }
}
#[test]
fn independent_bulk_sources_keep_cross_rate_queued_boundaries_identical() {
    let (first, mut first_writer, _) = fixture(1024, 257);
    let (second, mut second_writer, _) = fixture(1024, 193);
    let data: Vec<_> = (0..257).map(|n| frames(n, 1)).collect();
    let next: Vec<_> = (0..193).map(|n| frames(n, 1000)).collect();
    fill(&mut first_writer, &first, &data);
    fill(&mut second_writer, &second, &next);
    let make = |frames, rate| StaticSoundData {
        sample_rate: rate,
        frames,
        settings: Default::default(),
        slice: None,
    };
    let (mut a, ah, mut aq) =
        make(FrameStorage::from(data), 32000).into_sound_with_queue(2, Arc::new(()));
    let (mut b, bh, mut bq) =
        make(FrameStorage::Stream(first.clone()), 32000).into_sound_with_queue(2, Arc::new(()));
    aq.announce(1).unwrap();
    bq.announce(1).unwrap();
    aq.push(QueuedStaticSound {
        index: 1,
        data: make(FrameStorage::from(next), 44100),
        retention: None,
    })
    .unwrap_or_else(|_| panic!("queue slot"));
    bq.push(QueuedStaticSound {
        index: 1,
        data: make(FrameStorage::Stream(second.clone()), 44100),
        retention: None,
    })
    .unwrap_or_else(|_| panic!("queue slot"));
    aq.start();
    bq.start();
    let info = MockInfoBuilder::new().build();
    for _ in 0..16 {
        a.on_start_processing();
        b.on_start_processing();
        let mut expected = [Frame::ZERO; 64];
        let mut actual = expected;
        a.process(&mut expected, 1. / 48000., &info);
        b.process(&mut actual, 1. / 48000., &info);
        for (n, (actual, expected)) in actual.into_iter().zip(expected).enumerate() {
            assert_eq!(actual, expected, "sample={n}");
        }
        assert_eq!(bh.playback().item, ah.playback().item);
    }
    assert!(a.finished() && b.finished());
}

#[derive(Debug)]
struct TinyPackets(Arc<AtomicBool>);
struct TinyDecoder {
    cursor: usize,
    packet: [Frame; 1],
    ready: Arc<AtomicBool>,
}
impl AudioDecoderFactory for TinyPackets {
    fn open(
        &self,
        _: Box<dyn MediaRead>,
        _: u64,
        _: BudgetPool,
        _: Arc<AtomicBool>,
    ) -> Result<Box<dyn AudioDecoder>, String> {
        Ok(Box::new(TinyDecoder {
            cursor: 0,
            packet: [Frame::ZERO],
            ready: self.0.clone(),
        }))
    }
}
impl AudioDecoder for TinyDecoder {
    fn info(&self) -> crate::decode::AudioInfo {
        crate::decode::AudioInfo {
            sample_rate: 48000,
            channels: 2,
            frames: Some(257),
        }
    }
    fn seek(&mut self, frame: u64) -> Result<u64, String> {
        self.cursor = frame as usize;
        Ok(frame)
    }
    fn decode(&mut self, _: &AtomicBool) -> Result<bool, String> {
        if self.cursor == 257 {
            self.ready.store(true, Ordering::Release);
            return Ok(false);
        }
        self.packet[0] = frames(self.cursor, 0);
        self.cursor += 1;
        Ok(true)
    }
    fn pcm(&self) -> &[Frame] {
        &self.packet
    }
}
#[test]
fn tiny_decoder_packets_fill_the_window_and_publish_the_eof_tail() {
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 2,
            max_bytes: 1 << 20,
        },
    );
    let ready = Arc::new(AtomicBool::new(false));
    let sound = DecodedSound::prepare(
        std::io::Cursor::new(Vec::<u8>::new()),
        0,
        pool.clone(),
        DecodeLimits {
            io_deadline: None,
            static_threshold: 0,
            window_frames: 512,
        },
        Arc::new(|| {}),
        &AtomicBool::new(false),
        &TinyPackets(ready.clone()),
    )
    .unwrap();
    let limit = std::time::Instant::now() + Duration::from_secs(3);
    while !ready.load(Ordering::Acquire) {
        assert!(
            std::time::Instant::now() < limit,
            "one-frame packets exhausted block credits before the PCM window filled"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    for index in 0..257 {
        let mut actual = sound.data.frames.frame(index);
        while actual.is_none() {
            assert!(
                std::time::Instant::now() < limit,
                "EOF tail was not published"
            );
            std::thread::sleep(Duration::from_millis(1));
            actual = sound.data.frames.frame(index);
        }
        assert_eq!(actual, Some(frames(index, 0)));
    }
    drop(sound);
    while pool.usage().bytes != 0 {
        assert!(
            std::time::Instant::now() < limit,
            "cancelled block pool retained its budget"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
