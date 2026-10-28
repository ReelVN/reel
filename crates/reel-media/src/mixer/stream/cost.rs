//! Reproducible host callback cost, excluding decoding, setup and final release.
use super::*;
use reel_audio::{info::MockInfoBuilder, sound::SoundData};
use reel_core::budget::{BudgetCategory, QueueLimits};
use std::time::Instant;

#[derive(Debug)]
struct Factory(Arc<AtomicBool>);
struct Decoder {
    frames: Vec<Frame>,
    _ticket: BudgetTicket,
    ready: Arc<AtomicBool>,
    sent: bool,
}
impl AudioDecoderFactory for Factory {
    fn open(
        &self,
        _: Box<dyn MediaRead>,
        _: u64,
        pool: BudgetPool,
        _: Arc<AtomicBool>,
    ) -> Result<Box<dyn AudioDecoder>, String> {
        let ticket = pool
            .reserve(32768 * size_of::<Frame>() as u64)
            .map_err(|e| e.message)?;
        Ok(Box::new(Decoder {
            frames: (0..32768)
                .map(|n| Frame::new((n % 127) as f32 / 127., -(n % 127) as f32 / 127.))
                .collect(),
            _ticket: ticket,
            ready: self.0.clone(),
            sent: false,
        }))
    }
}
impl AudioDecoder for Decoder {
    fn info(&self) -> crate::decode::AudioInfo {
        crate::decode::AudioInfo {
            sample_rate: 48000,
            channels: 2,
            frames: Some(self.frames.len() as u64),
        }
    }
    fn seek(&mut self, _: u64) -> Result<u64, String> {
        self.sent = false;
        Ok(0)
    }
    fn decode(&mut self, _: &AtomicBool) -> Result<bool, String> {
        if self.sent {
            self.ready.store(true, Ordering::Release);
            Ok(false)
        } else {
            self.sent = true;
            Ok(true)
        }
    }
    fn pcm(&self) -> &[Frame] {
        &self.frames
    }
}
fn wait(mut predicate: impl FnMut() -> bool) {
    let limit = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < limit, "cost fixture failed to settle");
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
#[ignore = "host callback measurement; run release with --ignored --nocapture"]
fn pcm_callback_cost() {
    let mut times = Vec::new();
    let mut checksum = 0.0_f32;
    for run in 0..28 {
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 8,
                max_bytes: 4 << 20,
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
                window_frames: 32776,
            },
            Arc::new(|| {}),
            &AtomicBool::new(false),
            &Factory(ready.clone()),
        )
        .unwrap();
        wait(|| ready.load(Ordering::Acquire));
        let (mut playback, _) = sound.data.clone().into_sound().unwrap();
        let info = MockInfoBuilder::new().build();
        let mut out = [Frame::ZERO; 512];
        let begin = Instant::now();
        for _ in 0..65 {
            playback.on_start_processing();
            playback.process(std::hint::black_box(&mut out), 1. / 48000., &info);
            checksum += out[17].left;
        }
        let elapsed = begin.elapsed().as_nanos();
        assert!(playback.finished());
        if run >= 4 {
            times.push(elapsed);
        }
        drop(playback);
        drop(sound);
        wait(|| pool.usage().bytes == 0);
    }
    times.sort_unstable();
    let median = times[times.len() / 2];
    eprintln!(
        "pcm_callback_cost blocks=65 frames_per_block=512 runs={} median_run_ns={median} median_callback_ns={} ns_per_output_frame={:.3} checksum={}",
        times.len(),
        median / 65,
        median as f64 / (65. * 512.),
        std::hint::black_box(checksum)
    );
}
