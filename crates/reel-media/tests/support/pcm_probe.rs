//! Standalone allocator observer, like reel-core's existing deallocation probe.
//! Production crates retain unsafe_code=forbid. Unsafe operations below only
//! forward GlobalAlloc parameters to System; PCM code never dereferences pointers.
#![deny(unsafe_op_in_unsafe_fn)]
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    clock::MonoTime,
    wake::WakeSignal,
};
use reel_media::*;
use std::time::Instant;
mod allocation_observer;
use allocation_observer::{COUNTS, ENABLED};
fn decoded(pool: &BudgetPool, frame: [f32; 2], frames: usize) -> DecodedSound {
    let pcm = vec![frame; frames];
    let ticket = pool
        .reserve((pcm.capacity() + pcm.len()) as u64 * 8)
        .unwrap();
    DecodedSound::new(48_000, pcm, ticket).unwrap()
}
fn wait_for(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + std::time::Duration::from_secs(3);
    while !done() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}
fn full_unused_queue_remains_allocation_free() {
    let mut service =
        AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 2,
            max_bytes: 2048,
        },
    );
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
            internal_buffer_frames: 64,
        },
        &service,
    )
    .unwrap();
    let _a = mixer.play(decoded(&pool, [0.1, -0.2], 64)).unwrap();
    assert!(output.start(mixer.take_fill().unwrap()).is_ok());
    let mut data = [0.0f32; 512 * 2];
    let timing = CallbackTiming {
        now: MonoTime(0),
        latency_frames: Some(0),
        uncertainty_frames: None,
    };
    for _ in 0..2 {
        ENABLED.set(true);
        output.render(&mut data, timing);
        ENABLED.set(false);
    }
    let b = mixer.play(decoded(&pool, [0.3, -0.4], 64)).unwrap(); // NO implicit cleanup.
    for _ in 0..16 {
        ENABLED.set(true);
        output.render(&mut data, timing);
        ENABLED.set(false);
    }
    assert!(b.is_stopped());
    assert_eq!(pool.usage().tickets, 2); // A unused, B retained in arena.
    assert_eq!(mixer.reclaim_unused(1).transferred_sounds, 1);
    ENABLED.set(true);
    output.render(&mut data, timing);
    ENABLED.set(false);
    assert_eq!(mixer.reclaim_unused(1).transferred_sounds, 1);
    wait_for(|| { mixer.reclaim_unused(1); pool.usage().tickets == 0 });
    output.stop().unwrap();
    drop(mixer);
    service.drain_and_join().unwrap();
    let counts = COUNTS.get();
    assert_eq!(
        (
            counts.allocations,
            counts.reallocations,
            counts.deallocations
        ),
        (0, 0, 0)
    );
    println!(
        "full_unused_queue native_capacity=1 A_retired/B_stopped_retained PCM_calls=19 alloc=0 realloc=0 dealloc=0 control_quota=1 physical_release=worker"
    );
}
fn group_envelopes_are_allocation_free() {
    use std::time::Duration;
    let mut service = AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
    let pool = BudgetPool::new(BudgetCategory::CpuOutput, QueueLimits { max_requests: 4, max_bytes: 1 << 20 });
    let mut output = OfflineOutput::new(AudioDeviceConfig { sample_rate_hz: 48_000, sample_format: SampleFormat::F32, layout: ChannelLayout::Stereo }, WakeSignal::new(|| {})).unwrap();
    let format = output.configure(AudioRequest::default()).unwrap();
    let mut mixer = KiraMixer::new(&format, MixerLimits::default(), &service).unwrap();
    let mut voices = AudioGroup::ALL.map(|group| mixer.play_in(group, decoded(&pool, [0.1, 0.1], 256).looping()).unwrap());
    assert!(output.start(mixer.take_fill().unwrap()).is_ok());
    let mut pcm = [0.0_f32; 256];
    for block in 0..1000 {
        ENABLED.set(true);
        let db = if block % 2 == 0 { -12.0 } else { 0.0 };
        for (group, voice) in AudioGroup::ALL.into_iter().zip(&mut voices) {
            mixer.set_group_volume_db(Some(group), db, Duration::from_millis(30)).unwrap();
            voice.set_pan(if block % 2 == 0 { -1.0 } else { 1.0 }, Duration::from_millis(30)).unwrap();
            voice.set_volume_db(db, Duration::from_millis(30)).unwrap();
        }
        output.render(&mut pcm, CallbackTiming { now: MonoTime(0), latency_frames: None, uncertainty_frames: None });
        ENABLED.set(false);
        assert!(pcm.iter().all(|v| v.is_finite()));
    }
    for voice in &mut voices { voice.fade_out(Duration::from_millis(20)); }
    for _ in 0..20 {
        ENABLED.set(true);
        output.render(&mut pcm, CallbackTiming { now: MonoTime(0), latency_frames: None, uncertainty_frames: None });
        ENABLED.set(false);
    }
    assert!(voices.iter().all(|voice| voice.is_stopped()));
    output.stop().unwrap();
    drop(mixer);
    service.drain_and_join().unwrap();
    assert_eq!(pool.usage().tickets, 0);
    let counts = COUNTS.get();
    assert_eq!((counts.allocations, counts.reallocations, counts.deallocations), (0, 0, 0));
    println!("group_envelopes groups=4 coalesced_changes=12000 PCM_calls=1020 alloc=0 realloc=0 dealloc=0");
}
fn queued_segments_have_no_callback_allocation() {
    let mut service = AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
    let pool = BudgetPool::new(BudgetCategory::CpuOutput, QueueLimits { max_requests: 4, max_bytes: 8192 });
    let mut output = OfflineOutput::new(AudioDeviceConfig { sample_rate_hz: 48_000, sample_format: SampleFormat::F32, layout: ChannelLayout::Stereo }, WakeSignal::new(|| {})).unwrap();
    let format = output.configure(AudioRequest::default()).unwrap();
    let mut mixer = KiraMixer::new(&format, MixerLimits { max_sounds: 4, internal_buffer_frames: 64 }, &service).unwrap();
    let segment = |frame| decoded(&pool, frame, 128)
        .with_playback(std::time::Duration::ZERO, Some(std::time::Duration::from_secs_f64(64. / 48_000.)), None).unwrap();
    let mut sound = mixer.play(segment([0.1, -0.1])).unwrap();
    mixer.announce(&sound, 3).unwrap();
    for index in 1..=3 {
        let amplitude = (index + 1) as f32 / 10.;
        mixer.queue(&mut sound, index, segment([amplitude, -amplitude])).unwrap();
    }
    assert!(output.start(mixer.take_fill().unwrap()).is_ok());
    let mut pcm = [0.0_f32; 1024];
    let timing = CallbackTiming { now: MonoTime(0), latency_frames: None, uncertainty_frames: None };
    COUNTS.set(Default::default());
    ENABLED.set(true);
    output.render(&mut pcm, timing);
    ENABLED.set(false);
    for (index, frame) in pcm[..512].chunks_exact(2).enumerate() {
        let amplitude = (index / 64 + 1) as f32 / 10.;
        assert!((frame[0] - amplitude).abs() < 1e-6 && (frame[1] + amplitude).abs() < 1e-6, "gap or order error at frame {index}");
    }
    ENABLED.set(true);
    output.render(&mut pcm, timing);
    ENABLED.set(false);
    let counts = COUNTS.get();
    assert_eq!((counts.allocations, counts.reallocations, counts.deallocations), (0, 0, 0));
    wait_for(|| { mixer.reclaim_unused(4); pool.usage().bytes == 0 });
    assert!(sound.is_stopped());
    output.stop().unwrap();
    drop(mixer);
    service.drain_and_join().unwrap();
    println!("prepared_queue segments=4 exclusive_end=64/128 boundary_gaps=0 callback_alloc=0 realloc=0 dealloc=0 final_pcm_bytes={}", pool.usage().bytes);
}

