//! Temporary device probe; retained only on the diagnostic branch.
//! Nested phases overlap. Worker durations are attributed when work completes;
//! their totals can exceed the main thread's elapsed time. Normal builds erase
//! scopes and counters; profiling builds use fixed atomic arrays without IO.

/// Stages recorded by the shared runtime and device backend.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Phase {
    Input,
    Tick,
    Vm,
    UiSync,
    Record,
    Compile,
    ImagePlan,
    Upload,
    Replay,
    GlyphRaster,
    Draw,
    Present,
    Read,
    Decode,
    TargetCreate,
    BufferCreate,
    Poll,
    Load,
    Mount,
    Sampler,
    Composite,
    Screen,
    Clear,
    SourceComposite,
    CoverageComposite,
    EffectComposite,
    AudioMix,
    AudioDecode,
    VideoOpen,
    VideoDecode,
    OffscreenPlan,
    CompositeBatch,
}
pub const NAMES: [&str; 32] = [
    "input",
    "tick",
    "vm",
    "ui_sync",
    "record",
    "compile",
    "image_plan",
    "upload",
    "replay",
    "glyph_raster",
    "draw",
    "present",
    "read",
    "decode",
    "target_create",
    "buffer_create",
    "poll",
    "load",
    "mount",
    "sampler",
    "composite",
    "screen",
    "clear",
    "source_composite",
    "coverage_composite",
    "effect_composite",
    "audio_mix",
    "audio_decode",
    "video_open",
    "video_decode",
    "offscreen_plan",
    "composite_batch",
];

/// Times one phase. Repeated calls contribute their duration and count.
pub struct Scope {
    #[cfg(feature = "profiling")]
    start: std::time::Instant,
    #[cfg(feature = "profiling")]
    phase: Phase,
}
impl Scope {
    #[inline]
    pub fn new(phase: Phase) -> Self {
        #[cfg(not(feature = "profiling"))]
        let _ = phase;
        let scope = Self {
            #[cfg(feature = "profiling")]
            start: std::time::Instant::now(),
            #[cfg(feature = "profiling")]
            phase,
        };
        #[cfg(feature = "profiling")]
        enabled::begin(phase, scope.start);
        scope
    }
}
#[cfg(feature = "profiling")]
mod enabled {
    use super::*;
    use std::sync::{
        OnceLock,
        atomic::{AtomicU32, AtomicU64, Ordering},
    };
    static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();
    static MICROS: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];
    static CALLS: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];
    static LAST_MICROS: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];
    static LAST_CALLS: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];
    static ACTIVE: [AtomicU32; NAMES.len()] = [const { AtomicU32::new(0) }; NAMES.len()];
    static SINCE: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];

    fn tracked(phase: Phase) -> bool {
        !matches!(
            phase,
            Phase::Draw | Phase::TargetCreate | Phase::BufferCreate | Phase::GlyphRaster
        )
    }
    pub(super) fn begin(phase: Phase, start: std::time::Instant) {
        if tracked(phase) && ACTIVE[phase as usize].fetch_add(1, Ordering::Relaxed) == 0 {
            let epoch = EPOCH.get_or_init(|| start);
            SINCE[phase as usize].store(
                start.saturating_duration_since(*epoch).as_micros() as u64,
                Ordering::Relaxed,
            );
        }
    }

    impl Drop for Scope {
        fn drop(&mut self) {
            let index = self.phase as usize;
            MICROS[index].fetch_add(self.start.elapsed().as_micros() as u64, Ordering::Relaxed);
            CALLS[index].fetch_add(1, Ordering::Relaxed);
            if tracked(self.phase) {
                ACTIVE[index].fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
    /// Completed phase work since the previous sample, across all threads.
    #[derive(Clone, Copy, Default)]
    pub struct Sample {
        pub micros: [u64; NAMES.len()],
        pub calls: [u64; NAMES.len()],
    }
    impl Sample {
        /// The main-thread collector is the sole consumer. Durations and counts
        /// are sampled independently; concurrent completions may cross a row.
        pub fn take() -> Self {
            let current = Self::snapshot();
            Self {
                micros: std::array::from_fn(|i| {
                    current.micros[i]
                        .wrapping_sub(LAST_MICROS[i].swap(current.micros[i], Ordering::Relaxed))
                }),
                calls: std::array::from_fn(|i| {
                    current.calls[i]
                        .wrapping_sub(LAST_CALLS[i].swap(current.calls[i], Ordering::Relaxed))
                }),
            }
        }
        /// Independent readers observe cumulative counters without consuming them.
        pub fn snapshot() -> Self {
            Self {
                micros: std::array::from_fn(|i| MICROS[i].load(Ordering::Relaxed)),
                calls: std::array::from_fn(|i| CALLS[i].load(Ordering::Relaxed)),
            }
        }
        pub fn delta(self, previous: Self) -> Self {
            Self {
                micros: std::array::from_fn(|i| self.micros[i].wrapping_sub(previous.micros[i])),
                calls: std::array::from_fn(|i| self.calls[i].wrapping_sub(previous.calls[i])),
            }
        }
        /// Elapsed microseconds in phases that have entered and not returned yet.
        pub fn active() -> [Option<u64>; NAMES.len()] {
            let now = EPOCH
                .get_or_init(std::time::Instant::now)
                .elapsed()
                .as_micros() as u64;
            std::array::from_fn(|i| {
                (ACTIVE[i].load(Ordering::Relaxed) != 0)
                    .then(|| now.saturating_sub(SINCE[i].load(Ordering::Relaxed)))
            })
        }
    }
}
#[cfg(feature = "profiling")]
pub use enabled::Sample;
