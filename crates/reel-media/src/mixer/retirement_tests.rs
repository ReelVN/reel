use super::*;
use crate::retirement::{
    ActivityState, AudioActivityRetirement, AudioOwner, DescriptorRelease, PhysicalRelease,
};
use crate::{ChannelLayout, ReclaimLimits, SampleFormat, maintenance::JobKind};
use reel_core::budget::{BudgetCategory, QueueLimits};
use reel_core::identity::Scope;
use std::{
    sync::{Barrier, atomic::AtomicUsize, mpsc},
    thread,
    time::Instant,
};

fn format() -> AudioFormat {
    AudioFormat::new(48_000, SampleFormat::F32, ChannelLayout::Stereo, 512).unwrap()
}
fn limits(n: usize) -> MixerLimits {
    MixerLimits {
        max_sounds: n,
        internal_buffer_frames: 64,
    }
}
fn wait(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !done() {
        assert!(Instant::now() < deadline, "bounded test wait timed out");
        thread::park_timeout(Duration::from_millis(1));
    }
}
fn sound(pool: &BudgetPool, value: f32) -> DecodedSound {
    DecodedSound::new(
        48_000,
        vec![[value, -value]; 64],
        pool.reserve(1024).unwrap(),
    )
    .unwrap()
}
fn gate_worker(
    service: &AudioReclaimService,
    target: JobKind,
) -> (mpsc::Receiver<()>, Arc<Barrier>) {
    let (send, entered) = mpsc::channel();
    let release = Arc::new(Barrier::new(2));
    let worker_release = release.clone();
    let attempts = AtomicUsize::new(0);
    *service.shared.test_hook.lock().unwrap() = Some(Arc::new(move |kind| {
        if kind == target && attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            send.send(()).unwrap();
            worker_release.wait();
        }
    }));
    (entered, release)
}

