//! Bounded worker playback and mixer-owned PCM consumption.
use super::{DecodedPacket, MediaRead, VideoDecoderFactory, VideoFrame, VideoInfo};
use reel_audio::{
    Frame,
    info::Info,
    sound::{Sound, SoundData},
};
use reel_core::{
    budget::{BudgetPool, BudgetTicket},
    wake::WakeSignal,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone, Copy, Debug)]
pub struct VideoOptions {
    pub queued_video_frames: usize,
    pub queued_audio_frames: usize,
    pub start: Duration,
    /// Exclusive source endpoint, independent of looping. Hosts can queue the
    /// next segment against the same audio consumption clock.
    pub end: Option<Duration>,
    /// Restore a terminal visual without admitting or decoding its audio track.
    pub poster: bool,
    /// Initial transport state; decoding can prepare the first picture while paused.
    pub paused: bool,
}
impl Default for VideoOptions {
    fn default() -> Self {
        Self {
            queued_video_frames: 3,
            queued_audio_frames: 8192,
            start: Duration::ZERO,
            end: None,
            poster: false,
            paused: false,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum VideoTerminal {
    Completed,
    Stopped,
    Cancelled,
    Failed(String),
}
#[derive(Debug)]
pub struct ResidentFrame {
    pub frame: VideoFrame,
    _ticket: BudgetTicket,
}
impl reel_core::image::Yuv420Storage for ResidentFrame {
    fn plane(&self, index: usize) -> &[u8] {
        &self.frame.planes[index]
    }
}
pub type VideoFrameLease = Arc<ResidentFrame>;

// Single writer (PCM), control-side reader. Uses only 32-bit atomics, including
// on hosts without native 64-bit atomics. No lock or retry on the PCM path.
struct Clock {
    sequence: AtomicU32,
    low: AtomicU32,
    high: AtomicU32,
}
impl Clock {
    fn new(time: f64) -> Self {
        let bits = time.to_bits();
        Self {
            sequence: AtomicU32::new(0),
            low: AtomicU32::new(bits as u32),
            high: AtomicU32::new((bits >> 32) as u32),
        }
    }
    fn store(&self, time: f64) {
        self.sequence.fetch_add(1, Ordering::AcqRel);
        let bits = time.to_bits();
        self.low.store(bits as u32, Ordering::Relaxed);
        self.high.store((bits >> 32) as u32, Ordering::Relaxed);
        self.sequence.fetch_add(1, Ordering::Release);
    }
    fn load(&self) -> f64 {
        loop {
            let before = self.sequence.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let bits = u64::from(self.low.load(Ordering::Relaxed))
                | (u64::from(self.high.load(Ordering::Relaxed)) << 32);
            if before == self.sequence.load(Ordering::Acquire) {
                return f64::from_bits(bits);
            }
        }
    }
}
struct Transport {
    paused: AtomicBool,
    cancelled: Arc<AtomicBool>,
    audio_end: AtomicBool,
    audio_drained: AtomicBool,
    clock: Clock,
    gain: AtomicU32,
}
struct State {
    info: Option<VideoInfo>,
    audio: Option<StreamSoundData>,
    frames: VecDeque<VideoFrameLease>,
    eof: bool,
    terminal: Option<VideoTerminal>,
}
struct Shared {
    state: Mutex<State>,
    available: Condvar,
    transport: Arc<Transport>,
    wake: WakeSignal,
}
#[derive(Clone, Copy)]
struct Sample {
    frame: Frame,
    end: f64,
}
struct PcmResidency {
    _ticket: BudgetTicket,
}

/// A one-shot stream admission into the existing mixer. Its ring buffer is
/// allocated on the decoder worker; the audio callback only reads Copy samples.
pub struct StreamSoundData {
    consumer: rtrb::Consumer<Sample>,
    transport: Arc<Transport>,
    sample_rate: u32,
    _residency: Arc<PcmResidency>,
}
struct StreamSound {
    data: StreamSoundData,
    current: Option<Sample>,
    phase: f64,
}
impl SoundData for StreamSoundData {
    type Error = std::convert::Infallible;
    type Handle = ();
    fn into_sound(self) -> Result<(Box<dyn Sound>, ()), Self::Error> {
        Ok((
            Box::new(StreamSound {
                data: self,
                current: None,
                phase: 0.,
            }),
            (),
        ))
    }
}
impl Sound for StreamSound {
    fn process(&mut self, out: &mut [Frame], dt: f64, _info: &Info) {
        out.fill(Frame::ZERO);
        let transport = &self.data.transport;
        if transport.paused.load(Ordering::Acquire) || transport.cancelled.load(Ordering::Acquire) {
            return;
        }
        let rate = f64::from(self.data.sample_rate);
        let step = dt * rate;
        let gain = f32::from_bits(transport.gain.load(Ordering::Relaxed));
        let mut position = None;
        'frames: for dest in out {
            if self.current.is_none() {
                match self.data.consumer.pop() {
                    Ok(s) => self.current = Some(s),
                    Err(_) => {
                        if transport.audio_end.load(Ordering::Acquire) {
                            transport.audio_drained.store(true, Ordering::Release);
                        }
                        break;
                    }
                }
            }
            while self.phase >= 1. {
                match self.data.consumer.pop() {
                    Ok(s) => {
                        self.current = Some(s);
                        self.phase -= 1.;
                    }
                    Err(_) => {
                        if transport.audio_end.load(Ordering::Acquire) {
                            position = Some(self.current.unwrap().end);
                            transport.audio_drained.store(true, Ordering::Release);
                            self.current = None;
                        }
                        break 'frames;
                    }
                }
            }
            let current = self.current.unwrap();
            let next = self.data.consumer.peek().map_or(current.frame, |s| s.frame);
            *dest = (current.frame + (next - current.frame) * (self.phase as f32)) * gain;
            self.phase += step;
            position = Some(current.end - (1. - self.phase.min(1.)) / rate);
        }
        if let Some(position) = position {
            transport.clock.store(position);
        }
    }
    fn finished(&self) -> bool {
        self.data.transport.cancelled.load(Ordering::Acquire)
            || self.data.transport.audio_drained.load(Ordering::Acquire)
    }
}

/// The host keeps this control handle while a stage node owns its frame lease.
/// `tick` receives session elapsed time and never does IO or waits for the worker.
pub struct VideoPlayback {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    cancel_source: Box<dyn Fn() + Send>,
    position: f64,
    audio_clock: bool,
    options: VideoOptions,
}
impl VideoPlayback {
    /// Start playback with the MP4 software decoder on a background worker.
    #[cfg(feature = "video-software")]
    pub fn start(
        source: Box<dyn MediaRead>,
        cancel_source: impl Fn() + Send + 'static,
        options: VideoOptions,
        budget: BudgetPool,
        wake: WakeSignal,
    ) -> Result<Self, String> {
        Self::start_with_decoder(
            source,
            cancel_source,
            options,
            budget,
            wake,
            Arc::new(super::SoftwareVideoDecoderFactory),
        )
    }
    /// Start the common picture/PCM transport with a host-selected decoder.
    /// Factory opening and every decoder operation run on the owned worker.
    pub fn start_with_decoder(
        source: Box<dyn MediaRead>,
        cancel_source: impl Fn() + Send + 'static,
        options: VideoOptions,
        budget: BudgetPool,
        wake: WakeSignal,
        decoder: Arc<dyn VideoDecoderFactory>,
    ) -> Result<Self, String> {
        if options.queued_video_frames == 0
            || options.queued_audio_frames == 0
            || (!options.poster && options.end.is_some_and(|end| end <= options.start))
        {
            return Err("video queues must be nonempty and end must follow start".into());
        }
        let position = options.start.as_secs_f64();
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                info: None,
                audio: None,
                frames: VecDeque::with_capacity(options.queued_video_frames),
                eof: false,
                terminal: None,
            }),
            available: Condvar::new(),
            transport: Arc::new(Transport {
                paused: AtomicBool::new(options.paused),
                cancelled: Arc::new(AtomicBool::new(false)),
                audio_end: AtomicBool::new(false),
                audio_drained: AtomicBool::new(false),
                clock: Clock::new(position),
                gain: AtomicU32::new(1.0_f32.to_bits()),
            }),
            wake,
        });
        let background = shared.clone();
        let worker = thread::Builder::new()
            .name("reel-video-decode".into())
            .spawn(move || {
                let result = decode(source, options, budget, &background, decoder.as_ref());
                let mut state = background.state.lock().unwrap();
                if let Err(error) = result
                    && !background.transport.cancelled.load(Ordering::Acquire)
                {
                    state.terminal = Some(VideoTerminal::Failed(error));
                    background
                        .transport
                        .cancelled
                        .store(true, Ordering::Release);
                }
                state.eof = true;
                background
                    .transport
                    .audio_end
                    .store(true, Ordering::Release);
                drop(state);
                background.wake.wake();
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            shared,
            worker: Some(worker),
            cancel_source: Box::new(cancel_source),
            position,
            audio_clock: false,
            options,
        })
    }
    pub fn info(&self) -> Option<VideoInfo> {
        self.shared.state.lock().unwrap().info.clone()
    }
    /// Admit this exactly once through MediaPlayer/KiraMixer. Until admission
    /// the bounded audio queue stalls decode, rather than discarding the track.
    pub fn take_audio(&mut self) -> Option<StreamSoundData> {
        let data = self.shared.state.lock().unwrap().audio.take();
        self.audio_clock |= data.is_some();
        data
    }
    pub fn position(&self) -> Duration {
        Duration::from_secs_f64(self.position)
    }
    pub fn set_volume(&self, volume: f64) -> Result<(), String> {
        if !volume.is_finite() || volume < 0. || volume > f32::MAX as f64 {
            return Err("video volume must be finite and nonnegative".into());
        }
        self.shared
            .transport
            .gain
            .store((volume as f32).to_bits(), Ordering::Relaxed);
        Ok(())
    }
    pub fn pause(&self, paused: bool) {
        self.shared
            .transport
            .paused
            .store(paused, Ordering::Release);
        self.shared.available.notify_all();
    }
    pub fn tick(&mut self, elapsed: Duration) {
        let transport = &self.shared.transport;
        let mut state = self.shared.state.lock().unwrap();
        if state.terminal.is_some() || transport.paused.load(Ordering::Acquire) {
            return;
        }
        let Some(info) = &state.info else {
            return;
        };
        let end = self.options.end.map(|e| e.as_secs_f64()).or(info.duration);
        if info.audio_sample_rate.is_some() && !transport.audio_drained.load(Ordering::Acquire) {
            if self.audio_clock {
                self.position = transport.clock.load();
            }
        } else {
            self.position += elapsed.as_secs_f64();
        }
        if let Some(end) = end {
            self.position = self.position.min(end);
        }
        if state.eof
            && state.frames.is_empty()
            && end.is_some_and(|end| self.position >= end)
            && (info.audio_sample_rate.is_none() || transport.audio_drained.load(Ordering::Acquire))
        {
            state.terminal = Some(VideoTerminal::Completed);
        }
        drop(state);
        self.shared.available.notify_one();
        self.reap();
    }
    pub fn take_frame(&mut self) -> Option<VideoFrameLease> {
        let mut state = self.shared.state.lock().unwrap();
        let mut frame = None;
        while state
            .frames
            .front()
            .is_some_and(|frame| self.options.poster || frame.frame.pts <= self.position)
        {
            frame = state.frames.pop_front();
        }
        drop(state);
        if frame.is_some() {
            self.shared.available.notify_one();
        }
        frame
    }
    pub fn terminal(&self) -> Option<VideoTerminal> {
        self.shared.state.lock().unwrap().terminal.clone()
    }
    pub fn stop(&mut self) {
        self.finish(VideoTerminal::Stopped);
    }
    pub fn cancel(&mut self) {
        self.finish(VideoTerminal::Cancelled);
    }
    fn finish(&mut self, result: VideoTerminal) {
        self.shared
            .transport
            .cancelled
            .store(true, Ordering::Release);
        (self.cancel_source)();
        let mut state = self.shared.state.lock().unwrap();
        state.terminal.get_or_insert(result);
        state.frames.clear();
        state.audio = None;
        drop(state);
        self.shared.available.notify_all();
        self.shared.wake.wake();
        self.reap();
    }
    fn reap(&mut self) {
        if self.worker.as_ref().is_some_and(|w| w.is_finished()) {
            let _ = self.worker.take().unwrap().join();
        }
    }
    pub fn worker_finished(&mut self) -> bool {
        self.reap();
        self.worker.is_none()
    }
}
impl Drop for VideoPlayback {
    fn drop(&mut self) {
        self.cancel(); /* detached worker owns cleanup until its cancelled read returns */
    }
}

