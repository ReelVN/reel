use crate::{pcm::Converter, *};
use atomic_refcell::AtomicRefCell;
use reel_core::{
    result::{RejectCode, RejectReason, Rejected},
    wake::WakeSignal,
};
use rtrb::RingBuffer;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use sync_wrapper::SyncWrapper;
use triple_buffer::{Input, Output as SnapshotOutput, TripleBuffer};

const ACTIVE: u8 = 1;
const CLOSED: u8 = 2;
const FAULT: u8 = 4;

pub(crate) struct Shared {
    gate: AtomicU8,
    fault: AtomicU8,
    metrics: Arc<Telemetry>,
}
struct Telemetry {
    underruns: AtomicUsize,
    silent_frames: AtomicUsize,
    realtime_denied: AtomicUsize,
    dirty: AtomicBool,
}
impl Telemetry {
    fn new() -> Self {
        Self {
            underruns: AtomicUsize::new(0),
            silent_frames: AtomicUsize::new(0),
            realtime_denied: AtomicUsize::new(0),
            dirty: AtomicBool::new(false),
        }
    }
    fn reset(&self) {
        self.underruns.store(0, Ordering::Relaxed);
        self.silent_frames.store(0, Ordering::Relaxed);
        self.realtime_denied.store(0, Ordering::Relaxed);
    }
    fn underrun(&self, frames: usize) {
        self.underruns.fetch_add(1, Ordering::Relaxed);
        self.silent_frames.fetch_add(frames, Ordering::Relaxed);
        self.dirty.store(true, Ordering::Release);
    }
}
impl Shared {
    fn new(metrics: Arc<Telemetry>) -> Self {
        Self {
            gate: AtomicU8::new(CLOSED),
            fault: AtomicU8::new(0),
            metrics,
        }
    }
    /// One atomic decides whether this call entered before or after close.
    fn enter(&self) -> Option<CallGuard<'_>> {
        self.gate
            .compare_exchange(0, ACTIVE, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| CallGuard(self))
    }
    fn close_and_wait(&self) {
        self.gate.fetch_or(CLOSED, Ordering::AcqRel);
        // Control side only. The callback releases ACTIVE and never waits for us.
        while self.gate.load(Ordering::Acquire) & ACTIVE != 0 {
            thread::yield_now();
        }
    }
    fn open(&self) {
        let _ = self
            .gate
            .compare_exchange(CLOSED, 0, Ordering::AcqRel, Ordering::Acquire);
    }
    /// No allocation, formatting, notification callback, or blocking on the PCM thread.
    pub fn fail(&self, kind: AudioErrorKind) {
        self.gate.fetch_or(CLOSED | FAULT, Ordering::AcqRel);
        let code = match kind {
            AudioErrorKind::ReconfigureRequired => 2,
            AudioErrorKind::DeviceContract => 3,
            _ => 1,
        };
        let _ = self
            .fault
            .compare_exchange(0, code, Ordering::AcqRel, Ordering::Acquire);
        self.metrics.dirty.store(true, Ordering::Release);
    }
    pub fn underrun(&self, frames: usize) {
        self.metrics.underrun(frames);
    }
    /// Native discontinuity, distinct from a bounded-queue shortage filled with
    /// silence. Unknown lost frames invalidate the whole presentation mapping.
    #[cfg(any(
        test,
        all(
            feature = "cpal-output",
            any(target_os = "linux", target_os = "windows", target_os = "macos")
        )
    ))]
    pub fn native_xrun(&self) {
        self.metrics.underrun(0);
        self.fail(AudioErrorKind::ReconfigureRequired);
    }
    #[cfg(any(
        test,
        all(
            feature = "cpal-output",
            any(target_os = "linux", target_os = "windows", target_os = "macos")
        )
    ))]
    pub fn realtime_denied(&self) {
        self.metrics.realtime_denied.fetch_add(1, Ordering::Relaxed);
        self.metrics.dirty.store(true, Ordering::Release);
    }
    fn error(&self) -> Option<AudioError> {
        match self.fault.load(Ordering::Acquire) {
            1 => Some(AudioError::new(
                AudioErrorKind::DeviceFailure,
                "native output failed",
            )),
            2 => Some(AudioError::new(
                AudioErrorKind::ReconfigureRequired,
                "device route or format changed; stop and rebuild",
            )),
            3 => Some(AudioError::new(
                AudioErrorKind::DeviceContract,
                "device supplied invalid length or sample format",
            )),
            _ => None,
        }
    }
}
struct CallGuard<'a>(&'a Shared);
impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        self.0.gate.fetch_and(!ACTIVE, Ordering::Release);
    }
}