#[test]
fn single_budget_a_to_b_waits_for_actual_background_release_and_wakes() {
    for (label, queue_limits) in [
        (
            "one-ticket",
            QueueLimits {
                max_requests: 1,
                max_bytes: 16_384,
            },
        ),
        (
            "one-sound-bytes",
            QueueLimits {
                max_requests: 8,
                max_bytes: 1024,
            },
        ),
    ] {
        let pool = BudgetPool::new(BudgetCategory::CpuOutput, queue_limits);
        let (notifications, wakes) = mpsc::channel();
        let wake = WakeSignal::new(move || {
            let _ = notifications.send(thread::current().id());
        });
        let mut service = AudioReclaimService::new(
            ReclaimLimits {
                max_generations: 1,
                queued_work: 1,
            },
            wake.clone(),
        )
        .unwrap();
        let mut mixer = KiraMixer::new(&format(), limits(1), &service).unwrap();
        let mut fill = mixer.take_fill().unwrap();
        let (_, a_ticket) = mixer.reserve_decode_budget(&pool, 1024, 1);
        let owner = AudioOwner::new(Scope::Process, Arc::new(()));
        let state = ActivityState::new(crate::MediaId(1), crate::MediaEpoch(7), owner.clone());
        let receipt = AudioActivityRetirement(state.clone());
        let descriptor = DescriptorRelease(state.clone());
        let mut a = DecodedSound::new(48_000, vec![[0.1, -0.2]; 64], a_ticket.unwrap()).unwrap();
        a.track_retirement(PhysicalRelease(state));
        let a_handle = mixer.play(a).unwrap();
        wait(|| service.status().completed_accepted_data == 1);
        owner.0.closed.store(true, Ordering::Release);
        drop(descriptor);
        assert!(receipt.descriptor_released());
        assert!(!receipt.resources_released());
        assert!(!owner.is_retired());
        wake.acknowledge();
        while wakes.try_recv().is_ok() {}
        let mut pcm = [0.0; 1024];
        fill(&mut pcm);
        assert!(a_handle.is_stopped());
        assert!(
            pcm.as_chunks::<2>()
                .0
                .iter()
                .any(|f| (f[0] - 0.1).abs() < 1e-6 && (f[1] + 0.2).abs() < 1e-6)
        );
        assert_eq!(mixer.reclaim_unused(1).transferred_sounds, 0); // stopped still arena-owned
        assert!(pool.reserve(1024).is_err());
        fill(&mut pcm); // actual retirement publication, then atomic wake flag
        assert_ne!(
            wakes.recv_timeout(Duration::from_secs(1)).unwrap(),
            thread::current().id()
        );
        wake.acknowledge(); // Existing host contract: before draining sources.
        let (entered, release) = gate_worker(&service, JobKind::Sound);
        let (progress, b_ticket) = mixer.reserve_decode_budget(&pool, 1024, 1);
        assert_eq!(progress.transferred_sounds, 1);
        assert!(progress.needs_maintenance);
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(b_ticket.is_err()); // transfer is NOT destruction or budget return
        assert_eq!(pool.usage().tickets, 1);
        assert!(!receipt.resources_released());
        assert!(!owner.is_retired());
        release.wait();
        wait(|| service.status().completed_sounds == 1);
        assert!(receipt.is_retired());
        assert!(owner.is_retired());
        assert_eq!(pool.usage().tickets, 0);
        wakes.recv_timeout(Duration::from_secs(1)).unwrap();
        wake.acknowledge();
        let (progress, b_ticket) = mixer.reserve_decode_budget(&pool, 1024, 1);
        assert_eq!(progress.transferred_sounds, 1); // Empty queue endpoints also retire off-thread.
        assert!(progress.needs_maintenance);
        wait(|| service.status().completed_queue_work == 1);
        assert_eq!(mixer.reclaim_unused(1).transferred_sounds, 0);
        let b = mixer
            .play(DecodedSound::new(48_000, vec![[0.3, -0.4]; 64], b_ticket.unwrap()).unwrap())
            .unwrap();
        wait(|| service.status().completed_accepted_data == 2);
        fill(&mut pcm);
        assert!(b.is_stopped());
        assert!(
            pcm.as_chunks::<2>()
                .0
                .iter()
                .any(|f| (f[0] - 0.3).abs() < 1e-6 && (f[1] + 0.4).abs() < 1e-6)
        );
        fill(&mut pcm);
        assert_eq!(mixer.reclaim_unused(1).transferred_sounds, 1);
        wait(|| service.status().completed_sounds == 2);
        assert_eq!(pool.usage().tickets, 0);
        drop(fill);
        drop(mixer);
        assert_eq!(service.drain_and_join().unwrap().live_generations, 0);
        println!(
            "{label}: same mixer/fill A->B; transfer=1 remains Busy while worker gated; physical release then completion wake permits B; no early ticket return"
        );
    }
}

