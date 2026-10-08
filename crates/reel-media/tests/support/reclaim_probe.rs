//! Measures actual control-side destruction of a real Kira StaticSound's PCM.
//! Resource-count quotas cannot preempt an individual destructor.
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    wake::WakeSignal,
};
use reel_media::*;
use std::time::Instant;
mod allocation_observer;
use allocation_observer::{COUNTS, Counts, ENABLED};
fn main() {
    const SAMPLES: usize = 16;
    let format = AudioFormat::new(48_000, SampleFormat::F32, ChannelLayout::Stereo, 512).unwrap();
    let mut mixer = KiraMixer::new(
        &format,
        MixerLimits {
            max_sounds: 1,
            internal_buffer_frames: 128,
        },
        WakeSignal::new(|| {}),
    )
    .unwrap();
    let mut fill = mixer.take_fill().unwrap();
    let mut block = [0.0f32; 512 * 2];
    for frames in [64, 48_000, MAX_DECODED_FRAMES] {
        let bytes = frames as u64 * 16; // input + actual converted Frame overlap.
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 1,
                max_bytes: bytes,
            },
        );
        let mut costs = [0u64; SAMPLES];
        let mut max_free = 0;
        for cost in &mut costs {
            let (progress, ticket) = mixer.reserve_decode_budget(&pool, bytes, 1);
            assert_eq!(progress.reclaimed_sounds, 0);
            let pcm = vec![[0.1, -0.2]; frames]; // initialized/touched, not unbacked capacity.
            let mut handle = mixer
                .play(DecodedSound::new(48_000, pcm, ticket.unwrap()).unwrap())
                .unwrap();
            fill(&mut block);
            assert!(
                block
                    .chunks_exact(2)
                    .any(|f| (f[0] - 0.1).abs() < 1e-6 && (f[1] + 0.2).abs() < 1e-6)
            );
            handle.stop(); // short processing can retire a maximum-size resident clip.
            fill(&mut block);
            assert!(handle.is_stopped());
            fill(&mut block); // actual Kira retirement, not a substitute mixer.
            assert_eq!(pool.usage().tickets, 1);
            COUNTS.set(Counts::default());
            ENABLED.set(true);
            let begin = Instant::now();
            let progress = mixer.reclaim_unused(1);
            *cost = begin.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
            ENABLED.set(false);
            let frees = COUNTS.get();
            // The large Frame Arc allocation physically reached System::dealloc;
            // no Weak reference keeps that allocation resident during measurement.
            assert!(frees.largest_free >= frames * 8);
            max_free = max_free.max(frees.largest_free);
            assert_eq!(progress.reclaimed_sounds, 1);
            assert!(progress.needs_maintenance);
            assert_eq!(pool.usage().tickets, 0);
            assert_eq!(pool.usage().bytes, 0);
        }
        costs.sort_unstable();
        println!(
            "control_reclaim real_kira_frames={frames} actual_pcm_bytes={} ticket_peak_bytes={} samples={SAMPLES} median_us={:.3} max_us={:.3} quota=1 largest_system_free_bytes={max_free}; single_destructor_indivisible=true",
            frames as u64 * 8,
            pool.usage().peak_bytes,
            costs[SAMPLES / 2] as f64 / 1000.0,
            costs[SAMPLES - 1] as f64 / 1000.0
        );
    }
}