struct Notifier {
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Notifier {
    fn new(metrics: Arc<Telemetry>, wake: WakeSignal) -> Result<Self, AudioError> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let quitting = shutdown.clone();
        let thread = thread::Builder::new()
            .name("reel-audio-notify".into())
            .spawn(move || {
                while !quitting.load(Ordering::Acquire) {
                    if metrics.dirty.swap(false, Ordering::AcqRel) {
                        wake.wake();
                    }
                    thread::park_timeout(Duration::from_millis(2));
                }
            })
            .map_err(|e| AudioError::new(AudioErrorKind::Unavailable, e.to_string()))?;
        Ok(Self {
            shutdown,
            thread: Some(thread),
        })
    }
}
impl Drop for Notifier {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

// Mutable-only access preserves PcmFill: Send without adding a Sync bound.
// The realtime borrow attempts one CAS and never retries or waits.
struct FillSlot(AtomicRefCell<SyncWrapper<Option<PcmFill>>>);
impl FillSlot {
    fn new() -> Self {
        Self(AtomicRefCell::new(SyncWrapper::new(None)))
    }
    fn discard(&self) {
        // The caller has closed entry and fenced the callback borrow.
        let fill = self.0.borrow_mut().get_mut().take();
        drop(fill);
    }
}

pub(crate) struct Callback {
    converter: Converter,
    fill_slot: Arc<FillSlot>,
    pub shared: Arc<Shared>,
    clock: Input<AudioClockObservation>,
    epoch: OutputEpoch,
    generated: u64,
    presented: u64,
    clock_capability: AudioClockCapability,
}
impl Callback {
    fn new(
        format: AudioFormat,
        fill_slot: Arc<FillSlot>,
        shared: Arc<Shared>,
        epoch: OutputEpoch,
        clock: Input<AudioClockObservation>,
        clock_capability: AudioClockCapability,
    ) -> Self {
        Self {
            converter: Converter::new(format),
            fill_slot,
            shared,
            clock,
            epoch,
            generated: 0,
            presented: 0,
            clock_capability,
        }
    }
    pub fn render<T: DeviceSample>(&mut self, data: &mut [T], timing: CallbackTiming) {
        data.fill(T::from_pcm(0.0));
        let Some(_entered) = self.shared.enter() else {
            return;
        };
        let format = &self.converter.format;
        let channels = format.channel_map.len();
        if T::FORMAT != format.device_sample_format || !data.len().is_multiple_of(channels) {
            self.shared.fail(AudioErrorKind::DeviceContract);
            return;
        }
        if data.is_empty() {
            return;
        }
        let frames = data.len() / channels;
        let Ok(mut slot) = self.fill_slot.0.try_borrow_mut() else {
            self.shared.fail(AudioErrorKind::DeviceContract);
            return;
        };
        if let Some(fill) = slot.get_mut() {
            self.converter.render(data, fill);
        } else {
            self.shared.underrun(frames);
        }
        // A concurrent native fault invalidates this epoch. Do not publish new
        // software counts after noticing it; late old-epoch writes are also
        // rejected by the control reader's epoch check.
        if self.shared.fault.load(Ordering::Acquire) != 0 {
            return;
        }
        let before = self.generated;
        self.generated = before.saturating_add(frames as u64);
        let submission_known = self.clock_capability == AudioClockCapability::Estimated;
        let precision = match (submission_known, timing.latency_frames) {
            (true, Some(latency)) => {
                // Only a driver that guarantees previous blocks reached its sink
                // may use generated history as submission history. CPAL never opts in.
                self.presented = self
                    .presented
                    .max(before.saturating_sub(u64::from(latency)));
                ClockPrecision::Estimated
            }
            _ => ClockPrecision::Unavailable,
        };
        self.clock.write(AudioClockObservation {
            output_epoch: self.epoch,
            generated_frames: self.generated,
            submitted_frames: if precision == ClockPrecision::Unavailable {
                0
            } else {
                self.generated
            },
            estimated_presented_frames: if precision == ClockPrecision::Unavailable {
                0
            } else {
                self.presented
            },
            monotonic_time: timing.now,
            sample_rate_hz: self.converter.format.sample_rate_hz,
            latency_frames: timing.latency_frames, // native latency may still be diagnostic
            uncertainty_frames: if precision == ClockPrecision::Unavailable {
                None
            } else {
                timing.uncertainty_frames
            },
            precision,
        });
    }
}

/// Native lifecycle boundary. The gate and control-owned slot supply fill
/// quiescence and disposal independently of native pause or callback lifetime.
pub(crate) trait Driver {
    type Stream;
    fn capabilities(&self) -> AudioCaps;
    fn configure(&mut self, request: &AudioRequest) -> Result<AudioFormat, AudioError>;
    fn build(
        &mut self,
        format: &AudioFormat,
        callback: Callback,
    ) -> Result<Self::Stream, AudioError>;
    fn play(&mut self, stream: &Self::Stream) -> Result<(), AudioError>;
    fn pause(&mut self, stream: &Self::Stream) -> Result<(), AudioError>;
    fn verify(&self, format: &AudioFormat) -> Result<(), AudioError>;
}

pub(crate) struct DrivenOutput<D: Driver> {
    pub driver: D,
    pub stream: Option<D::Stream>,
    pub shared: Arc<Shared>,
    fill_slot: Option<Arc<FillSlot>>,
    format: Option<AudioFormat>,
    state: AudioState,
    control_error: Option<AudioError>,
    epoch: OutputEpoch,
    clock: Mutex<Option<SnapshotOutput<AudioClockObservation>>>,
    wake: WakeSignal,
    _notifier: Notifier,
}
impl<D: Driver> DrivenOutput<D> {
    pub fn new(driver: D, wake: WakeSignal) -> Result<Self, AudioError> {
        let metrics = Arc::new(Telemetry::new());
        let shared = Arc::new(Shared::new(metrics.clone()));
        let notifier = Notifier::new(metrics, wake.clone())?;
        Ok(Self {
            driver,
            stream: None,
            shared,
            fill_slot: None,
            format: None,
            state: AudioState::Stopped,
            control_error: None,
            epoch: OutputEpoch(0),
            clock: Mutex::new(None),
            wake,
            _notifier: notifier,
        })
    }
    fn current_epoch(&self) -> OutputEpoch {
        OutputEpoch(self.epoch.0 + u64::from(self.shared.fault.load(Ordering::Acquire) != 0))
    }
    fn reject(fill: PcmFill, error: AudioError) -> Rejected<PcmFill> {
        let code = match error.kind {
            AudioErrorKind::Busy => RejectCode::Busy,
            AudioErrorKind::InvalidRequest => RejectCode::InvalidRequest,
            AudioErrorKind::Unsupported => RejectCode::Unsupported,
            AudioErrorKind::Unavailable => RejectCode::Unavailable,
            _ => RejectCode::ServiceFault,
        };
        Rejected {
            request: fill,
            reason: RejectReason::new(code, error.message),
        }
    }
    fn fault_control(&mut self, error: AudioError) -> AudioError {
        self.control_error = Some(error.clone());
        self.shared.fail(error.kind);
        self.shared.close_and_wait();
        self.wake.wake();
        error
    }
}
impl<D: Driver> AudioOutput for DrivenOutput<D> {
    fn capabilities(&self) -> AudioCaps {
        self.driver.capabilities()
    }
    fn configure(&mut self, request: AudioRequest) -> Result<AudioFormat, AudioError> {
        if self.state != AudioState::Stopped {
            return Err(AudioError::new(
                AudioErrorKind::Busy,
                "configure requires stop",
            ));
        }
        // A failed negotiation leaves no accidentally reusable prior format.
        self.format = None;
        let format = self.driver.configure(&request)?;
        format.validate()?;
        self.epoch = OutputEpoch(self.epoch.0.checked_add(2).ok_or_else(|| {
            AudioError::new(AudioErrorKind::Unavailable, "output epoch exhausted")
        })?);
        self.shared.metrics.reset();
        self.shared = Arc::new(Shared::new(self.shared.metrics.clone()));
        *self.clock.get_mut().expect("control clock poisoned") = None;
        self.control_error = None;
        self.format = Some(format.clone());
        Ok(format)
    }
    fn start(&mut self, fill: PcmFill) -> Result<(), Rejected<PcmFill>> {
        if self.state != AudioState::Stopped {
            return Err(Self::reject(
                fill,
                AudioError::new(AudioErrorKind::Busy, "start requires stopped output"),
            ));
        }
        let Some(format) = self.format.clone() else {
            return Err(Self::reject(
                fill,
                AudioError::new(AudioErrorKind::InvalidRequest, "configure before start"),
            ));
        };
        if let Err(error) = self.driver.verify(&format) {
            return Err(Self::reject(fill, error));
        }
        let Some(next_epoch) = self.epoch.0.checked_add(2) else {
            return Err(Self::reject(
                fill,
                AudioError::new(AudioErrorKind::Unavailable, "output epoch exhausted"),
            ));
        };
        let epoch = OutputEpoch(next_epoch);
        // Each native stream has its own gate/fault latch. Late old errors cannot
        // stop a rebuilt stream or mutate its epoch. Metrics remain output-owned.
        let prepared = Arc::new(Shared::new(self.shared.metrics.clone()));
        let fill_slot = Arc::new(FillSlot::new());
        let (clock_in, clock_out) = TripleBuffer::new(&AudioClockObservation::unavailable(
            epoch,
            format.sample_rate_hz,
        ))
        .split();
        let callback = Callback::new(
            format.clone(),
            fill_slot.clone(),
            prepared.clone(),
            epoch,
            clock_in,
            self.driver.capabilities().clock,
        );
        // fill has NOT been moved into the slot. Early native callbacks are silent.
        let stream = match self.driver.build(&format, callback) {
            Ok(s) => s,
            Err(e) => return Err(Self::reject(fill, e)),
        };
        if let Err(e) = self.driver.play(&stream) {
            drop(stream);
            return Err(Self::reject(fill, e));
        }
        // Native play success is acceptance. A late native error is now status.
        // Entry is still closed; the callback cannot borrow while we insert fill.
        *fill_slot.0.borrow_mut().get_mut() = Some(fill);
        self.fill_slot = Some(fill_slot);
        self.shared = prepared;
        self.epoch = epoch;
        *self.clock.get_mut().expect("control clock poisoned") = Some(clock_out);
        self.stream = Some(stream);
        self.state = AudioState::Running;
        self.control_error = None;
        self.shared.open(); // FAULT bit prevents a racing error from reopening entry.
        self.wake.wake();
        Ok(())
    }
    fn suspend(&mut self) -> Result<(), AudioError> {
        if self.status().state != AudioState::Running {
            return Err(AudioError::new(
                AudioErrorKind::Busy,
                "suspend requires running output",
            ));
        }
        self.shared.close_and_wait();
        if let Err(e) = self
            .driver
            .pause(self.stream.as_ref().expect("running stream"))
        {
            return Err(self.fault_control(e));
        }
        if let Some(e) = self.shared.error() {
            return Err(self.fault_control(e));
        }
        self.state = AudioState::Suspended;
        self.wake.wake();
        Ok(())
    }
    fn resume(&mut self) -> Result<(), AudioError> {
        if self.status().state != AudioState::Suspended {
            return Err(AudioError::new(
                AudioErrorKind::Busy,
                "resume requires suspended output",
            ));
        }
        if let Err(e) = self
            .driver
            .verify(self.format.as_ref().expect("configured stream"))
        {
            return Err(self.fault_control(e));
        }
        if let Err(e) = self
            .driver
            .play(self.stream.as_ref().expect("suspended stream"))
        {
            return Err(self.fault_control(e));
        }
        if let Some(e) = self.shared.error() {
            return Err(self.fault_control(e));
        }
        self.state = AudioState::Running;
        self.shared.open();
        self.wake.wake();
        Ok(())
    }
    fn status(&self) -> AudioStatus {
        let error = self.shared.error();
        AudioStatus {
            state: if self.stream.is_some() && error.is_some() {
                AudioState::Failed
            } else {
                self.state
            },
            output_epoch: self.current_epoch(),
            underrun_count: self.shared.metrics.underruns.load(Ordering::Relaxed) as u64,
            silent_frames: self.shared.metrics.silent_frames.load(Ordering::Relaxed) as u64,
            realtime_denied_count: self.shared.metrics.realtime_denied.load(Ordering::Relaxed)
                as u64,
            latest_device_error: self.control_error.clone().or(error),
        }
    }
    fn clock_observation(&self) -> AudioClockObservation {
        let epoch = self.current_epoch();
        let rate = self.format.as_ref().map_or(0, |f| f.sample_rate_hz);
        // Only the control-side reader locks. The callback publishes through triple_buffer.
        let mut clock = self.clock.lock().expect("control clock poisoned");
        let observation = clock
            .as_mut()
            .map_or(AudioClockObservation::unavailable(epoch, rate), |c| {
                *c.read()
            });
        if self.state == AudioState::Stopped || observation.output_epoch != epoch {
            AudioClockObservation::unavailable(epoch, rate)
        } else {
            observation
        }
    }
    fn stop(&mut self) -> Result<(), AudioError> {
        self.shared.close_and_wait();
        if let Some(slot) = self.fill_slot.take() {
            slot.discard();
        }
        let prior_error = if self.stream.is_some() {
            self.control_error.clone().or_else(|| self.shared.error())
        } else {
            None
        };
        // Native pause may fail. Fill is already disposed on the control side;
        // even a retained native callback now has only an empty CLOSED slot.
        let pause_error = self
            .stream
            .as_ref()
            .and_then(|s| self.driver.pause(s).err());
        drop(self.stream.take());
        self.state = AudioState::Stopped;
        self.wake.wake();
        if let Some(e) = prior_error.or(pause_error) {
            self.control_error = Some(e.clone());
            Err(e)
        } else {
            Ok(())
        }
    }
}
impl<D: Driver> Drop for DrivenOutput<D> {
    fn drop(&mut self) {
        self.shared.close_and_wait();
        if let Some(slot) = self.fill_slot.take() {
            slot.discard();
        }
        drop(self.stream.take());
    }
}

pub(crate) struct OfflineDriver {
    pub device: AudioDeviceConfig,
}
impl Driver for OfflineDriver {
    type Stream = Callback;
    fn capabilities(&self) -> AudioCaps {
        AudioCaps {
            clock: AudioClockCapability::Estimated,
            configurations: vec![AudioConfigRange {
                min_sample_rate_hz: self.device.sample_rate_hz,
                max_sample_rate_hz: self.device.sample_rate_hz,
                sample_format: self.device.sample_format,
                layout: self.device.layout,
            }],
            min_pull_frames: 1,
            max_pull_frames: MAX_PULL_FRAMES,
            suspend: true,
        }
    }
    fn configure(&mut self, request: &AudioRequest) -> Result<AudioFormat, AudioError> {
        negotiate(&self.capabilities(), request)
    }
    fn build(&mut self, _: &AudioFormat, callback: Callback) -> Result<Callback, AudioError> {
        Ok(callback)
    }
    fn play(&mut self, _: &Callback) -> Result<(), AudioError> {
        Ok(())
    }
    fn pause(&mut self, _: &Callback) -> Result<(), AudioError> {
        Ok(())
    }
    fn verify(&self, f: &AudioFormat) -> Result<(), AudioError> {
        if (
            f.sample_rate_hz,
            f.device_sample_format,
            f.device_channel_layout,
        ) != (
            self.device.sample_rate_hz,
            self.device.sample_format,
            self.device.layout,
        ) {
            Err(AudioError::new(
                AudioErrorKind::ReconfigureRequired,
                "offline device configuration changed",
            ))
        } else {
            Ok(())
        }
    }
}

/// Injectable output: the caller drives the same callback used by the desktop backend.
/// Does not claim hardware presentation; supplied timing is always an estimate.
pub struct OfflineOutput {
    inner: DrivenOutput<OfflineDriver>,
}
impl OfflineOutput {
    pub fn new(device: AudioDeviceConfig, wake: WakeSignal) -> Result<Self, AudioError> {
        if device.sample_rate_hz == 0 {
            return Err(AudioError::new(
                AudioErrorKind::InvalidRequest,
                "zero device rate",
            ));
        }
        Ok(Self {
            inner: DrivenOutput::new(OfflineDriver { device }, wake)?,
        })
    }
    pub fn render<T: DeviceSample>(&mut self, data: &mut [T], timing: CallbackTiming) {
        if let Some(callback) = self.inner.stream.as_mut() {
            callback.render(data, timing);
        } else {
            data.fill(T::from_pcm(0.0));
        }
    }
    /// Explicit test-device event, never an automatic format change in the mixer.
    pub fn change_device(&mut self, device: AudioDeviceConfig) {
        self.inner.driver.device = device;
        if self.inner.stream.is_some() {
            self.inner.shared.fail(AudioErrorKind::ReconfigureRequired);
        }
    }
    pub fn fail_device(&mut self) {
        self.inner.shared.fail(AudioErrorKind::DeviceFailure);
    }
    pub fn underrun_signal(&self) -> UnderrunSignal {
        UnderrunSignal::from_shared(self.inner.shared.clone())
    }
}
macro_rules! output_methods {
    () => {
        fn capabilities(&self) -> AudioCaps {
            self.inner.capabilities()
        }
        fn configure(&mut self, request: AudioRequest) -> Result<AudioFormat, AudioError> {
            self.inner.configure(request)
        }
        fn start(&mut self, fill: PcmFill) -> Result<(), Rejected<PcmFill>> {
            self.inner.start(fill)
        }
        fn suspend(&mut self) -> Result<(), AudioError> {
            self.inner.suspend()
        }
        fn resume(&mut self) -> Result<(), AudioError> {
            self.inner.resume()
        }
        fn status(&self) -> AudioStatus {
            self.inner.status()
        }
        fn clock_observation(&self) -> AudioClockObservation {
            self.inner.clock_observation()
        }
        fn stop(&mut self) -> Result<(), AudioError> {
            self.inner.stop()
        }
    };
}
#[cfg(all(
    feature = "cpal-output",
    any(target_os = "linux", target_os = "windows", target_os = "macos")
))]
pub(crate) use output_methods;
impl AudioOutput for OfflineOutput {
    output_methods!();
}

