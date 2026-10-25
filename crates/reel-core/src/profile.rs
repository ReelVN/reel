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
}
pub const NAMES: [&str; 19] = [
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
        Self {
            #[cfg(feature = "profiling")]
            start: std::time::Instant::now(),
            #[cfg(feature = "profiling")]
            phase,
        }
    }
}
#[cfg(feature = "profiling")]
mod enabled {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static MICROS: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];
    static CALLS: [AtomicU64; NAMES.len()] = [const { AtomicU64::new(0) }; NAMES.len()];

    impl Drop for Scope {
        fn drop(&mut self) {
            let index = self.phase as usize;
            MICROS[index].fetch_add(self.start.elapsed().as_micros() as u64, Ordering::Relaxed);
            CALLS[index].fetch_add(1, Ordering::Relaxed);
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
            Self {
                micros: std::array::from_fn(|i| MICROS[i].swap(0, Ordering::Relaxed)),
                calls: std::array::from_fn(|i| CALLS[i].swap(0, Ordering::Relaxed)),
            }
        }
    }
}
#[cfg(feature = "profiling")]
pub use enabled::Sample;
