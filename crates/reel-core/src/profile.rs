//! Optional runtime profiling for host and device measurements.
//! Nested phases overlap. Worker durations are attributed when work completes;
//! their totals can exceed the main thread's elapsed time. Normal builds erase
//! scopes and counters; profiling builds use fixed atomic arrays without IO.

/// Stages recorded by the shared runtime and device backend.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Phase {
    Input,
    Tick,
    /// Session scheduling, interpreter slices and native host work.
    Session,
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
    RetainedPlan,
    /// Interpreter slices, including VM builtins; excludes external host dispatch.
    VmSlice,
    /// External native dispatch, including continuation resumes.
    HostCall,
    /// Ordered copies between GPU-resident textures; includes submission CPU time.
    TextureCopy,
    /// Sampling native controller and touch state, before input dispatch.
    ControlPoll,
    /// Host event wait, including timeout and scheduler wake latency.
    HostWait,
}
pub const NAMES: [&str; 38] = [
    "input",
    "tick",
    "session",
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
    "retained_plan",
    "vm_slice",
    "host_call",
    "texture_copy",
    "control_poll",
    "host_wait",
];

/// Record retained-result reuse and the dependency bytes encoded for its key.
#[inline]
pub fn retained_cache(hit: bool, bytes: u64) {
    #[cfg(feature = "profiling")]
    enabled::retained_cache(hit, bytes);
    #[cfg(not(feature = "profiling"))]
    let _ = (hit, bytes);
}

/// Record bytes transferred into a geometry buffer by one upload operation.
/// Builds without profiling erase this counter alongside timing scopes.
#[inline]
pub fn geometry_upload(bytes: u64) {
    #[cfg(feature = "profiling")]
    enabled::geometry_upload(bytes);
    #[cfg(not(feature = "profiling"))]
    let _ = bytes;
}

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
#[cfg(not(feature = "profiling"))]
impl Drop for Scope {
    #[inline]
    fn drop(&mut self) {}
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
    static GEOMETRY_CALLS: AtomicU64 = AtomicU64::new(0);
    static GEOMETRY_BYTES: AtomicU64 = AtomicU64::new(0);
    static RETAINED_HITS: AtomicU64 = AtomicU64::new(0);
    static RETAINED_MISSES: AtomicU64 = AtomicU64::new(0);
    static RETAINED_KEY_BYTES: AtomicU64 = AtomicU64::new(0);

    pub(super) fn retained_cache(hit: bool, bytes: u64) {
        if hit {
            RETAINED_HITS.fetch_add(1, Ordering::Relaxed);
        } else {
            RETAINED_MISSES.fetch_add(1, Ordering::Relaxed);
        }
        RETAINED_KEY_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }
    /// Cumulative retained-result lookups. Key bytes contain paint dependencies.
    #[derive(Clone, Copy, Default)]
    pub struct RetainedCache {
        pub hits: u64,
        pub misses: u64,
        pub key_bytes: u64,
    }
    impl RetainedCache {
        /// Counters are independent observations of completed lookup work.
        pub fn snapshot() -> Self {
            Self {
                hits: RETAINED_HITS.load(Ordering::Relaxed),
                misses: RETAINED_MISSES.load(Ordering::Relaxed),
                key_bytes: RETAINED_KEY_BYTES.load(Ordering::Relaxed),
            }
        }
        /// Lookup work between two observations.
        pub fn delta(self, previous: Self) -> Self {
            Self {
                hits: self.hits.wrapping_sub(previous.hits),
                misses: self.misses.wrapping_sub(previous.misses),
                key_bytes: self.key_bytes.wrapping_sub(previous.key_bytes),
            }
        }
    }

    pub(super) fn geometry_upload(bytes: u64) {
        if bytes != 0 {
            GEOMETRY_BYTES.fetch_add(bytes, Ordering::Relaxed);
            GEOMETRY_CALLS.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Cumulative geometry transfers. Cache hits contribute zero bytes.
    #[derive(Clone, Copy, Default)]
    pub struct GeometryUploads {
        pub calls: u64,
        pub bytes: u64,
    }
    impl GeometryUploads {
        /// Counters are sampled independently; concurrent transfers may cross a row.
        pub fn snapshot() -> Self {
            Self {
                calls: GEOMETRY_CALLS.load(Ordering::Relaxed),
                bytes: GEOMETRY_BYTES.load(Ordering::Relaxed),
            }
        }

        /// Transfers recorded between two observations.
        pub fn delta(self, previous: Self) -> Self {
            Self {
                calls: self.calls.wrapping_sub(previous.calls),
                bytes: self.bytes.wrapping_sub(previous.bytes),
            }
        }
    }

    fn tracked(phase: Phase) -> bool {
        !matches!(
            phase,
            Phase::Draw
                | Phase::TargetCreate
                | Phase::BufferCreate
                | Phase::GlyphRaster
                | Phase::VmSlice
                | Phase::HostCall
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
    #[derive(Clone, Copy)]
    pub struct Sample {
        pub micros: [u64; NAMES.len()],
        pub calls: [u64; NAMES.len()],
    }
    impl Default for Sample {
        fn default() -> Self {
            Self {
                micros: [0; NAMES.len()],
                calls: [0; NAMES.len()],
            }
        }
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
pub use enabled::{GeometryUploads, RetainedCache, Sample};
