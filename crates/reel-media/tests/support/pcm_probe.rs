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
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    time::Instant,
};
#[derive(Clone, Copy, Default, Debug)]
struct Counts {
    allocations: usize,
    deallocations: usize,
    reallocations: usize,
}
thread_local! { static ENABLED: Cell<bool> = const { Cell::new(false) }; static COUNTS: Cell<Counts> = const { Cell::new(Counts { allocations: 0, deallocations: 0, reallocations: 0 }) }; }
fn count(update: impl FnOnce(&mut Counts)) {
    if ENABLED.try_with(Cell::get).unwrap_or(false) {
        let _ = COUNTS.try_with(|cell| {
            let mut counts = cell.get();
            update(&mut counts);
            cell.set(counts);
        });
    }
}
struct Observer;
#[global_allocator]
static ALLOCATOR: Observer = Observer;
// SAFETY: Forward every original valid pointer/Layout to System exactly once.
unsafe impl GlobalAlloc for Observer {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(|c| c.allocations += 1);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(|c| c.allocations += 1);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count(|c| c.deallocations += 1);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count(|c| c.reallocations += 1);
        unsafe { System.realloc(ptr, layout, size) }
    }
}
fn decoded(pool: &BudgetPool, frame: [f32; 2], frames: usize) -> DecodedSound {
    let pcm = vec![frame; frames];
    let ticket = pool
        .reserve((pcm.capacity() + pcm.len()) as u64 * 8)
        .unwrap();
    DecodedSound::new(48_000, pcm, ticket).unwrap()
}
fn main() {
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
    let mut mixer = KiraMixer::new(&format, MixerLimits::default()).unwrap();
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
    assert_eq!(pool.usage().bytes, 0);
}
