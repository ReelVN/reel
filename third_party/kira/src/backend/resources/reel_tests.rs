//! Deterministic scheduling is compiled only by the independent fork test script.
use crate::{
    AudioManager, AudioManagerSettings, Frame,
    backend::{Backend, Renderer},
    sound::{
        PlaybackState,
        static_sound::{StaticSoundData, StaticSoundSettings},
    },
    track::MainTrackBuilder,
};
use std::{
    cell::RefCell,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
struct Schedule {
    removed: Barrier,
    resume: Barrier,
    attempts: AtomicUsize,
}
thread_local! { static SCHEDULE: RefCell<Option<Arc<Schedule>>> = const { RefCell::new(None) }; }
pub(super) fn before_unused_push() {
    let schedule = SCHEDULE.with(|s| s.borrow().clone());
    if let Some(schedule) = schedule {
        if schedule.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            schedule.removed.wait();
            schedule.resume.wait();
        }
    }
}
struct Capture(Option<Renderer>);
impl Backend for Capture {
    type Settings = ();
    type Error = ();
    fn setup(_: (), _: usize) -> Result<(Self, u32), ()> {
        Ok((Self(None), 48_000))
    }
    fn start(&mut self, renderer: Renderer) -> Result<(), ()> {
        self.0 = Some(renderer);
        Ok(())
    }
}
fn sound(left: f32, right: f32) -> StaticSoundData {
    StaticSoundData {
        sample_rate: 48_000,
        frames: vec![Frame::new(left, right); 64].into(),
        settings: StaticSoundSettings::default(),
        slice: None,
    }
}
#[test]
fn reel_retirement_interleaving_retains_pcm_and_explicit_reclaim_has_quota() {
    let mut manager = AudioManager::<Capture>::new(AudioManagerSettings {
        backend_settings: (),
        capacities: crate::Capacities {
            sub_track_capacity: 0,
            send_track_capacity: 0,
            clock_capacity: 0,
            modulator_capacity: 0,
            listener_capacity: 0,
        },
        main_track_builder: MainTrackBuilder::new().sound_capacity(1),
        internal_buffer_size: 64,
    })
    .unwrap();
    let a = sound(0.1, -0.2);
    let a_pcm = Arc::downgrade(&a.frames);
    let a_handle = manager.play(a).unwrap();
    let mut renderer = manager.backend_mut().0.take().unwrap();
    let schedule = Arc::new(Schedule {
        removed: Barrier::new(2),
        resume: Barrier::new(2),
        attempts: AtomicUsize::new(0),
    });
    let worker_schedule = schedule.clone();
    let (commands, requests) = mpsc::channel();
    let (reports, results) = mpsc::channel();
    let worker = thread::spawn(move || {
        SCHEDULE.with(|s| *s.borrow_mut() = Some(worker_schedule));
        let mut pcm = [0.0; 512 * 2];
        while requests.recv().unwrap() {
            renderer.on_start_processing();
            renderer.process(&mut pcm, 2);
            let mut peak = [0.0f32; 2];
            for f in pcm.chunks_exact(2) {
                for c in 0..2 {
                    if f[c].abs() > peak[c].abs() {
                        peak[c] = f[c];
                    }
                }
            }
            reports.send(peak).unwrap();
        }
        renderer // Teardown stays on the control thread, as in Reel's fill ownership.
    });
    let render = || {
        commands.send(true).unwrap();
        results.recv_timeout(Duration::from_secs(5)).unwrap()
    };
    assert_eq!(render(), [0.1, -0.2]);
    assert_eq!(a_handle.state(), PlaybackState::Stopped);
    commands.send(true).unwrap();
    schedule.removed.wait(); // A arena slot freed, but original queue push not yet run.
    assert!(a_pcm.upgrade().is_some());
    let b = sound(0.3, -0.4);
    let b_pcm = Arc::downgrade(&b.frames);
    let b_handle = manager.play(b).unwrap();
    schedule.resume.wait();
    assert_eq!(
        results.recv_timeout(Duration::from_secs(5)).unwrap(),
        [0.3, -0.4]
    );
    assert_eq!(b_handle.state(), PlaybackState::Stopped);
    assert_eq!(render(), [0.0, 0.0]); // unused is full; BEFORE next(), B stays in arena.
    assert_eq!(schedule.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(manager.main_track().num_sounds(), 1);
    assert!(a_pcm.upgrade().is_some() && b_pcm.upgrade().is_some());
    assert_eq!(manager.main_track().reclaim_unused(0), (0, true));
    assert!(a_pcm.upgrade().is_some());
    assert_eq!(manager.main_track().reclaim_unused(1), (1, true));
    assert!(a_pcm.upgrade().is_none() && b_pcm.upgrade().is_some());
    render(); // With credit, B can now move to unused.
    assert_eq!(schedule.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(manager.main_track().num_sounds(), 0);
    let _c = manager.play(sound(0.5, -0.6)).unwrap();
    assert!(b_pcm.upgrade().is_some()); // Successful play(C) MUST NOT secretly drain B.
    assert_eq!(manager.main_track().reclaim_unused(usize::MAX), (1, true)); // capacity clamps quota to 1.
    assert!(b_pcm.upgrade().is_none());
    assert_eq!(manager.main_track().reclaim_unused(1), (0, false));
    commands.send(false).unwrap();
    drop(worker.join().unwrap());
    println!(
        "real_kira A=[0.1,-0.2] B=[0.3,-0.4]; freed-before-publish schedule; full queue retained B; quota=0/1 respected; play(C) did not reclaim; PCM dropped by control"
    );
}
