//! Automatic storage selection. Both storage kinds use the original Kira queue.
use super::*;
use reel_audio::sound::static_sound::{FrameSource, FrameStorage, StreamStatus};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize},
};

use crate::decode::{AudioDecoder, AudioDecoderFactory, MediaRead};

/// PCM residency limits used while preparing a sound off the audio thread.
#[derive(Clone, Debug)]
pub struct DecodeLimits {
    /// Optional asset deadline, updated off the realtime thread from window occupancy.
    pub io_deadline: Option<reel_core::io::ReadDeadline>,
    /// Maximum decoded PCM bytes retained as a complete static sound.
    pub static_threshold: u64,
    /// Number of preallocated PCM frames; must be at least eight. Ready, cached
    /// and recycled blocks share this fixed pool. Queue/history metadata is also
    /// charged before allocation; the mixer separately accounts its voice cache.
    pub window_frames: usize,
}

mod blocks;
pub(super) mod control;
use atomic_refcell::AtomicRefCell;
pub(super) use control::Control;
use sync_wrapper::SyncWrapper;

struct Window {
    io_deadline: Option<reel_core::io::ReadDeadline>,
    sample_rate: u32,
    capacity: usize,
    needs_block: AtomicBool,
    status: Arc<StreamStatus>,
    wanted: AtomicUsize,
    serial: AtomicU64,
    stopped: Arc<AtomicBool>,
    control: Arc<Control>,
    _residency: Arc<Residency>,
}
struct Lease {
    // Block queues physically release before the final residency owner.
    reader: AtomicRefCell<SyncWrapper<blocks::Reader>>,
    window: Arc<Window>,
    cancel: Arc<dyn Fn() + Send + Sync>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.window.stopped.store(true, Ordering::Release);
        self.window.control.stop();
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
        let mut frame = [Frame::ZERO];
        (self.read_frames(index, &mut frame) != 0).then_some(frame[0])
    }
    fn read_frames(&self, index: usize, output: &mut [Frame]) -> usize {
        if output.is_empty() {
            return 0;
        }
        let Ok(mut reader) = self.reader.try_borrow_mut() else {
            return 0;
        };
        reader.get_mut().read(&self.window, index, output)
    }
    fn seek(&self, index: usize) {
        // Starting an already prepared queue segment needs no decoder generation.
        // Immutable cached samples are also valid for a seek back into that window.
        if let Ok(mut reader) = self.reader.try_borrow_mut()
            && reader.get_mut().seek_cached(&self.window, index)
        {
            return;
        }
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
    fn run(
        &self,
        mut decoder: Box<dyn AudioDecoder>,
        mut writer: blocks::Writer,
    ) -> Result<(), String> {
        let mut filling: Option<blocks::Block> = None;
        let mut packet_len = 0usize;
        let mut packet_start = 0usize;
        let mut next = 0usize;
        let mut serial = self.serial.load(Ordering::Acquire);
        let mut eof = false;
        let mut seeking_to = None;
        let mut last_idle_wanted = usize::MAX;
        let mut deadline_published = false;
        while !self.stopped.load(Ordering::Acquire) {
            let request = self.serial.load(Ordering::Acquire);
            let wanted = self.wanted.load(Ordering::Acquire);
            let outside = wanted < next.saturating_sub(self.capacity.saturating_sub(4))
                || wanted > next.saturating_add(self.capacity);
            if request != serial || (outside && seeking_to.is_none()) {
                if let Some(deadline) = &self.io_deadline {
                    deadline.buffered(Duration::ZERO);
                    deadline_published = true;
                }
                serial = request;
                seeking_to = Some(wanted);
                next = usize::try_from(match decoder.seek(wanted as u64) {
                    Ok(index) => index,
                    Err(_) if self.status.len() == usize::MAX => decoder.seek(0)?,
                    Err(error) => return Err(error),
                })
                .map_err(|_| "audio seek exceeds address space")?;
                if let Some(block) = &mut filling {
                    block.len = 0;
                }
                packet_len = 0;
                packet_start = next;
                eof = false;
            }
            if seeking_to.is_some_and(|target| next >= target) {
                seeking_to = None;
            }
            if !eof && next < wanted.saturating_add(self.capacity.saturating_sub(4)) {
                if next >= packet_start + packet_len {
                    if let Some(deadline) = &self.io_deadline {
                        deadline.buffered(Duration::from_secs_f64(
                            next.saturating_sub(wanted) as f64 / f64::from(self.sample_rate),
                        ));
                        deadline_published = true;
                    }
                    let decoded = {
                        let _profile =
                            reel_core::profile::Scope::new(reel_core::profile::Phase::AudioDecode);
                        decoder.decode(&self.stopped)?
                    };
                    if !decoded {
                        if filling.as_ref().is_some_and(|block| block.len != 0) {
                            writer.publish(filling.take().expect("partial PCM owner"));
                            self.control.publish(serial);
                        }
                        self.needs_block.store(false, Ordering::Release);
                        self.status.set_len(next);
                        if next == 0 {
                            return Err("audio contains no PCM frames".into());
                        }
                        self.control.publish(serial);
                        eof = true;
                        continue;
                    }
                    packet_len = decoder.pcm().len();
                    if packet_len == 0 {
                        return Err("decoder returned an empty PCM chunk".into());
                    }
                    next.checked_add(packet_len)
                        .filter(|&n| n < usize::MAX)
                        .ok_or("audio timeline exceeds address space")?;
                    packet_start = next;
                }
                if next < wanted {
                    if let Some(block) = &mut filling {
                        block.len = 0;
                    }
                    next = (packet_start + packet_len).min(wanted);
                    continue;
                }
                if let Some(mut block) = filling.take().or_else(|| writer.take()) {
                    self.needs_block.store(false, Ordering::Release);
                    if block.len == 0 {
                        block.start = next;
                        block.serial = serial;
                    }
                    let count = (block.frames.len() - block.len)
                        .min(packet_start + packet_len - next)
                        .min(wanted.saturating_add(self.capacity.saturating_sub(4)) - next);
                    block.frames[block.len..block.len + count].copy_from_slice(
                        &decoder.pcm()[next - packet_start..next - packet_start + count],
                    );
                    block.len += count;
                    next += count;
                    if block.len == block.frames.len() {
                        writer.publish(block);
                        self.control.publish(serial);
                    } else {
                        // Pack small codec packets into the same owned block.
                        // EOF or the actual window boundary publishes its tail.
                        filling = Some(block);
                    }
                    continue;
                }
                self.needs_block.store(true, Ordering::Release);
            } else {
                if filling.as_ref().is_some_and(|block| block.len != 0) {
                    writer.publish(filling.take().expect("window tail PCM owner"));
                    self.control.publish(serial);
                }
                self.needs_block.store(false, Ordering::Release);
            }
            // Non-RT control wakes held/paused windows. Active and admitted
            // future streams retain bounded polling for atomic-only PCM demand.
            if deadline_published {
                if let Some(deadline) = &self.io_deadline {
                    deadline.clear();
                }
                deadline_published = false;
            }
            let idle = wanted == last_idle_wanted;
            last_idle_wanted = wanted;
            self.control.wait(!idle)?;
        }
        Ok(())
    }
}
impl DecodedSound {
    /// Prepare an ordinary audio item without loading the complete encoded file.
    /// Unknown or large PCM uses the same queued transport with a bounded block
    /// window. Every preparation owns independent decoder/reader endpoints;
    /// temporary aliases retain that ownership without adding another consumer.
    /// Preparation waits off the realtime/control thread for the first complete
    /// PCM block or terminal error. The borrowed cancellation flag is checked at
    /// most every 10 ms during this initialization wait; IO cancellation retains
    /// its reader-specific contract. Held/paused windows then wait for control;
    /// active and queued windows poll atomic PCM demand within 8/16 ms.
    pub fn prepare(
        reader: impl MediaRead + 'static,
        length: u64,
        pool: BudgetPool,
        limits: DecodeLimits,
        cancel: Arc<dyn Fn() + Send + Sync>,
        cancelled: &AtomicBool,
        factory: &dyn AudioDecoderFactory,
    ) -> Result<Self, String> {
        let DecodeLimits {
            static_threshold,
            window_frames,
            io_deadline,
        } = limits;
        if window_frames < 8 {
            return Err("audio stream window needs at least eight frames".into());
        }
        if cancelled.load(Ordering::Acquire) {
            return Err("audio preparation cancelled".into());
        }
        let stopped = Arc::new(AtomicBool::new(false));
        let mut decoder = factory.open(Box::new(reader), length, pool.clone(), stopped.clone())?;
        let info = decoder.info();
        if info.sample_rate == 0 {
            return Err("audio sample rate is zero".into());
        }
        let count = info
            .frames
            .map(usize::try_from)
            .transpose()
            .map_err(|_| "audio frame count exceeds address space")?
            .unwrap_or(usize::MAX);
        if info.frames.is_some() && count == usize::MAX {
            return Err("audio frame count exceeds source timeline".into());
        }
        let sample_rate = info.sample_rate;
        let static_bytes = info
            .frames
            .and_then(|n| n.checked_mul(size_of::<Frame>() as u64));
        if static_bytes.is_some_and(|n| n <= static_threshold) {
            let mut ticket = pool.reserve(0).map_err(|e| e.message)?;
            let mut frames = Vec::<Frame>::new();
            loop {
                if cancelled.load(Ordering::Acquire) {
                    return Err("audio preparation cancelled".into());
                }
                if !decoder.decode(cancelled)? {
                    break;
                }
                let pcm = decoder.pcm();
                if pcm.is_empty() {
                    return Err("decoder returned an empty PCM chunk".into());
                }
                let required = frames
                    .len()
                    .checked_add(pcm.len())
                    .ok_or("PCM size overflow")?;
                let capacity = if required > frames.capacity() {
                    required.max(frames.capacity().saturating_mul(2))
                } else {
                    frames.capacity()
                };
                let peak = capacity
                    .checked_add(if capacity > frames.capacity() {
                        frames.capacity()
                    } else {
                        0
                    })
                    .and_then(|n| n.checked_mul(size_of::<Frame>()))
                    .filter(|&n| n <= isize::MAX as usize)
                    .ok_or("PCM capacity overflow")?;
                ticket.resize(peak as u64).map_err(|e| e.message)?;
                if capacity > frames.capacity() {
                    frames
                        .try_reserve_exact(capacity - frames.len())
                        .map_err(|e| e.to_string())?;
                }
                ticket
                    .resize(frames.capacity() as u64 * size_of::<Frame>() as u64)
                    .map_err(|e| e.message)?;
                frames.extend_from_slice(pcm);
            }
            if frames.is_empty() {
                return Err("audio contains no PCM frames".into());
            }
            drop(decoder);
            let bytes = frames.len() as u64 * size_of::<Frame>() as u64;
            ticket
                .resize(frames.capacity() as u64 * size_of::<Frame>() as u64 + bytes)
                .map_err(|e| e.message)?;
            let frames = frames.into();
            ticket.resize(bytes).map_err(|e| e.message)?;
            (cancel)();
            return Ok(Self {
                data: StaticSoundData {
                    sample_rate,
                    frames,
                    settings: Default::default(),
                    slice: None,
                },
                control: None,
                budget: Arc::new(Residency {
                    ticket,
                    release: Mutex::new(None),
                }),
            });
        }
        if cancelled.load(Ordering::Acquire) {
            return Err("audio preparation cancelled".into());
        }
        let bytes = blocks::storage_bytes(window_frames)
            .and_then(|bytes| {
                bytes.checked_add(
                    (size_of::<Control>()
                        + 2 * size_of::<usize>()
                        + size_of::<Window>()
                        + size_of::<Lease>()
                        + size_of::<StreamStatus>()
                        + size_of::<Residency>()
                        + size_of::<[usize; 8]>()) as u64,
                )
            })
            .ok_or("audio window size overflow")?;
        let ticket = pool.reserve(bytes).map_err(|e| e.message)?;
        let budget = Arc::new(Residency {
            ticket,
            release: Mutex::new(None),
        });
        let control = Arc::new(Control::new()?);
        let window = Arc::new(Window {
            io_deadline,
            sample_rate,
            capacity: window_frames,
            needs_block: AtomicBool::new(false),
            status: Arc::new(StreamStatus::new(count)),
            wanted: AtomicUsize::new(0),
            serial: AtomicU64::new(0),
            stopped,
            control: control.clone(),
            _residency: budget.clone(),
        });
        let (reader, writer) = blocks::channels(window_frames);
        let lease = Arc::new(Lease {
            reader: AtomicRefCell::new(SyncWrapper::new(reader)),
            window: window.clone(),
            cancel,
        });
        control.bind_source(Arc::downgrade(&lease));
        std::thread::Builder::new()
            .name("reel-audio-stream".into())
            .stack_size(512 << 10)
            .spawn(move || {
                if let Err(error) = window.run(decoder, writer)
                    && !window.stopped.load(Ordering::Acquire)
                {
                    window.status.fail(error);
                }
                if let Some(deadline) = &window.io_deadline {
                    deadline.clear();
                }
                window.control.finish();
            })
            .map_err(|e| e.to_string())?;
        control.wait_initial(cancelled)?;
        Ok(Self {
            control: Some(Arc::downgrade(&control)),
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

#[cfg(all(test, feature = "decode"))]
mod tests {
    use super::*;
    use crate::decode::{AudioDecoderFactory, SoftwareAudioDecoderFactory};
    use reel_audio::sound::streaming::{Decoder, symphonia::SymphoniaDecoder};
    #[test]
    fn opus_static_and_windowed_playback_share_audible_loop_positions() {
        let bytes = include_bytes!("../../tests/fixtures/stereo.opus");
        let pool = BudgetPool::new(
            reel_core::budget::BudgetCategory::CpuOutput,
            reel_core::budget::QueueLimits {
                max_requests: 8,
                max_bytes: 1 << 20,
            },
        );
        let mut decoder = SoftwareAudioDecoderFactory
            .open(
                Box::new(std::io::Cursor::new(bytes)),
                bytes.len() as u64,
                pool.clone(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        let mut reference = Vec::new();
        while decoder.decode(&AtomicBool::new(false)).unwrap() {
            reference.extend_from_slice(decoder.pcm());
        }
        assert_eq!(reference.len(), 6576);
        drop(decoder);
        for threshold in [0, 1 << 20] {
            let mut sound = DecodedSound::prepare(
                std::io::Cursor::new(bytes),
                bytes.len() as u64,
                pool.clone(),
                DecodeLimits {
                    io_deadline: None,
                    static_threshold: threshold,
                    window_frames: 256,
                },
                Arc::new(|| {}),
                &AtomicBool::new(false),
                &SoftwareAudioDecoderFactory,
            )
            .unwrap();
            assert_eq!(sound.data.frames.len(), reference.len());
            for target in [0, 2000, 0, 2000, 0] {
                sound.seek(Duration::from_secs_f64(target as f64 / 48000.));
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                loop {
                    if let Some(actual) = sound.data.frames.frame(target) {
                        assert_eq!(actual, reference[target]);
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "Opus window failed to refill"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            drop(sound);
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while pool.usage().bytes != 0 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "Opus storage retained after retirement"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
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
            io_deadline: None,
            sample_rate: 48_000,
            capacity: 64,
            needs_block: AtomicBool::new(false),
            status: Arc::new(StreamStatus::new(usize::MAX)),
            wanted: AtomicUsize::new(expected + 100_000),
            serial: AtomicU64::new(1),
            stopped: Arc::new(AtomicBool::new(false)),
            control: Arc::new(Control::new().unwrap()),
            _residency: residency,
        });
        window.control.mode(true, false);
        let (reader, writer) = blocks::channels(64);
        let worker = window.clone();
        drop(decoder);
        let decoder = SoftwareAudioDecoderFactory
            .open(
                Box::new(std::io::Cursor::new(bytes.clone())),
                bytes.len() as u64,
                pool.clone(),
                window.stopped.clone(),
            )
            .unwrap();
        let handle = std::thread::spawn(move || worker.run(decoder, writer));
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
        drop(reader);
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
                DecodeLimits {
                    io_deadline: None,
                    static_threshold: threshold,
                    window_frames: 4096,
                },
                Arc::new(|| {}),
                &AtomicBool::new(false),
                &SoftwareAudioDecoderFactory,
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

#[cfg(test)]
mod cost;
