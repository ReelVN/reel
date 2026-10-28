use super::*;
use reel_core::budget::{BudgetCategory, QueueLimits};
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

#[derive(Debug)]
struct Factory {
    calls: Arc<AtomicUsize>,
    fail: bool,
}
struct Decoder {
    calls: Arc<AtomicUsize>,
    frame: usize,
    packet: [Frame; 128],
    fail: bool,
}
impl AudioDecoderFactory for Factory {
    fn open(
        &self,
        _: Box<dyn MediaRead>,
        _: u64,
        _: BudgetPool,
        _: Arc<AtomicBool>,
    ) -> Result<Box<dyn AudioDecoder>, String> {
        Ok(Box::new(Decoder {
            calls: self.calls.clone(),
            frame: 0,
            packet: [Frame::ZERO; 128],
            fail: self.fail,
        }))
    }
}
impl AudioDecoder for Decoder {
    fn info(&self) -> crate::decode::AudioInfo {
        crate::decode::AudioInfo {
            sample_rate: 48000,
            channels: 2,
            frames: Some(8192),
        }
    }
    fn seek(&mut self, frame: u64) -> Result<u64, String> {
        self.frame = frame as usize;
        Ok(frame)
    }
    fn decode(&mut self, _: &AtomicBool) -> Result<bool, String> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if self.fail {
            return Err("first PCM decode failed".into());
        }
        if self.frame >= 8192 {
            return Ok(false);
        }
        for (offset, sample) in self.packet.iter_mut().enumerate() {
            *sample = Frame::new((self.frame + offset) as f32, 0.);
        }
        self.frame += self.packet.len();
        Ok(true)
    }
    fn pcm(&self) -> &[Frame] {
        &self.packet
    }
}
fn pool() -> BudgetPool {
    BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 4,
            max_bytes: 1 << 20,
        },
    )
}
fn prepare(
    pool: &BudgetPool,
    calls: Arc<AtomicUsize>,
    fail: bool,
    cancelled: &AtomicBool,
) -> Result<DecodedSound, String> {
    DecodedSound::prepare(
        std::io::Cursor::new(Vec::<u8>::new()),
        0,
        pool.clone(),
        DecodeLimits {
            io_deadline: None,
            static_threshold: 0,
            window_frames: 512,
        },
        Arc::new(|| {}),
        cancelled,
        &Factory { calls, fail },
    )
}
fn wait(mut predicate: impl FnMut() -> bool) {
    let limit = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < limit, "decoder control failed to settle");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn parked(control: &Control) {
    wait(|| control.waits.load(Ordering::Acquire) > 0);
    // Consume the retained mode/seek notification before observing steady wait.
    std::thread::sleep(Duration::from_millis(20));
    let before = control.waits.load(Ordering::Acquire);
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(
        control.waits.load(Ordering::Acquire),
        before,
        "held decoder periodically woke"
    );
}
#[test]
fn first_block_precedes_preparation_and_held_seek_wakes_without_pcm() {
    let pool = pool();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut sound = prepare(&pool, calls.clone(), false, &AtomicBool::new(false)).unwrap();
    assert!(calls.load(Ordering::Acquire) > 0);
    assert_eq!(sound.data.frames.frame(0), Some(Frame::ZERO));
    let control = sound.control.as_ref().unwrap().upgrade().unwrap();
    parked(&control);
    sound.seek(Duration::from_secs_f64(4096. / 48000.));
    wait(|| control.prepared());
    assert_eq!(sound.data.frames.frame(4096), Some(Frame::new(4096., 0.)));
    parked(&control);
    drop(sound);
    // Descriptor only; neither PCM nor decoder is retained by the control alias.
    drop(control);
    wait(|| pool.usage().bytes == 0);
}
#[test]
fn first_decode_failure_and_pre_cancel_return_without_admission() {
    let pool = pool();
    let calls = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        prepare(&pool, calls.clone(), true, &AtomicBool::new(false)).unwrap_err(),
        "first PCM decode failed"
    );
    wait(|| pool.usage().bytes == 0);
    let before = calls.load(Ordering::Acquire);
    assert!(prepare(&pool, calls.clone(), false, &AtomicBool::new(true)).is_err());
    assert_eq!(calls.load(Ordering::Acquire), before);
}
#[test]
fn pause_parks_resume_and_start_rearm_and_future_queue_progresses_without_control_poll() {
    let pool = pool();
    let calls = Arc::new(AtomicUsize::new(0));
    let sound = prepare(&pool, calls, false, &AtomicBool::new(false)).unwrap();
    let control = sound.control.as_ref().unwrap().upgrade().unwrap();
    let format = AudioFormat::new(
        48_000,
        crate::SampleFormat::F32,
        crate::ChannelLayout::Stereo,
        512,
    )
    .unwrap();
    let mut reclaim = crate::AudioReclaimService::new(
        crate::ReclaimLimits::default(),
        reel_core::wake::WakeSignal::new(|| {}),
    )
    .unwrap();
    let mut mixer = KiraMixer::new(
        &format,
        MixerLimits {
            max_sounds: 2,
            internal_buffer_frames: 128,
        },
        &reclaim,
    )
    .unwrap();
    let mut fill = mixer.take_fill().unwrap();
    let mut playback = mixer.play_in_mode(AudioGroup::Music, sound, true).unwrap();
    parked(&control);
    mixer.start(&playback);
    let before = control.waits.load(Ordering::Acquire);
    wait(|| control.waits.load(Ordering::Acquire) > before + 1);
    playback.pause();
    fill(&mut [0.; 128]);
    parked(&control);
    playback.resume();
    let before = control.waits.load(Ordering::Acquire);
    wait(|| control.waits.load(Ordering::Acquire) > before + 1);
    let next = prepare(
        &pool,
        Arc::new(AtomicUsize::new(0)),
        false,
        &AtomicBool::new(false),
    )
    .unwrap();
    let future = next.control.as_ref().unwrap().upgrade().unwrap();
    mixer.announce(&playback, 1).unwrap();
    mixer.queue(&mut playback, 1, next).unwrap();
    assert_eq!(future.mode.load(Ordering::Acquire), QUEUED);
    // PCM can select and consume the successor before any controller inspects it.
    wait(|| {
        fill(&mut [0.; 128]);
        playback.handle.state() == PlaybackState::Stopped
    });
    assert_eq!(playback.handle.queue_cursor().0, 1);
    playback.stop();
    drop(playback);
    drop(fill);
    drop(mixer);
    drop(control);
    drop(future);
    reclaim.drain_and_join().unwrap();
    wait(|| pool.usage().bytes == 0);
}
struct GatedFactory {
    entered: Arc<AtomicBool>,
    release: Arc<WorkerEvent>,
}
impl std::fmt::Debug for GatedFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GatedFactory")
    }
}
struct GatedDecoder {
    entered: Arc<AtomicBool>,
    release: Arc<WorkerEvent>,
}
impl AudioDecoderFactory for GatedFactory {
    fn open(
        &self,
        _: Box<dyn MediaRead>,
        _: u64,
        _: BudgetPool,
        _: Arc<AtomicBool>,
    ) -> Result<Box<dyn AudioDecoder>, String> {
        Ok(Box::new(GatedDecoder {
            entered: self.entered.clone(),
            release: self.release.clone(),
        }))
    }
}
impl AudioDecoder for GatedDecoder {
    fn info(&self) -> crate::decode::AudioInfo {
        crate::decode::AudioInfo {
            sample_rate: 48000,
            channels: 2,
            frames: None,
        }
    }
    fn seek(&mut self, frame: u64) -> Result<u64, String> {
        Ok(frame)
    }
    fn decode(&mut self, _: &AtomicBool) -> Result<bool, String> {
        self.entered.store(true, Ordering::Release);
        self.release.wait(None)?;
        Ok(false)
    }
    fn pcm(&self) -> &[Frame] {
        &[]
    }
}
#[test]
fn initialization_cancel_uses_bounded_flag_fallback_and_cancels_blocked_io() {
    let pool = pool();
    let entered = Arc::new(AtomicBool::new(false));
    let released = Arc::new(WorkerEvent::new().unwrap());
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_pool = pool.clone();
    let worker_entered = entered.clone();
    let worker_cancelled = cancelled.clone();
    let worker = std::thread::spawn(move || {
        let release_io = released.clone();
        DecodedSound::prepare(
            std::io::Cursor::new(Vec::<u8>::new()),
            0,
            worker_pool,
            DecodeLimits {
                io_deadline: None,
                static_threshold: 0,
                window_frames: 512,
            },
            Arc::new(move || release_io.signal().unwrap()),
            &worker_cancelled,
            &GatedFactory {
                entered: worker_entered,
                release: released,
            },
        )
    });
    wait(|| entered.load(Ordering::Acquire));
    assert!(
        !worker.is_finished(),
        "preparation returned before its first PCM block"
    );
    cancelled.store(true, Ordering::Release);
    // Joining only the cancelled preparation job; blocked IO is released by its
    // existing cancel callback, independently of a host or PCM callback.
    wait(|| worker.is_finished());
    assert_eq!(
        worker.join().unwrap().unwrap_err(),
        "audio preparation cancelled"
    );
    wait(|| pool.usage().bytes == 0);
}