fn streaming_callback_is_allocation_free() {
    use std::{sync::{Arc, atomic::AtomicBool}, time::Duration};
    let bytes = std::fs::read("examples/assets/chime.wav").unwrap();
    let length = bytes.len() as u64;
    let pool = BudgetPool::new(BudgetCategory::CpuOutput, QueueLimits { max_requests: 4, max_bytes: 1 << 20 });
    let sound = DecodedSound::prepare(std::io::Cursor::new(bytes), length, pool.clone(), 0, 1024, Arc::new(|| {}), &AtomicBool::new(false), &reel_media::decode::PlatformAudioDecoderFactory).unwrap().looping();
    let mut service = AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
    let mut output = OfflineOutput::new(AudioDeviceConfig { sample_rate_hz: 48_000, sample_format: SampleFormat::F32, layout: ChannelLayout::Stereo }, WakeSignal::new(|| {})).unwrap();
    let format = output.configure(AudioRequest::default()).unwrap();
    let mut mixer = KiraMixer::new(&format, MixerLimits::default(), &service).unwrap();
    let mut voice = mixer.play(sound).unwrap();
    assert!(output.start(mixer.take_fill().unwrap()).is_ok());
    let mut pcm = [0.0f32; 256];
    let mut audible = false;
    for block in 0..1024 {
        if block == 512 { voice.seek(Duration::from_millis(20)); }
        ENABLED.set(true);
        output.render(&mut pcm, CallbackTiming { now: MonoTime(0), latency_frames: None, uncertainty_frames: None });
        ENABLED.set(false);
        audible |= pcm.iter().any(|v| *v != 0.);
        std::thread::sleep(Duration::from_micros(100));
    }
    assert!(audible, "stream position={} stopped={} error={:?}", voice.position_seconds(), voice.is_stopped(), voice.error());
    voice.stop();
    ENABLED.set(true);
    output.render(&mut pcm, CallbackTiming { now: MonoTime(0), latency_frames: None, uncertainty_frames: None });
    ENABLED.set(false);
    drop(voice);
    wait_for(|| {
        ENABLED.set(true);
        output.render(&mut pcm, CallbackTiming { now: MonoTime(0), latency_frames: None, uncertainty_frames: None });
        ENABLED.set(false);
        mixer.reclaim_unused(64); pool.usage().bytes == 0
    });
    output.stop().unwrap();
    drop(mixer);
    service.drain_and_join().unwrap();
    let c = COUNTS.get();
    assert_eq!((c.allocations, c.reallocations, c.deallocations), (0, 0, 0));
    println!("streaming PCM_calls=1025 loop/seek/stop alloc=0 realloc=0 dealloc=0 final_pcm=0");
}