#[test]
fn full_normal_queue_preserves_unused_and_reserved_teardowns_and_bounds_rebuild() {
    let mut service = AudioReclaimService::new(
        ReclaimLimits {
            max_generations: 1,
            queued_work: 1,
        },
        WakeSignal::new(|| {}),
    )
    .unwrap();
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 4,
            max_bytes: 4096,
        },
    );
    let mut mixer = KiraMixer::new(&format(), limits(3), &service).unwrap();
    let mut fill = mixer.take_fill().unwrap();
    for n in 1..=3 {
        mixer.play(sound(&pool, n as f32 / 10.0)).unwrap();
        wait(|| service.status().completed_accepted_data == n);
    }
    fill(&mut [0.0; 1024]);
    fill(&mut [0.0; 1024]);
    assert_eq!(mixer.reclaim_unused(0).transferred_sounds, 0);
    let (entered, release) = gate_worker(&service, JobKind::Sound);
    assert_eq!(mixer.reclaim_unused(1).transferred_sounds, 1);
    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(mixer.reclaim_unused(1).transferred_sounds, 1); // one queued + one worker
    let full = mixer.reclaim_unused(usize::MAX);
    assert_eq!(full.transferred_sounds, 0); // third Sound stays in original Kira unused queue
    assert!(full.needs_maintenance);
    assert_eq!(pool.usage().tickets, 3);
    assert_eq!(service.status().outstanding_work, 2); // exact R+1 bound
    let refused = sound(&pool, 0.9);
    let original_pcm = refused.data.frames.static_frames().unwrap().as_ptr();
    let rejected = mixer.play(refused).err().unwrap();
    assert_eq!(
        rejected
            .request
            .data
            .frames
            .static_frames()
            .unwrap()
            .as_ptr(),
        original_pcm
    );
    drop(rejected); // Refused request still belongs to caller.
    assert_eq!(
        KiraMixer::new(&format(), limits(1), &service)
            .err()
            .unwrap()
            .kind,
        AudioErrorKind::Busy
    );
    assert_eq!(
        service.drain_and_join().unwrap_err().kind,
        AudioErrorKind::Busy
    );
    drop(fill); // dedicated credit remains available even with normal queue full
    drop(mixer);
    assert_eq!(service.status().unsubmitted_teardowns, 0);
    assert_eq!(pool.usage().tickets, 3);
    assert_eq!(
        KiraMixer::new(&format(), limits(1), &service)
            .err()
            .unwrap()
            .kind,
        AudioErrorKind::Busy
    );
    release.wait();
    wait(|| service.status().live_generations == 0);
    assert_eq!(pool.usage().tickets, 0); // third Sound released through native teardown
    let rebuilt = KiraMixer::new(&format(), limits(1), &service).unwrap();
    drop(rebuilt); // untaken fill: worker Manager Drop submits its Renderer credit
    let final_status = service.drain_and_join().unwrap();
    assert_eq!(final_status.live_generations, 0);
    assert_eq!(final_status.outstanding_work, 0);
    assert_eq!(final_status.completed_teardowns, 4);
    println!(
        "capacity=1: queued+inflight=2; full destination retains third native Sound and rejects play unchanged; reserved Renderer/Manager moves still succeed; old generation blocks rebuild until real completion; untaken fill drains at final host exit"
    );
}

#[test]
fn accepted_original_cannot_become_last_pcm_reference_on_preempted_control() {
    let mut service = AudioReclaimService::new(
        ReclaimLimits {
            max_generations: 1,
            queued_work: 1,
        },
        WakeSignal::new(|| {}),
    )
    .unwrap();
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 1,
            max_bytes: 1024,
        },
    );
    let mut mixer = KiraMixer::new(&format(), limits(1), &service).unwrap();
    let mut fill = mixer.take_fill().unwrap();
    let (entered_send, entered) = mpsc::channel();
    let release = Arc::new(Barrier::new(2));
    let control_release = release.clone();
    *service.shared.after_accept.lock().unwrap() = Some(Arc::new(move || {
        entered_send.send(()).unwrap();
        control_release.wait();
    }));
    let owner = AudioOwner::new(Scope::Process, Arc::new(()));
    let evidence = ActivityState::new(crate::MediaId(1), crate::MediaEpoch(7), owner.clone());
    let receipt = AudioActivityRetirement(evidence.clone());
    let descriptor = DescriptorRelease(evidence.clone());
    let mut original = sound(&pool, 0.2);
    original.track_retirement(PhysicalRelease(evidence));
    owner.0.closed.store(true, Ordering::Release);
    let control = thread::spawn(move || {
        mixer.play(original).unwrap();
        mixer
    });
    entered.recv_timeout(Duration::from_secs(1)).unwrap();
    let mut pcm = [0.0; 1024];
    fill(&mut pcm); // real Sound installed and finished while accepting control is paused
    assert!(pcm.iter().any(|v| (*v - 0.2f32).abs() < 1e-6));
    drop(fill);
    wait(|| service.status().completed_teardowns == 1); // actual Renderer/Sound already gone
    assert_eq!(pool.usage().tickets, 1); // the original owned request still holds actual PCM
    assert!(!receipt.resources_released());
    assert!(!receipt.is_retired());
    assert!(!owner.is_retired());
    release.wait(); // success MUST move it to already reserved chunk, not drop on this thread
    let mixer = control.join().unwrap();
    wait(|| service.status().completed_accepted_data == 1);
    assert_eq!(pool.usage().tickets, 0);
    assert!(receipt.resources_released());
    assert!(!receipt.descriptor_released());
    assert!(!owner.is_retired());
    drop(descriptor);
    for _ in 0..3 {
        assert!(receipt.is_retired());
        assert!(owner.is_retired());
    }
    drop(mixer);
    service.drain_and_join().unwrap();
    println!(
        "successful play preemption: worker destroys real Renderer first; original accepted DecodedSound keeps physical receipt pending, then moves as last PCM owner to worker; resource evidence follows ticket release, owner evidence also waits for descriptor cleanup"
    );
}

