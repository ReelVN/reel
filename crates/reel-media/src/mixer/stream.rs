//! Automatic storage selection. Both storage kinds use the original Kira queue.
use super::*;
use reel_audio::sound::{
    static_sound::{FrameSource, FrameStorage, StreamStatus},
    streaming::{
        Decoder,
        symphonia::{AudioRead, SymphoniaDecoder},
    },
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize},
};

const READER_BYTES: u64 = 64 << 10;
struct Slot {
    index: AtomicUsize,
    pcm: AtomicU64,
}
struct Window {
    slots: Box<[Slot]>,
    status: Arc<StreamStatus>,
    wanted: AtomicUsize,
    serial: AtomicU64,
    stopped: AtomicBool,
    _residency: Arc<Residency>,
}
struct Lease {
    window: Arc<Window>,
    cancel: Arc<dyn Fn() + Send + Sync>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.window.stopped.store(true, Ordering::Release);
        (self.cancel)();
    }
}
impl FrameSource for Lease {
    fn len(&self) -> usize {
        self.window.status.len()
    }
    fn status(&self) -> Option<Arc<StreamStatus>> {
        Some(self.window.status.clone())
    }
    fn frame(&self, index: usize) -> Option<Frame> {
        self.window.wanted.store(index, Ordering::Release);
        let slot = &self.window.slots[index % self.window.slots.len()];
        if slot.index.load(Ordering::SeqCst) != index {
            return None;
        }
        let bits = slot.pcm.load(Ordering::SeqCst);
        if slot.index.load(Ordering::SeqCst) != index {
            return None;
        }
        Some(Frame::new(
            f32::from_bits(bits as u32),
            f32::from_bits((bits >> 32) as u32),
        ))
    }
    fn seek(&self, index: usize) {
        self.window.wanted.store(index, Ordering::Release);
        self.window.serial.fetch_add(1, Ordering::AcqRel);
    }
    fn failed(&self) -> bool {
        self.window.status.failed()
    }
    fn error(&self) -> Option<String> {
        self.window.status.error()
    }
}
impl Window {
    fn store(&self, index: usize, frame: Frame) {
        let slot = &self.slots[index % self.slots.len()];
        slot.index.store(usize::MAX, Ordering::SeqCst);
        slot.pcm.store(
            u64::from(frame.left.to_bits()) | (u64::from(frame.right.to_bits()) << 32),
            Ordering::SeqCst,
        );
        slot.index.store(index, Ordering::SeqCst);
    }
    fn run(&self, mut decoder: SymphoniaDecoder, mut scratch: BudgetTicket) -> Result<(), String> {
        let mut packet = Vec::<Frame>::new();
        let mut packet_start = 0usize;
        let mut next = 0usize;
        let mut serial = self.serial.load(Ordering::Acquire);
        let mut eof = false;
        let mut seeking_to = None;
        let mut last_idle_wanted = usize::MAX;
        while !self.stopped.load(Ordering::Acquire) {
            let wanted = self.wanted.load(Ordering::Acquire);
            let request = self.serial.load(Ordering::Acquire);
            let outside = wanted < next.saturating_sub(self.slots.len().saturating_sub(4))
                || wanted > next.saturating_add(self.slots.len());
            if request != serial || (outside && seeking_to.is_none()) {
                serial = request;
                seeking_to = Some(wanted);
                next = match decoder.seek(wanted) {
                    Ok(index) => index,
                    Err(_) if self.status.len() == usize::MAX => {
                        decoder.seek(0).map_err(|e| e.to_string())?
                    }
                    Err(error) => return Err(error.to_string()),
                };
                packet.clear();
                packet_start = next;
                eof = false;
            }
            if seeking_to.is_some_and(|target| next >= target) {
                seeking_to = None;
            }
            if eof || next >= wanted.saturating_add(self.slots.len().saturating_sub(4)) {
                // Paused and not-yet-admitted items keep their bounded window,
                // but do not wake at the active refill cadence.
                let idle = wanted == last_idle_wanted;
                last_idle_wanted = wanted;
                std::thread::sleep(Duration::from_millis(if idle { 8 } else { 1 }));
                continue;
            }
            if next >= packet_start + packet.len() {
                drop(packet);
                // Codec output can remain allocated between packet calls.
                // Keep its high-water reservation until this worker retires.
                packet = decoder
                    .decode_reserved(|bytes| {
                        if self.stopped.load(Ordering::Acquire) {
                            return Err(std::io::Error::other("audio preparation cancelled"));
                        }
                        scratch
                            .resize(scratch.bytes().max(READER_BYTES + bytes as u64))
                            .map_err(|e| std::io::Error::other(e.message))
                    })
                    .map_err(|e| e.to_string())?;
                packet_start = next;
                if packet.is_empty() {
                    self.status.set_len(next);
                    if next == 0 {
                        return Err("audio contains no PCM frames".into());
                    }
                    eof = true;
                    continue;
                }
                scratch
                    .resize(
                        scratch
                            .bytes()
                            .max(READER_BYTES + packet.capacity() as u64 * 8),
                    )
                    .map_err(|e| e.message)?;
            }
            let limit = (packet_start + packet.len())
                .min(wanted.saturating_add(self.slots.len().saturating_sub(4)));
            while next < limit {
                if self.stopped.load(Ordering::Acquire)
                    || self.serial.load(Ordering::Acquire) != serial
                {
                    break;
                }
                if next >= wanted {
                    self.store(next, packet[next - packet_start]);
                }
                next += 1;
            }
        }
        Ok(())
    }
}
impl DecodedSound {
    /// Prepare an ordinary audio item without loading the complete encoded file.
    /// Unknown or large PCM uses the same queued transport with a bounded window.
    pub fn prepare(
        reader: impl AudioRead + 'static,
        length: u64,
        pool: BudgetPool,
        static_threshold: u64,
        window_frames: usize,
        cancel: Arc<dyn Fn() + Send + Sync>,
        cancelled: &AtomicBool,
    ) -> Result<Self, String> {
        if window_frames < 8 {
            return Err("audio stream window needs at least eight frames".into());
        }
        let mut scratch = pool.reserve(READER_BYTES).map_err(|e| e.message)?;
        let decoder = SymphoniaDecoder::from_reader(reader, length).map_err(|e| e.to_string())?;
        let count = decoder.num_frames();
        let sample_rate = decoder.sample_rate();
        let static_bytes = u64::try_from(count).ok().and_then(|n| n.checked_mul(8));
        if count != usize::MAX && static_bytes.is_some_and(|n| n <= static_threshold) {
            let data = decoder
                .into_static(|bytes| {
                    if cancelled.load(Ordering::Acquire) {
                        return Err(std::io::Error::other("audio preparation cancelled"));
                    }
                    scratch
                        .resize(scratch.bytes().max(READER_BYTES + bytes as u64))
                        .map_err(|e| std::io::Error::other(e.message))
                })
                .map_err(|e| e.to_string())?;
            if data.frames.is_empty() {
                return Err("audio contains no PCM frames".into());
            }
            scratch
                .resize(data.frames.len() as u64 * 8)
                .map_err(|e| e.message)?;
            (cancel)();
            return Ok(Self {
                data,
                budget: Arc::new(Residency {
                    ticket: scratch,
                    release: Mutex::new(None),
                }),
            });
        }
        if cancelled.load(Ordering::Acquire) {
            return Err("audio preparation cancelled".into());
        }
        let bytes = window_frames
            .checked_mul(std::mem::size_of::<Slot>())
            .and_then(|n| {
                n.checked_add(
                    std::mem::size_of::<Window>()
                        + std::mem::size_of::<Lease>()
                        + std::mem::size_of::<StreamStatus>()
                        + std::mem::size_of::<Residency>()
                        + 8 * std::mem::size_of::<usize>(),
                )
            })
            .ok_or("audio window size overflow")?;
        let ticket = pool.reserve(bytes as u64).map_err(|e| e.message)?;
        let budget = Arc::new(Residency {
            ticket,
            release: Mutex::new(None),
        });
        let window = Arc::new(Window {
            slots: (0..window_frames)
                .map(|_| Slot {
                    index: AtomicUsize::new(usize::MAX),
                    pcm: AtomicU64::new(0),
                })
                .collect(),
            status: Arc::new(StreamStatus::new(count)),
            wanted: AtomicUsize::new(0),
            serial: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            _residency: budget.clone(),
        });
        let lease = Arc::new(Lease {
            window: window.clone(),
            cancel,
        });
        std::thread::Builder::new()
            .name("reel-audio-stream".into())
            .stack_size(512 << 10)
            .spawn(move || {
                if let Err(error) = window.run(decoder, scratch)
                    && !window.stopped.load(Ordering::Acquire)
                {
                    window.status.fail(error);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            data: StaticSoundData {
                sample_rate,
                frames: FrameStorage::Stream(lease),
                settings: Default::default(),
                slice: None,
            },
            budget,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_length_seek_past_eof_scans_once_and_publishes_actual_end() {
        let bytes = include_bytes!("../../../../examples/assets/chime.wav").to_vec();
        let decoder =
            SymphoniaDecoder::from_reader(std::io::Cursor::new(bytes.clone()), bytes.len() as u64)
                .unwrap();
        let expected = decoder.num_frames();
        let pool = BudgetPool::new(
            reel_core::budget::BudgetCategory::CpuOutput,
            reel_core::budget::QueueLimits {
                max_requests: 2,
                max_bytes: 1 << 20,
            },
        );
        let residency = Arc::new(Residency {
            ticket: pool.reserve(1024).unwrap(),
            release: Mutex::new(None),
        });
        let window = Arc::new(Window {
            slots: (0..64)
                .map(|_| Slot {
                    index: AtomicUsize::new(usize::MAX),
                    pcm: AtomicU64::new(0),
                })
                .collect(),
            status: Arc::new(StreamStatus::new(usize::MAX)),
            wanted: AtomicUsize::new(expected + 100_000),
            serial: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
            _residency: residency,
        });
        let worker = window.clone();
        let scratch = pool.reserve(READER_BYTES).unwrap();
        let handle = std::thread::spawn(move || worker.run(decoder, scratch));
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while window.status.len() == usize::MAX {
            if std::time::Instant::now() >= deadline {
                window.stopped.store(true, Ordering::Release);
                handle.join().unwrap().unwrap();
                panic!("unknown EOF seek did not settle");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(window.status.len(), expected);
        window.stopped.store(true, Ordering::Release);
        handle.join().unwrap().unwrap();
        drop(window);
        assert_eq!(pool.usage().bytes, 0);
    }
    #[test]
    fn mp3_priming_packet_prepares_static_and_windowed_pcm_and_releases_budget() {
        let bytes = include_bytes!("../../../reel-audio/tests/fixtures/short-tone.mp3");
        for threshold in [0, 1 << 20] {
            let pool = BudgetPool::new(
                reel_core::budget::BudgetCategory::CpuOutput,
                reel_core::budget::QueueLimits {
                    max_requests: 8,
                    max_bytes: 1 << 20,
                },
            );
            let sound = DecodedSound::prepare(
                std::io::Cursor::new(bytes),
                bytes.len() as u64,
                pool.clone(),
                threshold,
                4096,
                Arc::new(|| {}),
                &AtomicBool::new(false),
            )
            .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(frame) = sound.data.frames.frame(80) {
                    assert!(frame.left != 0. || frame.right != 0.);
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "priming packet prevented PCM preparation"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            drop(sound);
            while pool.usage().bytes != 0 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "decoder retained residency after retirement"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}
