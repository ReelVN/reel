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
fn device() -> AudioDeviceConfig {
    AudioDeviceConfig {
        sample_rate_hz: 48_000,
        sample_format: SampleFormat::F32,
        layout: ChannelLayout::Stereo,
    }
}
struct SyncDriver {
    offline: OfflineDriver,
    build_fail: bool,
    play_fail: bool,
    pause_fail: bool,
    verify_fail: bool,
    async_fail: bool,
    retired: Option<Arc<Shared>>,
}
impl SyncDriver {
    fn new() -> Self {
        Self {
            offline: OfflineDriver { device: device() },
            build_fail: false,
            play_fail: false,
            pause_fail: false,
            verify_fail: false,
            async_fail: false,
            retired: None,
        }
    }
}
impl Driver for SyncDriver {
    type Stream = RefCell<Callback>;
    fn capabilities(&self) -> AudioCaps {
        self.offline.capabilities()
    }
    fn configure(&mut self, r: &AudioRequest) -> Result<AudioFormat, AudioError> {
        self.offline.configure(r)
    }
    fn build(&mut self, _: &AudioFormat, mut cb: Callback) -> Result<Self::Stream, AudioError> {
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