#[test]
fn construction_abort_and_worker_error_keep_reserved_ownership_and_exit() {
    let mut service = AudioReclaimService::new(
        ReclaimLimits {
            max_generations: 1,
            queued_work: 1,
        },
        WakeSignal::new(|| {}),
    )
    .unwrap();
    service
        .shared
        .abort_construction
        .store(true, Ordering::Release);
    assert_eq!(
        KiraMixer::new(&format(), limits(1), &service)
            .err()
            .unwrap()
            .kind,
        AudioErrorKind::DeviceFailure
    );
    wait(|| service.status().completed_teardowns == 2 && service.status().live_generations == 0);
    assert_eq!(service.status().completed_teardowns, 2);
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 1,
            max_bytes: 1024,
        },
    );
    let mut mixer = KiraMixer::new(&format(), limits(1), &service).unwrap();
    let mut fill = mixer.take_fill().unwrap();
    mixer.play(sound(&pool, 0.1)).unwrap();
    wait(|| service.status().completed_accepted_data == 1);
    fill(&mut [0.0; 1024]);
    fill(&mut [0.0; 1024]);
    *service.shared.test_hook.lock().unwrap() = Some(Arc::new(|kind| {
        if kind == JobKind::Sound {
            panic!("injected worker failure; owned job unwinds on worker");
        }
    }));
    assert_eq!(mixer.reclaim_unused(1).transferred_sounds, 1);
    wait(|| service.status().worker_errors == 1);
    assert_eq!(pool.usage().tickets, 0);
    assert_eq!(
        KiraMixer::new(&format(), limits(1), &service)
            .err()
            .unwrap()
            .kind,
        AudioErrorKind::Busy
    );
    drop(fill);
    drop(mixer); // fault never disconnects the guaranteed teardown destination
    assert_eq!(
        service.drain_and_join().unwrap_err().kind,
        AudioErrorKind::DeviceFailure
    );
    assert_eq!(service.status().outstanding_work, 0);
    assert_eq!(service.status().live_generations, 0);
    assert_eq!(service.status().completed_teardowns, 4);
    println!(
        "construction abort moves both native owners; caught worker failure unwinds real Sound on worker, refuses new generations, drains reserved teardown and reports error at host join"
    );
}

#[test]
fn service_drop_detaches_and_retains_consumers_until_late_mixer_teardown() {
    let service = AudioReclaimService::new(
        ReclaimLimits {
            max_generations: 1,
            queued_work: 1,
        },
        WakeSignal::new(|| {}),
    )
    .unwrap();
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 1,
            max_bytes: 1024,
        },
    );
    let mut mixer = KiraMixer::new(&format(), limits(1), &service).unwrap();
    let owner = AudioOwner::new(Scope::Process, Arc::new(()));
    let state = ActivityState::new(crate::MediaId(1), crate::MediaEpoch(7), owner.clone());
    let receipt = AudioActivityRetirement(state.clone());
    let mut sound = sound(&pool, 0.1);
    sound.track_retirement(PhysicalRelease(state.clone()));
    mixer.play(sound).unwrap();
    owner.0.closed.store(true, Ordering::Release);
    drop(DescriptorRelease(state));
    drop(service); // no join and no consumer destruction on this control thread
    assert_eq!(pool.usage().tickets, 1);
    assert!(!receipt.resources_released());
    assert!(!owner.is_retired());
    drop(mixer); // contains untaken fill and pending native Sound
    wait(|| owner.is_retired());
    assert!(receipt.is_retired());
    assert_eq!(pool.usage().tickets, 0);
}