/// Attach a bounded PCM queue's shortage accounting to one output.
#[derive(Clone)]
pub struct UnderrunSignal(Arc<Telemetry>);
impl UnderrunSignal {
    pub(crate) fn from_shared(shared: Arc<Shared>) -> Self {
        Self(shared.metrics.clone())
    }
}
/// Preallocated stereo PCM bridge for worker-produced samples. No waiting at either end.
/// A queue shortage freezes content consumption while the output device clock advances.
pub fn bounded_pcm_queue(
    capacity_frames: usize,
    signal: UnderrunSignal,
) -> Result<(rtrb::Producer<[f32; 2]>, PcmFill), AudioError> {
    if capacity_frames == 0 || std::alloc::Layout::array::<[f32; 2]>(capacity_frames).is_err() {
        return Err(AudioError::new(
            AudioErrorKind::InvalidRequest,
            "PCM queue requires a nonzero, representable frame capacity",
        ));
    }
    let (producer, mut consumer) = RingBuffer::<[f32; 2]>::new(capacity_frames);
    let fill: PcmFill = Box::new(move |out| {
        let mut missing = 0;
        for frame in out.chunks_exact_mut(2) {
            if let Ok([left, right]) = consumer.pop() {
                frame[0] = left;
                frame[1] = right;
            } else {
                frame.fill(0.0);
                missing += 1;
            }
        }
        if missing != 0 {
            signal.0.underrun(missing);
        }
    });
    Ok((producer, fill))
}

#[cfg(test)]
mod tests;