fn main() {
    streaming_callback_is_allocation_free();
    queued_segments_have_no_callback_allocation();
    let mut service =
        AudioReclaimService::new(ReclaimLimits::default(), WakeSignal::new(|| {})).unwrap();
    full_unused_queue_remains_allocation_free();
    group_envelopes_are_allocation_free();
    const BLOCKS: usize = 4096;
    const FRAMES: usize = 512;
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 4,
            max_bytes: 4 * 1024 * 1024,
        },
    );
    let mut output = OfflineOutput::new(
        AudioDeviceConfig {
            sample_rate_hz: 48_000,
            sample_format: SampleFormat::I16,
            layout: ChannelLayout::Surround71,
        },
        WakeSignal::new(|| {}),
    )
    .unwrap();
    let format = output
        .configure(AudioRequest {
            allowed_layouts: vec![ChannelLayout::Surround71],
            allowed_sample_formats: vec![SampleFormat::I16],
            max_pull_frames: FRAMES as u32,
            ..AudioRequest::default()
        })
        .unwrap();
    let mut mixer = KiraMixer::new(&format, MixerLimits::default(), &service).unwrap();
    let first = mixer
        .play(decoded(&pool, [0.8, -0.8], 48_000).looping())
        .ok()
        .unwrap();
    let second = mixer
        .play(decoded(&pool, [0.7, -0.7], 48_000).looping())
        .ok()
        .unwrap();
    // A finite sound exercises Kira's deferred retirement too.
    let _finite = mixer.play(decoded(&pool, [0.01, -0.01], 64)).ok().unwrap();
    assert!(output.start(mixer.take_fill().unwrap()).is_ok());
    let mut data = [42i16; FRAMES * 8];
    let mut times = vec![0u64; BLOCKS];
    let begin = Instant::now();
    for (block, elapsed) in times.iter_mut().enumerate() {
        let timing = CallbackTiming {
            now: MonoTime(block as u64 * FRAMES as u64 * 1_000_000_000 / 48_000),
            latency_frames: Some(FRAMES as u32),
            uncertainty_frames: Some(0),
        };
        ENABLED.set(true);
        let start = Instant::now();
        output.render(&mut data, timing);
        *elapsed = start.elapsed().as_nanos() as u64;
        ENABLED.set(false);
        for frame in data.chunks_exact(8).skip(8) {
            assert_eq!(frame, &[i16::MAX, i16::MIN, 0, 0, 0, 0, 0, 0]);
        }
    }
    let wall = begin.elapsed();
    let counts = COUNTS.get();
    assert_eq!(
        (
            counts.allocations,
            counts.deallocations,
            counts.reallocations
        ),
        (0, 0, 0)
    );
    let last = output.clock_observation();
    let suspended_position = first.position_seconds();
    output.suspend().unwrap();
    ENABLED.set(true);
    output.render(
        &mut data,
        CallbackTiming {
            now: MonoTime(99_000_000_000),
            latency_frames: Some(512),
            uncertainty_frames: Some(0),
        },
    );
    ENABLED.set(false);
    assert!(data.iter().all(|v| *v == 0));
    assert_eq!(first.position_seconds(), suspended_position);
    assert_eq!(output.clock_observation(), last);
    output.resume().unwrap();
    output.render(
        &mut data,
        CallbackTiming {
            now: MonoTime(100_000_000_000),
            latency_frames: Some(512),
            uncertainty_frames: Some(0),
        },
    );
    assert!(second.position_seconds() > suspended_position);
    output.stop().unwrap();
    let position = first.position_seconds();
    output.render(
        &mut data,
        CallbackTiming {
            now: MonoTime(101_000_000_000),
            latency_frames: None,
            uncertainty_frames: None,
        },
    );
    assert!(data.iter().all(|v| *v == 0));
    assert_eq!(first.position_seconds(), position);
    times.sort_unstable();
    println!(
        "real_kira blocks={BLOCKS} frames_per_block={FRAMES} device=8ch/48000Hz/i16 mixer=2ch/f32 samples=[32767,-32768,0,0,0,0,0,0]"
    );
    println!(
        "callback alloc={} realloc={} dealloc={} p95_us={:.3} p99_us={:.3} max_us={:.3} block_deadline_us={:.3} elapsed_ms={:.3}",
        counts.allocations,
        counts.reallocations,
        counts.deallocations,
        times[BLOCKS * 95 / 100] as f64 / 1000.0,
        times[BLOCKS * 99 / 100] as f64 / 1000.0,
        times[BLOCKS - 1] as f64 / 1000.0,
        FRAMES as f64 * 1_000_000.0 / 48_000.0,
        wall.as_secs_f64() * 1000.0
    );
    println!(
        "clock epoch={} submitted={} estimated_presented={} latency={:?} uncertainty={:?} precision={:?}",
        last.output_epoch.0,
        last.submitted_frames,
        last.estimated_presented_frames,
        last.latency_frames,
        last.uncertainty_frames,
        last.precision
    );
    println!(
        "lifecycle suspended_position={suspended_position:.9} stopped_position={position:.9} stop_prevents_fill=true; cpu_pcm_budget_peak_bytes={}",
        pool.usage().peak_bytes
    );
    drop(mixer);
    service.drain_and_join().unwrap();
    assert_eq!(pool.usage().bytes, 0);
}
