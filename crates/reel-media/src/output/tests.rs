use super::*;
use reel_core::clock::MonoTime;
use std::{
    cell::RefCell,
    sync::{atomic::AtomicUsize, mpsc},
    time::Duration,
};

fn timing() -> CallbackTiming {
    CallbackTiming {
        now: MonoTime(1_000_000),
        latency_frames: Some(0),
        uncertainty_frames: Some(0),
    }
}

#[test]
fn native_close_failure_is_reported_after_fill_is_closed_and_disposed() {
    let mut driver = SyncDriver::new();
    driver.close_fail = true;
    let mut output = DrivenOutput::new(driver, WakeSignal::new(|| {})).unwrap();
    output.configure(AudioRequest::default()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    assert!(
        output
            .start(counted_fill(calls.clone(), drops.clone()))
            .is_ok()
    );
    let error = output.stop().unwrap_err();
    assert_eq!(error.message, "native release failed");
    assert_eq!(calls.load(Ordering::Acquire), 0);
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert!(output.stream.is_none());
}
fn device() -> AudioDeviceConfig {
    AudioDeviceConfig {
        sample_rate_hz: 48_000,
        sample_format: SampleFormat::F32,
        layout: ChannelLayout::Stereo,
    }
}
struct SyncDriver {
    offline: OfflineDriver,
    prebuild: bool,
    builds: usize,
    closes: Arc<AtomicUsize>,
    build_fail: bool,
    play_fail: bool,
    pause_fail: bool,
    close_fail: bool,
    verify_fail: bool,
    async_fail: bool,
    retired: Option<Arc<Shared>>,
    clock: AudioClockCapability,
}
impl SyncDriver {
    fn new() -> Self {
        Self {
            offline: OfflineDriver { device: device() },
            prebuild: false,
            builds: 0,
            closes: Arc::new(AtomicUsize::new(0)),
            build_fail: false,
            play_fail: false,
            pause_fail: false,
            close_fail: false,
            verify_fail: false,
            async_fail: false,
            retired: None,
            clock: AudioClockCapability::Estimated,
        }
    }
}
impl Driver for SyncDriver {
    type Stream = RefCell<Callback>;
    fn prepare_on_configure(&self) -> bool {
        self.prebuild
    }
    fn close(&mut self, stream: Self::Stream) -> Result<(), AudioError> {
        self.closes.fetch_add(1, Ordering::Relaxed);
        drop(stream);
        if self.close_fail {
            Err(AudioError::new(
                AudioErrorKind::DeviceFailure,
                "native release failed",
            ))
        } else {
            Ok(())
        }
    }
    fn capabilities(&self) -> AudioCaps {
        let mut caps = self.offline.capabilities();
        caps.clock = self.clock;
        caps
    }
    fn configure(&mut self, r: &AudioRequest) -> Result<AudioFormat, AudioError> {
        self.offline.configure(r)
    }
    fn build(&mut self, _: &AudioFormat, mut cb: Callback) -> Result<Self::Stream, AudioError> {
        self.builds += 1;
        // Deliberately hostile native API: callback during build, including error return.
        let mut data = [42.0f32; 6];
        cb.render(&mut data, timing());
        assert_eq!(data, [0.0f32; 6]);
        self.retired = Some(cb.shared.clone());
        if self.build_fail {
            cb.shared.fail(AudioErrorKind::DeviceFailure);
            return Err(AudioError::new(AudioErrorKind::Unavailable, "build failed"));
        }
        Ok(RefCell::new(cb))
    }
    fn play(&mut self, cb: &Self::Stream) -> Result<(), AudioError> {
        let mut data = [42.0f32; 6];
        cb.borrow_mut().render(&mut data, timing());
        assert_eq!(data, [0.0f32; 6]);
        if self.play_fail {
            return Err(AudioError::new(
                AudioErrorKind::DeviceFailure,
                "simulated play operation rejected",
            ));
        }
        if self.async_fail {
            cb.borrow().shared.fail(AudioErrorKind::DeviceFailure);
        }
        Ok(())
    }
    fn pause(&mut self, _: &Self::Stream) -> Result<(), AudioError> {
        if self.pause_fail {
            Err(AudioError::new(
                AudioErrorKind::DeviceFailure,
                "pause failed",
            ))
        } else {
            Ok(())
        }
    }
    fn verify(&self, _: &AudioFormat) -> Result<(), AudioError> {
        if self.verify_fail {
            Err(AudioError::new(
                AudioErrorKind::ReconfigureRequired,
                "format changed",
            ))
        } else {
            Ok(())
        }
    }
}
fn output() -> DrivenOutput<SyncDriver> {
    let mut output = DrivenOutput::new(SyncDriver::new(), WakeSignal::new(|| {})).unwrap();
    output.configure(AudioRequest::default()).unwrap();
    output
}
fn counted_fill(calls: Arc<AtomicUsize>, drops: Arc<AtomicUsize>) -> PcmFill {
    struct DropCount(Arc<AtomicUsize>);
    impl Drop for DropCount {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let owned = DropCount(drops);
    Box::new(move |pcm| {
        let _keep = &owned;
        calls.fetch_add(1, Ordering::Relaxed);
        pcm.fill(0.25);
    })
}

#[test]
fn prepared_native_stream_stays_silent_and_first_start_reuses_it() {
    let mut driver = SyncDriver::new();
    driver.prebuild = true;
    let closes = driver.closes.clone();
    let mut output = DrivenOutput::new(driver, WakeSignal::new(|| {})).unwrap();
    output.configure(AudioRequest::default()).unwrap();
    assert_eq!(output.driver.builds, 1);
    assert_eq!(output.status().state, AudioState::Stopped);
    assert_eq!(output.clock_observation().generated_frames, 0);
    let mut data = [42.0f32; 6];
    output
        .prepared
        .as_ref()
        .unwrap()
        .stream
        .borrow_mut()
        .render(&mut data, timing());
    assert_eq!(data, [0.0; 6]);
    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    assert!(
        output
            .start(counted_fill(calls.clone(), drops.clone()))
            .is_ok()
    );
    assert_eq!(output.driver.builds, 1);
    assert!(output.prepared.is_none());
    output
        .stream
        .as_ref()
        .unwrap()
        .borrow_mut()
        .render(&mut data, timing());
    assert_eq!(data, [0.25; 6]);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    output.stop().unwrap();
    assert!(output.close_complete());
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(closes.load(Ordering::Relaxed), 1);
}

#[test]
fn unused_prepared_stream_retires_on_reconfigure_stop_and_drop() {
    let mut driver = SyncDriver::new();
    driver.prebuild = true;
    let closes = driver.closes.clone();
    let mut output = DrivenOutput::new(driver, WakeSignal::new(|| {})).unwrap();
    output.configure(AudioRequest::default()).unwrap();
    let old = output.driver.retired.clone().unwrap();
    output.configure(AudioRequest::default()).unwrap();
    assert_eq!(closes.load(Ordering::Relaxed), 1);
    old.fail(AudioErrorKind::DeviceFailure);
    assert!(output.start(Box::new(|pcm| pcm.fill(0.0))).is_ok());
    assert_eq!(output.status().state, AudioState::Running);
    output.stop().unwrap();
    output.configure(AudioRequest::default()).unwrap();
    output.stop().unwrap();
    assert!(output.close_complete());
    assert_eq!(closes.load(Ordering::Relaxed), 3);
    output.configure(AudioRequest::default()).unwrap();
    drop(output);
    assert_eq!(closes.load(Ordering::Relaxed), 4);
}

#[test]
fn prepared_stream_play_rejection_returns_the_uncalled_fill() {
    let mut driver = SyncDriver::new();
    driver.prebuild = true;
    let mut output = DrivenOutput::new(driver, WakeSignal::new(|| {})).unwrap();
    output.configure(AudioRequest::default()).unwrap();
    output.driver.play_fail = true;
    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let fill = counted_fill(calls.clone(), drops.clone());
    let address = (&*fill as *const dyn FnMut(&mut [f32])) as *const ();
    let rejected = output.start(fill).unwrap_err();
    assert_eq!(
        address,
        (&*rejected.request as *const dyn FnMut(&mut [f32])) as *const ()
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert!(output.close_complete());
    drop(rejected);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}
#[test]
fn native_build_and_play_rejection_return_identical_uncalled_fill() {
    for stage in 0..3 {
        let mut output = output();
        output.driver.build_fail = stage == 0;
        output.driver.play_fail = stage == 1;
        output.driver.verify_fail = stage == 2;
        let calls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let fill = counted_fill(calls.clone(), drops.clone());
        let address = (&*fill as *const dyn FnMut(&mut [f32])) as *const ();
        let mut rejected = output.start(fill).expect_err("must reject");
        assert_eq!(
            address,
            (&*rejected.request as *const dyn FnMut(&mut [f32])) as *const ()
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert_eq!(output.status().state, AudioState::Stopped);
        assert!(output.status().latest_device_error.is_none());
        (rejected.request)(&mut [0.0f32; 2]);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        drop(rejected);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}
#[test]
fn accepted_async_failure_retains_fill_until_stop_and_old_faults_are_isolated() {
    let mut output = output();
    output.driver.async_fail = true;
    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    assert!(
        output
            .start(counted_fill(calls.clone(), drops.clone()))
            .is_ok()
    );
    let failed_epoch = output.status().output_epoch;
    assert_eq!(output.status().state, AudioState::Failed);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    let retired = output.driver.retired.clone().unwrap();
    assert!(output.stop().is_err());
    assert_eq!(output.status().state, AudioState::Stopped);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    output.driver.async_fail = false;
    assert!(
        output
            .start(counted_fill(calls.clone(), drops.clone()))
            .is_ok()
    );
    assert!(output.status().output_epoch.0 > failed_epoch.0);
    retired.fail(AudioErrorKind::ReconfigureRequired);
    let mut data = [0.0f32; 2];
    output
        .stream
        .as_ref()
        .unwrap()
        .borrow_mut()
        .render(&mut data, timing());
    assert_eq!(data, [0.25; 2]);
    assert_eq!(output.status().state, AudioState::Running);
}
#[test]
fn suspend_resume_failures_retain_and_stop_error_still_drops_fill() {
    for on_resume in [false, true] {
        let mut output = output();
        let calls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        assert!(
            output
                .start(counted_fill(calls.clone(), drops.clone()))
                .is_ok()
        );
        if on_resume {
            output.suspend().unwrap();
            output.driver.verify_fail = true;
            assert_eq!(
                output.resume().unwrap_err().kind,
                AudioErrorKind::ReconfigureRequired
            );
        } else {
            output.driver.pause_fail = true;
            assert!(output.suspend().is_err());
        }
        assert_eq!(output.status().state, AudioState::Failed);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        let mut data = [42.0f32; 2];
        output
            .stream
            .as_ref()
            .unwrap()
            .borrow_mut()
            .render(&mut data, timing());
        assert_eq!(data, [0.0f32; 2]);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(output.stop().is_err());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(output.status().state, AudioState::Stopped);
    }
}

// Worker owns Callback without a Mutex. The deliberately blocked test fill makes
// the in-flight interval observable; production fills must always be bounded.
enum Command {
    Render(mpsc::Sender<()>),
    Quit,
}
struct ThreadStream {
    command: mpsc::Sender<Command>,
    worker: Option<JoinHandle<()>>,
}
impl Drop for ThreadStream {
    fn drop(&mut self) {
        let _ = self.command.send(Command::Quit);
        self.worker.take().unwrap().join().unwrap();
    }
}
struct ThreadDriver(OfflineDriver);
impl Driver for ThreadDriver {
    type Stream = ThreadStream;
    fn close(&mut self, stream: Self::Stream) -> Result<(), AudioError> {
        drop(stream);
        Ok(())
    }
    fn capabilities(&self) -> AudioCaps {
        self.0.capabilities()
    }
    fn configure(&mut self, r: &AudioRequest) -> Result<AudioFormat, AudioError> {
        self.0.configure(r)
    }
    fn build(&mut self, _: &AudioFormat, mut cb: Callback) -> Result<ThreadStream, AudioError> {
        let (command, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            while let Ok(command) = receiver.recv() {
                match command {
                    Command::Render(done) => {
                        cb.render(&mut [0.0f32; 8], timing());
                        done.send(()).unwrap();
                    }
                    Command::Quit => break,
                }
            }
        });
        Ok(ThreadStream {
            command,
            worker: Some(worker),
        })
    }
    fn play(&mut self, _: &ThreadStream) -> Result<(), AudioError> {
        Ok(())
    }
    fn pause(&mut self, _: &ThreadStream) -> Result<(), AudioError> {
        Ok(())
    }
    fn verify(&self, f: &AudioFormat) -> Result<(), AudioError> {
        self.0.verify(f)
    }
}
#[test]
fn suspend_and_stop_close_entry_then_wait_for_inflight_call() {
    for suspend in [false, true] {
        let mut output = DrivenOutput::new(
            ThreadDriver(OfflineDriver { device: device() }),
            WakeSignal::new(|| {}),
        )
        .unwrap();
        output.configure(AudioRequest::default()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut inner = counted_fill(calls.clone(), drops.clone());
        assert!(
            output
                .start(Box::new(move |pcm| {
                    inner(pcm);
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                }))
                .is_ok()
        );
        let command = output.stream.as_ref().unwrap().command.clone();
        let (done_tx, done_rx) = mpsc::channel();
        command.send(Command::Render(done_tx)).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let shared = output.shared.clone();
        let (stopped_tx, stopped_rx) = mpsc::channel();
        let control = thread::spawn(move || {
            if suspend {
                output.suspend().unwrap();
            } else {
                output.stop().unwrap();
            }
            stopped_tx.send(()).unwrap();
            output
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while shared.gate.load(Ordering::Acquire) & CLOSED == 0 {
            assert!(std::time::Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(stopped_rx.try_recv().is_err()); // cannot return while fill still owns the block.
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        stopped_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut output = control.join().unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        if suspend {
            let (done_tx, done_rx) = mpsc::channel();
            command.send(Command::Render(done_tx)).unwrap();
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            assert_eq!(drops.load(Ordering::Relaxed), 0);
            output.stop().unwrap();
        }
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(output.status().state, AudioState::Stopped);
    }
}
#[test]
fn realtime_fault_wakes_control_without_polling_the_output() {
    let (tx, rx) = mpsc::channel();
    let wake = WakeSignal::new(move || {
        tx.send(()).unwrap();
    });
    let mut output = DrivenOutput::new(SyncDriver::new(), wake.clone()).unwrap();
    output.configure(AudioRequest::default()).unwrap();
    assert!(output.start(Box::new(|_| {})).is_ok());
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    wake.acknowledge();
    output.shared.fail(AudioErrorKind::DeviceFailure);
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(output.status().state, AudioState::Failed);
}

#[test]
fn scheduling_denial_is_visible_without_discarding_accepted_fill() {
    let mut output = output();
    assert!(output.start(Box::new(|pcm| pcm.fill(0.5))).is_ok());
    output.shared.realtime_denied();
    assert_eq!(output.status().state, AudioState::Running);
    assert_eq!(output.status().realtime_denied_count, 1);
    let mut pcm = [0.0f32; 2];
    output
        .stream
        .as_ref()
        .unwrap()
        .borrow_mut()
        .render(&mut pcm, timing());
    assert_eq!(pcm, [0.5; 2]);
}

// Model a native API retaining its callback after the outer Stream is disposed.
// The fill must still be destroyed synchronously on the stop caller's thread.
#[test]
fn stop_disposes_fill_even_if_native_callback_object_is_retained() {
    use std::rc::Rc;
    struct RetainedStream {
        callback: Option<Callback>,
        retired: Rc<RefCell<Option<Callback>>>,
    }
    impl Drop for RetainedStream {
        fn drop(&mut self) {
            *self.retired.borrow_mut() = self.callback.take();
        }
    }
    struct RetainingDriver {
        offline: OfflineDriver,
        retired: Rc<RefCell<Option<Callback>>>,
    }
    impl Driver for RetainingDriver {
        type Stream = RetainedStream;
        fn close(&mut self, stream: Self::Stream) -> Result<(), AudioError> {
            drop(stream);
            Ok(())
        }
        fn capabilities(&self) -> AudioCaps {
            self.offline.capabilities()
        }
        fn configure(&mut self, r: &AudioRequest) -> Result<AudioFormat, AudioError> {
            self.offline.configure(r)
        }
        fn build(
            &mut self,
            _: &AudioFormat,
            callback: Callback,
        ) -> Result<Self::Stream, AudioError> {
            Ok(RetainedStream {
                callback: Some(callback),
                retired: self.retired.clone(),
            })
        }
        fn play(&mut self, _: &Self::Stream) -> Result<(), AudioError> {
            Ok(())
        }
        fn pause(&mut self, _: &Self::Stream) -> Result<(), AudioError> {
            Err(AudioError::new(
                AudioErrorKind::DeviceFailure,
                "simulated native pause error",
            ))
        }
        fn verify(&self, f: &AudioFormat) -> Result<(), AudioError> {
            self.offline.verify(f)
        }
    }
    struct DropThread {
        expected: std::thread::ThreadId,
        drops: Arc<AtomicUsize>,
    }
    impl Drop for DropThread {
        fn drop(&mut self) {
            assert_eq!(std::thread::current().id(), self.expected);
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
    let retired = Rc::new(RefCell::new(None));
    let mut output = DrivenOutput::new(
        RetainingDriver {
            offline: OfflineDriver { device: device() },
            retired: retired.clone(),
        },
        WakeSignal::new(|| {}),
    )
    .unwrap();
    output.configure(AudioRequest::default()).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let owned = DropThread {
        expected: std::thread::current().id(),
        drops: drops.clone(),
    };
    assert!(
        output
            .start(Box::new(move |pcm| {
                let _hold = &owned;
                pcm.fill(0.25);
            }))
            .is_ok()
    );
    let mut pcm = [0.0f32; 2];
    output
        .stream
        .as_mut()
        .unwrap()
        .callback
        .as_mut()
        .unwrap()
        .render(&mut pcm, timing());
    assert_eq!(pcm, [0.25; 2]);
    assert!(output.stop().is_err());
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(output.status().state, AudioState::Stopped);
    pcm.fill(42.0);
    retired
        .borrow_mut()
        .as_mut()
        .unwrap()
        .render(&mut pcm, timing());
    assert_eq!(pcm, [0.0; 2]);
    assert!(
        retired
            .borrow()
            .as_ref()
            .unwrap()
            .fill_slot
            .0
            .borrow_mut()
            .get_mut()
            .is_none()
    );
}

#[test]
fn native_generated_then_write_failure_invalidates_epoch_and_freezes_counts() {
    let service =
        AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
    use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
    let mut output = output();
    output.driver.clock = AudioClockCapability::Unavailable; // CPAL's declared capability.
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 1,
            max_bytes: 1_024,
        },
    );
    let mut mixer = KiraMixer::new(
        output.format.as_ref().unwrap(),
        MixerLimits {
            max_sounds: 1,
            internal_buffer_frames: 64,
        },
        &service,
    )
    .unwrap();
    let (_, ticket) = mixer.reserve_decode_budget(&pool, 1_024, 1);
    mixer
        .play(
            DecodedSound::new(48_000, vec![[0.1, -0.2]; 64], ticket.unwrap())
                .unwrap()
                .looping(),
        )
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let fill_calls = calls.clone();
    let mut real_fill = mixer.take_fill().unwrap();
    assert!(
        output
            .start(Box::new(move |out| {
                fill_calls.fetch_add(1, Ordering::Relaxed);
                real_fill(out);
            }))
            .is_ok()
    );
    assert_eq!(
        output.capabilities().clock,
        AudioClockCapability::Unavailable
    );
    let epoch = output.status().output_epoch;
    let mut data = [0.0f32; 512 * 2];
    output
        .stream
        .as_ref()
        .unwrap()
        .borrow_mut()
        .render(&mut data, timing());
    assert!(
        data.as_chunks::<2>()
            .0
            .iter()
            .any(|f| (f[0] - 0.1).abs() < 1e-6 && (f[1] + 0.2).abs() < 1e-6)
    );
    let generated = output.clock_observation();
    assert_eq!(generated.generated_frames, 512);
    assert_eq!(generated.submitted_frames(), None);
    assert_eq!(generated.submitted_frames, 0); // native acceptance unknown, not software generation.
    assert_eq!(generated.estimated_presented_frames, 0);
    assert_eq!(generated.presented_frames(), None);
    assert_eq!(generated.latency_frames, Some(0)); // diagnostic does not make a clock.
    // CPAL's legal order: render, writei EPIPE before any acceptance, Xrun,
    // prepare, next callback. The simulated device accepted/presented zero frames.
    #[cfg(all(
        feature = "cpal-output",
        any(target_os = "linux", target_os = "windows", target_os = "macos")
    ))]
    {
        assert_eq!(
            crate::desktop::CPAL_CLOCK_CAPABILITY,
            AudioClockCapability::Unavailable
        );
        crate::desktop::native_error(&output.shared, cpal::ErrorKind::Xrun);
    }
    #[cfg(not(all(
        feature = "cpal-output",
        any(target_os = "linux", target_os = "windows", target_os = "macos")
    )))]
    output.shared.native_xrun();
    let fault = output.clock_observation();
    assert!(fault.output_epoch.0 > epoch.0);
    assert_eq!(fault.precision, ClockPrecision::Unavailable);
    assert_eq!(fault.generated_frames, 0); // old software diagnostics are invalidated too.
    assert_eq!(fault.presented_frames(), None);
    assert_eq!(output.status().state, AudioState::Failed);
    output
        .stream
        .as_ref()
        .unwrap()
        .borrow_mut()
        .render(&mut data, timing());
    assert!(data.iter().all(|v| *v == 0.0));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(output.stream.as_ref().unwrap().borrow().generated, 512); // private old counter frozen.
    assert_eq!(output.clock_observation(), fault);
    assert_eq!(
        output.stop().unwrap_err().kind,
        AudioErrorKind::ReconfigureRequired
    );
    drop(mixer);
    let format = output.configure(AudioRequest::default()).unwrap();
    let mut rebuilt = KiraMixer::new(&format, MixerLimits::default(), &service).unwrap();
    assert!(output.start(rebuilt.take_fill().unwrap()).is_ok());
    assert!(output.status().output_epoch.0 > fault.output_epoch.0);
    output
        .stream
        .as_ref()
        .unwrap()
        .borrow_mut()
        .render(&mut data, timing());
    assert_eq!(output.clock_observation().generated_frames, 512);
    assert_eq!(output.clock_observation().presented_frames(), None);
    output.stop().unwrap();
    println!(
        "native EPIPE: real Kira generated=512; submitted/presented unknown; epoch {}->{}; closed next callback stays silent/calls=1; software count invalidated; rebuild has fresh diagnostics",
        epoch.0, fault.output_epoch.0
    );
}
#[test]
fn native_silent_eagain_before_any_write_never_promotes_software_count_to_clock() {
    let mut output = output();
    output.driver.clock = AudioClockCapability::Unavailable;
    assert!(output.start(Box::new(|pcm| pcm.fill(0.25))).is_ok());
    let mut data = [0.0f32; 512 * 2];
    output
        .stream
        .as_ref()
        .unwrap()
        .borrow_mut()
        .render(&mut data, timing());
    // ALSA writei Err(EAGAIN), frames_written==0 => CPAL returns Ok and silently
    // discards this generated block. writei Ok(0) is a retry, NOT this branch.
    output
        .stream
        .as_ref()
        .unwrap()
        .borrow_mut()
        .render(&mut data, timing());
    let observation = output.clock_observation();
    assert_eq!(observation.generated_frames, 1_024);
    assert_eq!(observation.submitted_frames(), None);
    assert_eq!(observation.submitted_frames, 0);
    assert_eq!(observation.estimated_presented_frames, 0);
    assert_eq!(observation.presented_frames(), None);
    assert_eq!(observation.precision, ClockPrecision::Unavailable);
    assert_eq!(output.status().underrun_count, 0); // CPAL delivered no error event.
    output.stop().unwrap();
    println!(
        "native EAGAIN with frames_written=0: generated=1024, submitted=unknown, presented=None despite latency=0; no Xrun event needed for safety"
    );
}

#[test]
fn unavailable_precision_hides_both_queries_and_preserves_generated_diagnostics() {
    let mut output = output(); // controlled Estimated-capability driver
    assert!(output.start(Box::new(|pcm| pcm.fill(0.25))).is_ok());
    let mut data = [0.0f32; 2];
    let mut no_estimate = timing();
    no_estimate.latency_frames = None;
    output
        .stream
        .as_ref()
        .unwrap()
        .borrow_mut()
        .render(&mut data, no_estimate);
    let unavailable = output.clock_observation();
    assert_eq!(unavailable.generated_frames, 1);
    assert_eq!(unavailable.precision, ClockPrecision::Unavailable);
    assert_eq!(unavailable.submitted_frames, 0);
    assert_eq!(unavailable.estimated_presented_frames, 0);
    assert_eq!(unavailable.submitted_frames(), None);
    assert_eq!(unavailable.presented_frames(), None);
    output
        .stream
        .as_ref()
        .unwrap()
        .borrow_mut()
        .render(&mut data, timing());
    assert_eq!(output.clock_observation().submitted_frames(), Some(2));
    assert_eq!(output.clock_observation().presented_frames(), Some(1));
    output.stop().unwrap();
}

#[test]
fn configured_notifier_has_no_periodic_wait_before_native_producers_exist() {
    let output = DrivenOutput::new(SyncDriver::new(), WakeSignal::new(|| {})).unwrap();
    let limit = std::time::Instant::now() + Duration::from_secs(2);
    while output._notifier.waits.load(Ordering::Acquire) == 0 {
        assert!(std::time::Instant::now() < limit);
        thread::park_timeout(Duration::from_millis(1));
    }
    let before = output._notifier.waits.load(Ordering::Acquire);
    thread::sleep(Duration::from_millis(40));
    assert_eq!(output._notifier.waits.load(Ordering::Acquire), before);
}
#[test]
fn suspended_output_keeps_the_bounded_native_error_notification_fallback() {
    let (send, receive) = mpsc::channel();
    let wake = WakeSignal::new(move || {
        let _ = send.send(());
    });
    let mut output = DrivenOutput::new(SyncDriver::new(), wake.clone()).unwrap();
    output.configure(AudioRequest::default()).unwrap();
    assert!(output.start(Box::new(|_| {})).is_ok());
    receive.recv_timeout(Duration::from_secs(2)).unwrap();
    wake.acknowledge();
    output.suspend().unwrap();
    receive.recv_timeout(Duration::from_secs(2)).unwrap();
    wake.acknowledge();
    output.shared.fail(AudioErrorKind::DeviceFailure);
    receive.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(output.status().state, AudioState::Failed);
}

#[test]
fn external_pcm_metrics_are_watched_before_device_start() {
    let (send, receive) = mpsc::channel();
    let output = OfflineOutput::new(
        AudioDeviceConfig {
            sample_rate_hz: 48000,
            sample_format: SampleFormat::F32,
            layout: ChannelLayout::Stereo,
        },
        WakeSignal::new(move || {
            let _ = send.send(());
        }),
    )
    .unwrap();
    let (_, mut fill) = bounded_pcm_queue(2, output.underrun_signal()).unwrap();
    fill(&mut [0.; 2]);
    receive.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(output.status().silent_frames, 1);
}
