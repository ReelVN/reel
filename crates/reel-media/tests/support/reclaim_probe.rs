//! Separate control ownership-transfer cost from actual worker PCM destruction.
//! Setup/decoding and harness completion waits are outside the timed main work.
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    clock::MonoTime,
    wake::WakeSignal,
};
use reel_media::*;
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
mod allocation_observer;
use allocation_observer::*;
const SAMPLES: usize = 16;
const MEASURED_FRAMES: usize = 8_388_608;
fn wait(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "bounded probe completion wait");
        std::thread::park_timeout(Duration::from_micros(100));
    }
}
fn enable_background(frames: usize, pool: &BudgetPool) {
    BACKGROUND_ENABLED.store(false, Ordering::Release);
    *OBSERVED_POOL.lock().unwrap() = Some(pool.clone());
    EXPECTED_CHARGE.store(frames * 16, Ordering::Release);
    PHYSICAL_FREE_ORDER_CHECKS.store(0, Ordering::Release);
    WRONG_PHYSICAL_FREE_ORDER.store(0, Ordering::Release);
    TARGET_LAYOUT.store(frames * 8 + 2 * size_of::<usize>(), Ordering::Release);
    WORKER_PCM_FREES.store(0, Ordering::Release);
    WRONG_THREAD_PCM_FREES.store(0, Ordering::Release);
    WORKER_PCM_FREE_NS.store(0, Ordering::Release);
    WORKER_LARGEST_FREE.store(0, Ordering::Release);
    BACKGROUND_ENABLED.store(true, Ordering::Release);
}
fn measured(action: impl FnOnce()) -> (u64, Counts) {
    COUNTS.set(Counts::default());
    ENABLED.set(true);
    let begin = Instant::now();
    action();
    let ns = begin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
    ENABLED.set(false);
    (ns, COUNTS.get())
}
fn distribution(mut samples: [u64; SAMPLES]) -> (f64, f64) {
    samples.sort_unstable();
    (
        samples[SAMPLES / 2] as f64 / 1000.0,
        samples[SAMPLES - 1] as f64 / 1000.0,
    )
}
fn physical_free(pool: &BudgetPool) -> (u64, usize) {
    assert_eq!(pool.usage().tickets, 0);
    assert_eq!(pool.usage().bytes, 0);
    assert_eq!(WORKER_PCM_FREES.load(Ordering::Acquire), 1);
    assert_eq!(WRONG_THREAD_PCM_FREES.load(Ordering::Acquire), 0);
    assert_eq!(PHYSICAL_FREE_ORDER_CHECKS.load(Ordering::Acquire), 2);
    assert_eq!(WRONG_PHYSICAL_FREE_ORDER.load(Ordering::Acquire), 0);
    let ns = WORKER_PCM_FREE_NS.load(Ordering::Acquire) as u64;
    let bytes = WORKER_LARGEST_FREE.load(Ordering::Acquire);
    BACKGROUND_ENABLED.store(false, Ordering::Release);
    *OBSERVED_POOL.lock().unwrap() = None;
    (ns, bytes)
}
fn main() {
    let mut service = AudioReclaimService::new(
        ReclaimLimits {
            max_generations: 1,
            queued_work: 1,
        },
        WakeSignal::new(|| {}),
    )
    .unwrap();
    let format = AudioFormat::new(48_000, SampleFormat::F32, ChannelLayout::Stereo, 512).unwrap();
    let mut mixer = KiraMixer::new(
        &format,
        MixerLimits {
            max_sounds: 1,
            internal_buffer_frames: 128,
        },
        &service,
    )
    .unwrap();
    let mut fill = mixer.take_fill().unwrap();
    let mut block = [0.0f32; 1024];
    for frames in [64, 48_000, MEASURED_FRAMES] {
        let bytes = frames as u64 * 16;
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 1,
                max_bytes: bytes,
            },
        );
        let mut ticks = [0u64; SAMPLES];
        let mut plays = ticks;
        let mut worker_free = ticks;
        let mut worker_jobs = ticks;
        let mut largest_worker_free = 0;
        for sample in 0..SAMPLES {
            let (_, ticket) = mixer.reserve_decode_budget(&pool, bytes, 1);
            let owned =
                DecodedSound::new(48_000, vec![[0.1, -0.2]; frames], ticket.unwrap()).unwrap();
            enable_background(frames, &pool);
            let accepted_before = service.status().completed_accepted_data;
            let mut handle = None;
            let (play_ns, _) = measured(|| {
                handle = Some(mixer.play(owned).unwrap());
            });
            plays[sample] = play_ns;
            wait(|| service.status().completed_accepted_data == accepted_before + 1);
            fill(&mut block);
            assert!(
                block
                    .chunks_exact(2)
                    .any(|f| (f[0] - 0.1).abs() < 1e-6 && (f[1] + 0.2).abs() < 1e-6)
            );
            handle.as_mut().unwrap().stop();
            fill(&mut block);
            fill(&mut block);
            assert_eq!(pool.usage().tickets, 1);
            let completed_before = service.status().completed_sounds;
            let (tick_ns, allocations) = measured(|| {
                let progress = mixer.reclaim_unused(1);
                assert_eq!(progress.transferred_sounds, 1);
                assert!(progress.needs_maintenance);
            });
            assert_eq!(
                (
                    allocations.allocations,
                    allocations.reallocations,
                    allocations.deallocations
                ),
                (0, 0, 0)
            );
            ticks[sample] = tick_ns;
            wait(|| service.status().completed_sounds == completed_before + 1);
            worker_jobs[sample] = service.status().last_job_ns as u64;
            let (free_ns, size) = physical_free(&pool);
            worker_free[sample] = free_ns;
            largest_worker_free = largest_worker_free.max(size);
        }
        let (tick_median, tick_max) = distribution(ticks);
        let (play_median, play_max) = distribution(plays);
        let (job_median, job_max) = distribution(worker_jobs);
        let (free_median, free_max) = distribution(worker_free);
        println!(
            "background_retirement real_kira_frames={frames} actual_pcm_bytes={} ticket_peak_bytes={} samples={SAMPLES} capacity=1",
            frames * 8,
            pool.usage().peak_bytes
        );
        println!(
            "control_transfer median_us={tick_median:.3} max_us={tick_max:.3} alloc=0 realloc=0 dealloc=0; accepted_original_move play_median_us={play_median:.3} play_max_us={play_max:.3}"
        );
        println!(
            "worker_full_job median_us={job_median:.3} max_us={job_max:.3}; actual_System_PCM_free median_us={free_median:.3} max_us={free_max:.3} largest_worker_free_bytes={largest_worker_free} PCM_frees_on_control=0; ticket_held_before_and_after_System_dealloc=true order_checks={} tickets_return_after_physical_free=true",
            SAMPLES * 2
        );
    }
    drop(fill);
    drop(mixer);
    wait(|| service.status().live_generations == 0);
    // A live 64MiB Sound at stop, rather than only retired garbage.
    let frames = MEASURED_FRAMES;
    let bytes = frames as u64 * 16;
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 1,
            max_bytes: bytes,
        },
    );
    let mut stops = [0u64; SAMPLES];
    let mut managers = stops;
    let mut worker_free = stops;
    let mut largest_control_free = 0;
    for sample in 0..SAMPLES {
        let mut output = OfflineOutput::new(
            AudioDeviceConfig {
                sample_rate_hz: 48_000,
                sample_format: SampleFormat::F32,
                layout: ChannelLayout::Stereo,
            },
            WakeSignal::new(|| {}),
        )
        .unwrap();
        let format = output.configure(AudioRequest::default()).unwrap();
        let mut mixer = KiraMixer::new(
            &format,
            MixerLimits {
                max_sounds: 1,
                internal_buffer_frames: 128,
            },
            &service,
        )
        .unwrap();
        let accepted_before = service.status().completed_accepted_data;
        mixer
            .play(
                DecodedSound::new(
                    48_000,
                    vec![[0.1, -0.2]; frames],
                    pool.reserve(bytes).unwrap(),
                )
                .unwrap()
                .looping(),
            )
            .unwrap();
        wait(|| service.status().completed_accepted_data == accepted_before + 1);
        assert!(output.start(mixer.take_fill().unwrap()).is_ok());
        output.render(
            &mut block,
            CallbackTiming {
                now: MonoTime(0),
                latency_frames: None,
                uncertainty_frames: None,
            },
        );
        assert!(block.iter().any(|f| (*f - 0.1f32).abs() < 1e-6));
        assert_eq!(pool.usage().tickets, 1);
        enable_background(frames, &pool);
        let (stop_ns, counts) = measured(|| output.stop().unwrap());
        stops[sample] = stop_ns;
        largest_control_free = largest_control_free.max(counts.largest_free);
        let (manager_ns, counts) = measured(|| drop(mixer));
        managers[sample] = manager_ns;
        largest_control_free = largest_control_free.max(counts.largest_free);
        assert!(largest_control_free < frames * 8); // only small shell/scratch disposal on caller
        assert_eq!(output.status().state, AudioState::Stopped);
        block.fill(42.0);
        output.render(
            &mut block,
            CallbackTiming {
                now: MonoTime(1),
                latency_frames: None,
                uncertainty_frames: None,
            },
        );
        assert!(block.iter().all(|f| *f == 0.0));
        wait(|| service.status().live_generations == 0);
        let (free_ns, _) = physical_free(&pool);
        worker_free[sample] = free_ns;
    }
    let (stop_median, stop_max) = distribution(stops);
    let (manager_median, manager_max) = distribution(managers);
    let (free_median, free_max) = distribution(worker_free);
    let final_status = service.drain_and_join().unwrap();
    println!(
        "live_64MiB_stop samples={SAMPLES} offline_stop_median_us={stop_median:.3} offline_stop_max_us={stop_max:.3} manager_move_median_us={manager_median:.3} manager_move_max_us={manager_max:.3} largest_control_free_bytes={largest_control_free}; actual_worker_PCM_free_median_us={free_median:.3} max_us={free_max:.3}"
    );
    println!(
        "final_host_drain generations={} outstanding={} control_owners={} worker_errors={}; native_pause_and_inflight_fence_have_no_0.5ms_promise=true",
        final_status.live_generations,
        final_status.outstanding_work,
        final_status.control_owners,
        final_status.worker_errors
    );
}