#[cfg(feature = "decode")]
#[test]
fn completed_stream_releases_window_before_queued_infinite_successor_and_status_survives() {
    use std::sync::atomic::AtomicBool;
    fn stream(pool: &BudgetPool) -> DecodedSound {
        let bytes = include_bytes!("../../../../examples/assets/chime.wav").to_vec();
        let length = bytes.len() as u64;
        DecodedSound::prepare(
            std::io::Cursor::new(bytes),
            length,
            pool.clone(),
            DecodeLimits {
                static_threshold: 0,
                window_frames: 256,
            },
            Arc::new(|| {}),
            &AtomicBool::new(false),
            &crate::decode::SoftwareAudioDecoderFactory,
        )
        .unwrap()
    }
    let a_pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 4,
            max_bytes: 1 << 20,
        },
    );
    let b_pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 2,
            max_bytes: 4096,
        },
    );
    let mut service =
        AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
    let mut mixer = KiraMixer::new(&format(), limits(3), &service).unwrap();
    let mut fill = mixer.take_fill().unwrap();
    let a = stream(&a_pool)
        .with_playback(Duration::ZERO, Some(Duration::from_millis(10)), None)
        .unwrap();
    let reel_audio::sound::static_sound::FrameStorage::Stream(source) = &a.data.frames else {
        panic!("expected stream")
    };
    let weak = Arc::downgrade(source);
    let status = source.status().unwrap();
    let mut playback = mixer.play(a).unwrap();
    mixer.announce(&playback, 1).unwrap();
    mixer
        .queue(&mut playback, 1, sound(&b_pool, 0.3).looping())
        .unwrap();
    wait(|| {
        fill(&mut [0.0f32; 1024]);
        mixer.reclaim_unused(64);
        playback.queue_cursor().0 == 1 && a_pool.usage().bytes == 0
    });
    assert!(
        weak.upgrade().is_none(),
        "consumed source owns no decoder/window despite retained queue descriptor"
    );
    assert!(b_pool.usage().bytes > 0);
    assert!(!playback.is_stopped());
    assert!(!status.is_empty());
    playback.stop();
    fill(&mut [0.0f32; 1024]);
    drop(playback);
    drop(fill);
    drop(mixer);
    service.drain_and_join().unwrap();
    assert_eq!(a_pool.usage().bytes + b_pool.usage().bytes, 0);

    // A native stop can retire PCM before the control poll; retain only facts.
    let mut service =
        AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
    let mixer = KiraMixer::new(&format(), limits(1), &service).unwrap();
    let mut activities = crate::AudioActivityService::new(
        mixer,
        QueueLimits {
            max_requests: 1,
            max_bytes: 1 << 20,
        },
        crate::MediaEpoch(1),
        WakeSignal::new(|| {}),
    )
    .ok()
    .unwrap();
    let mut fill = activities.take_fill().unwrap();
    let a = stream(&a_pool);
    let reel_audio::sound::static_sound::FrameStorage::Stream(source) = &a.data.frames else {
        unreachable!()
    };
    let weak = Arc::downgrade(source);
    let status = source.status().unwrap();
    let owner = activities.register_owner(Scope::Process).unwrap();
    let id = activities
        .play(crate::AudioActivityRequest {
            epoch: crate::MediaEpoch(1),
            owner,
            resource: reel_core::identity::ResourceVersion {
                key: reel_core::identity::AssetKey("stream".into()),
                version: reel_core::identity::ContentVersion([1; 32]),
            },
            group: AudioGroup::Music,
            sound: a,
        })
        .unwrap();
    status.fail("late decode error".into());
    // Deliberately do not inspect activity state until the callback and reclaimer finish.
    wait(|| {
        fill(&mut [0.0f32; 1024]);
        activities.maintain(0, 64);
        a_pool.usage().bytes == 0
    });
    assert!(weak.upgrade().is_none());
    assert_eq!(
        activities.snapshot(id).unwrap().terminal,
        Some(crate::AudioActivityTerminal::Failed(
            AudioErrorKind::DecodeFailure
        ))
    );
    activities.maintain(64, 64);
    assert_eq!(
        activities.snapshot(id).unwrap().terminal,
        Some(crate::AudioActivityTerminal::Failed(
            AudioErrorKind::DecodeFailure
        ))
    );
    drop(activities);
    drop(fill);
    service.drain_and_join().unwrap();
}