fn decode(
    source: Box<dyn MediaRead>,
    options: VideoOptions,
    budget: BudgetPool,
    shared: &Shared,
    factory: &dyn VideoDecoderFactory,
) -> Result<(), String> {
    let mut decoder = factory.open(source, budget.clone(), shared.transport.cancelled.clone())?;
    if let Some(end) = options.end {
        decoder.set_end(end.as_secs_f64());
    }
    if options.poster {
        decoder.prepare_poster(options.start.as_secs_f64())?;
    } else if options.start != Duration::ZERO {
        decoder.seek(options.start.as_secs_f64())?;
    }
    let info = decoder.info().clone();
    let mut audio = if let Some(rate) = info.audio_sample_rate {
        let ticket = budget
            .reserve((options.queued_audio_frames * std::mem::size_of::<Sample>()) as u64)
            .map_err(|e| e.message)?;
        let residency = Arc::new(PcmResidency { _ticket: ticket });
        let (producer, consumer) = rtrb::RingBuffer::new(options.queued_audio_frames);
        shared.state.lock().unwrap().audio = Some(StreamSoundData {
            consumer,
            transport: shared.transport.clone(),
            sample_rate: rate,
            _residency: residency.clone(),
        });
        Some((producer, residency, options.start.as_secs_f64()))
    } else {
        None
    };
    shared.state.lock().unwrap().info = Some(info.clone());
    shared.wake.wake();
    loop {
        if shared.transport.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        // One in-flight decoded picture/AAC block is accounted before decoding,
        // including while it waits behind the bounded publication queue.
        let plane_bytes = u64::from(info.width) * u64::from(info.height)
            + 2 * u64::from(info.width.div_ceil(2)) * u64::from(info.height.div_ceil(2));
        let mut scratch = budget
            .reserve(plane_bytes.max(1024 * 8))
            .map_err(|e| e.message)?;
        let Some(packet) = decoder.next_packet()? else {
            if options.poster {
                return Err("video contains no picture at the saved position".into());
            }
            break;
        };
        match packet {
            DecodedPacket::Video(mut frame) => {
                let end = options.end.map_or(f64::INFINITY, |e| e.as_secs_f64());
                if frame.pts >= end {
                    continue;
                }
                frame.duration = frame.duration.min(end - frame.pts);
                let mut state = shared.state.lock().unwrap();
                while state.frames.len() >= options.queued_video_frames
                    && !shared.transport.cancelled.load(Ordering::Acquire)
                {
                    state = shared.available.wait(state).unwrap();
                }
                if shared.transport.cancelled.load(Ordering::Acquire) {
                    return Ok(());
                }
                let bytes = frame.planes.iter().map(|p| p.capacity() as u64).sum();
                scratch.resize(bytes).map_err(|e| e.message)?;
                state.frames.push_back(Arc::new(ResidentFrame {
                    frame,
                    _ticket: scratch,
                }));
                if options.poster {
                    state.terminal = Some(VideoTerminal::Completed);
                }
                drop(state);
                shared.wake.wake();
                if options.poster {
                    return Ok(());
                }
            }
            DecodedPacket::Audio(packet) => {
                let (producer, _residency, position) = audio.as_mut().unwrap();
                let step = 1. / f64::from(packet.sample_rate);
                let endpoint = options.end.map_or(f64::INFINITY, |e| e.as_secs_f64());
                let mut index = 0;
                while *position < packet.pts || index < packet.frames.len() {
                    if shared.transport.cancelled.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    if *position + step / 2. >= endpoint {
                        break;
                    }
                    let silent = *position + step / 2. < packet.pts;
                    let frame = if silent {
                        Frame::ZERO
                    } else {
                        Frame::new(packet.frames[index][0], packet.frames[index][1])
                    };
                    match producer.push(Sample {
                        frame,
                        end: *position + step,
                    }) {
                        Ok(()) => {
                            *position += step;
                            if !silent {
                                index += 1;
                            }
                        }
                        Err(_) => {
                            let state = shared.state.lock().unwrap();
                            let _ = shared
                                .available
                                .wait_timeout(state, Duration::from_millis(5))
                                .unwrap();
                        }
                    }
                }
            }
        }
        if decoder.audio_finished() {
            shared.transport.audio_end.store(true, Ordering::Release);
        }
    }
    Ok(())
}
