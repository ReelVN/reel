//! Bounded effect-work attribution for profiling builds. Counts describe issued
//! drawing work, including retries, rather than GPU completion or shaded pixels.
//! Producers never allocate, wait for a lock, or write logs. A full table or a
//! concurrent drain drops an observation and increments the reported loss count.
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

mod effect_inputs;
mod frames;
#[cfg(all(feature = "gxm", target_os = "vita"))]
pub(crate) mod targets;
mod trace;
#[cfg(any(feature = "wgpu", feature = "gxm"))]
pub(crate) use effect_inputs::{
    EffectInputEvent, record_effect_input_event, record_effect_input_plan,
};
pub use effect_inputs::{EffectInputPlans, drain_effect_input_plans};
pub use trace::{
    KNOWN_ERROR_CODE, KNOWN_LOGICAL, KNOWN_OUTPUT_EXTENT, KNOWN_SCISSOR, KNOWN_SOURCE_EXTENT,
    KNOWN_STORAGE, KNOWN_STORAGE_KIND, KNOWN_STORE_REGION, KNOWN_STRIDE, KNOWN_TARGET_FORMAT,
    KNOWN_TEXTURE_BINDINGS, KNOWN_VALID_REGION, KNOWN_WALL, KNOWN_WRITE_REGION, TRACE_CAPACITY,
    TraceData, TraceDrain, TraceEvent, TraceKind, TraceStatus, drain_trace, set_trace_enabled,
    trace_enabled, write_trace, write_trace_rows,
};
#[cfg(all(feature = "gxm", target_os = "vita"))]
pub(crate) use trace::{
    TraceAttempt, TraceLink, TracePass, record_trace, trace_completed, trace_link, trace_submission,
};
mod paint;
mod paint_domain;
mod passes;
mod source_crop;
#[cfg(all(feature = "gxm", target_os = "vita"))]
pub(crate) use frames::FrameCompletion;
pub use frames::{FrameCompletionTiming, drain_frame_completion, set_frame_completion};
#[cfg(all(feature = "gxm", target_os = "vita"))]
pub(crate) use paint::record_paint_regions;
pub use paint::{PaintRegions, drain_paint_regions};
pub use paint_domain::{PaintDomain, drain_paint_domain};
#[cfg(feature = "wgpu")]
pub(crate) use paint_domain::{paint_domain_enabled, record_paint_domain};
#[cfg(all(feature = "gxm", target_os = "vita"))]
pub(crate) use passes::record_serial_pass;
pub use passes::{SerialPassTiming, drain_serial_passes};
#[cfg(any(feature = "wgpu", feature = "gxm"))]
pub(crate) use source_crop::{CropDecision, record_source_crop_plan, record_source_crop_window};
pub use source_crop::{SourceCropPlans, drain_source_crop_plans};

/// Maximum distinct effect/variant pairs retained between sampler drains.
pub const EFFECT_CAPACITY: usize = 128;

/// Cache miss counter order. Causes overlap when several dependencies changed;
/// attempts include both early source lookup and ordinary output lookup.
pub const CACHE_MISS_LABELS: [&str; 18] = [
    "cold",
    "recording",
    "source_token",
    "effect",
    "domain",
    "source_extent",
    "source_scissor",
    "effect_transform",
    "output_extent",
    "output_scissor",
    "quality",
    "unretained",
    "unproven",
    "recording_identity",
    "recording_pixels",
    "context_position",
    "context_source",
    "context_domain",
];

static SERIAL_EFFECT_TIMING: AtomicBool = AtomicBool::new(false);
static SHARED_SOURCE_ATLAS: AtomicBool = AtomicBool::new(crate::SHARED_SOURCE_ATLAS_DEFAULT);

/// Select shared source storage for diagnostics. Disabling it keeps the same
/// ordered source batching with independent textures, subject to its budget.
/// Vita starts with independent storage; desktop retains shared packing.
pub fn set_shared_source_atlas(enabled: bool) {
    SHARED_SOURCE_ATLAS.store(enabled, Ordering::Relaxed);
}

/// Return the current diagnostic source-storage policy. Its initial value is
/// the ordinary engine default, so probes can inherit it before applying an
/// explicit `source-atlas=on` or `source-atlas=off` diagnostic selection.
pub fn shared_source_atlas() -> bool {
    SHARED_SOURCE_ATLAS.load(Ordering::Relaxed)
}

/// Enable diagnostic GXM drains around effects, clears and source-paint draws.
/// Drains change scene batching, overlap and intermediate attachment stores;
/// results attribute isolated work and cannot serve as production frame times
/// or a pixel-equivalence reference. Disabled by default; other backends ignore it.
pub fn set_serial_effect_timing(enabled: bool) {
    SERIAL_EFFECT_TIMING.store(enabled, Ordering::Relaxed);
}

/// Return whether GXM's diagnostic serial drains are enabled. These drains
/// change batching and overlap, so their timings are isolated observations.
pub fn serial_effect_timing() -> bool {
    SERIAL_EFFECT_TIMING.load(Ordering::Relaxed)
}

/// One effect's accumulated work in a sampler interval.
#[derive(Clone, Copy, Debug, Default)]
pub struct EffectSample {
    /// Compiled effect identity, suitable for joining exported shader metadata.
    pub definition: [u8; 32],
    /// 0 ordinary, 1 alpha-coverage, 2 full-target, 3 cached output or lookup,
    /// 4 ordinary color with a proved invariant source axis and diagonal mapping.
    pub variant: u8,
    /// Successfully issued effect draws, including preparation retries.
    pub draws: u64,
    /// Reused effect outputs.
    pub hits: u64,
    /// The subset of hits found before drawing the effect's source.
    pub early_hits: u64,
    /// Failed lookup attempts by `CACHE_MISS_LABELS`, including overlapping causes.
    /// Unproven counts ordinary invocations without an eligible cache probe.
    pub cache_misses: [u64; CACHE_MISS_LABELS.len()],
    /// Sum of logical source areas for sampled inputs.
    pub source_pixels: u64,
    /// Sum of physical source-storage areas for sampled inputs.
    pub source_storage_pixels: u64,
    /// Draws sampling a smaller backing on the original logical source grid.
    pub crop_source_draws: u64,
    /// Complete original source-storage areas replaced by cropped backings.
    pub crop_source_original_storage_pixels: u64,
    /// Actual backing area sampled by cropped source draws, excluding other draws.
    pub crop_source_storage_pixels: u64,
    /// Sum of logical output areas.
    pub output_pixels: u64,
    /// Sum of physical output-storage areas.
    pub output_storage_pixels: u64,
    /// Scissor area summed over draws; an upper bound, not a fragment counter.
    pub scissor_pixels: u64,
    /// Bound texture units summed over draws, not texture-fetch instructions.
    pub sampler_bindings: u64,
    /// CPU wall time inside the final native draw call. It can wait for earlier
    /// scenes and must not be interpreted as this effect's GPU execution time.
    pub submit_wall_us: u64,
    /// Effect draws measured with explicit before/after completion drains.
    pub serial_draws: u64,
    /// Time draining work preceding serially measured effect draws.
    pub serial_prior_drain_us: u64,
    /// Time draining each serially measured draw after submission. Includes scene
    /// ending, native drain and polling overhead; this is not a GPU timestamp.
    pub serial_completion_drain_us: u64,
    /// Largest single completion drain in the interval, including native overhead.
    pub serial_max_completion_drain_us: u64,
    /// Dimensions of the largest physical source observed.
    pub max_source: [u32; 2],
    /// Dimensions of the largest physical output observed.
    pub max_output: [u32; 2],
    /// Widget owner responsible for the largest observed output allocation.
    pub largest_owner: u64,
}

#[cfg(any(all(feature = "gxm", target_os = "vita"), feature = "wgpu", test))]
pub(crate) fn area(size: [u32; 2]) -> u64 {
    u64::from(size[0]) * u64::from(size[1])
}

impl EffectSample {
    #[cfg(any(all(feature = "gxm", target_os = "vita"), feature = "wgpu", test))]
    fn add(&mut self, other: Self) {
        self.draws += other.draws;
        self.hits += other.hits;
        self.early_hits += other.early_hits;
        for (total, value) in self.cache_misses.iter_mut().zip(other.cache_misses) {
            *total += value;
        }
        self.source_pixels += other.source_pixels;
        self.source_storage_pixels += other.source_storage_pixels;
        self.crop_source_draws += other.crop_source_draws;
        self.crop_source_original_storage_pixels += other.crop_source_original_storage_pixels;
        self.crop_source_storage_pixels += other.crop_source_storage_pixels;
        self.output_pixels += other.output_pixels;
        self.output_storage_pixels += other.output_storage_pixels;
        self.scissor_pixels += other.scissor_pixels;
        self.sampler_bindings += other.sampler_bindings;
        self.submit_wall_us += other.submit_wall_us;
        self.serial_draws += other.serial_draws;
        self.serial_prior_drain_us += other.serial_prior_drain_us;
        self.serial_completion_drain_us += other.serial_completion_drain_us;
        self.serial_max_completion_drain_us = self
            .serial_max_completion_drain_us
            .max(other.serial_max_completion_drain_us);
        if area(other.max_source) > area(self.max_source) {
            self.max_source = other.max_source;
        }
        if area(other.max_output) > area(self.max_output) {
            self.max_output = other.max_output;
            self.largest_owner = other.largest_owner;
        }
    }
}

struct Effects {
    rows: [EffectSample; EFFECT_CAPACITY],
    len: usize,
}

impl Effects {
    const fn new() -> Self {
        Self {
            rows: [EffectSample {
                definition: [0; 32],
                variant: 0,
                draws: 0,
                hits: 0,
                early_hits: 0,
                cache_misses: [0; CACHE_MISS_LABELS.len()],
                source_pixels: 0,
                source_storage_pixels: 0,
                crop_source_draws: 0,
                crop_source_original_storage_pixels: 0,
                crop_source_storage_pixels: 0,
                output_pixels: 0,
                output_storage_pixels: 0,
                scissor_pixels: 0,
                sampler_bindings: 0,
                submit_wall_us: 0,
                serial_draws: 0,
                serial_prior_drain_us: 0,
                serial_completion_drain_us: 0,
                serial_max_completion_drain_us: 0,
                max_source: [0; 2],
                max_output: [0; 2],
                largest_owner: 0,
            }; EFFECT_CAPACITY],
            len: 0,
        }
    }
    #[cfg(any(all(feature = "gxm", target_os = "vita"), feature = "wgpu", test))]
    fn record(&mut self, sample: EffectSample) -> bool {
        if let Some(row) = self.rows[..self.len]
            .iter_mut()
            .find(|row| row.definition == sample.definition && row.variant == sample.variant)
        {
            row.add(sample);
        } else if self.len < EFFECT_CAPACITY {
            self.rows[self.len] = sample;
            self.len += 1;
        } else {
            return false;
        }
        true
    }
    fn drain(&mut self, output: &mut Vec<EffectSample>) {
        output.clear();
        output.extend_from_slice(&self.rows[..self.len]);
        self.len = 0;
    }
}

static EFFECTS: Mutex<Effects> = Mutex::new(Effects::new());
static DROPPED: AtomicU64 = AtomicU64::new(0);

#[cfg(any(all(feature = "gxm", target_os = "vita"), feature = "wgpu"))]
pub(crate) fn record_effect(sample: EffectSample) {
    #[cfg(all(feature = "gxm", target_os = "vita"))]
    if sample.variant == 3 {
        record_trace(
            TraceKind::EffectCache,
            TraceData {
                label: if sample.hits != 0 {
                    "effect-cache-hit"
                } else {
                    "effect-cache-lookup"
                },
                status: if sample.hits != 0 {
                    TraceStatus::Ok
                } else {
                    TraceStatus::Skipped
                },
                definition: sample.definition,
                owner: sample.largest_owner,
                count: sample.hits,
                ..Default::default()
            },
        );
    }
    record_effect_into(&EFFECTS, &DROPPED, sample);
}

#[cfg(any(all(feature = "gxm", target_os = "vita"), feature = "wgpu", test))]
#[inline]
fn record_effect_into(table: &Mutex<Effects>, dropped: &AtomicU64, sample: EffectSample) {
    if !table
        .try_lock()
        .is_ok_and(|mut effects| effects.record(sample))
    {
        dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// Drain interval counters into reusable sampler storage. Only one consumer
/// should drain. `None` leaves all counters pending when a producer owns the lock.
/// The returned number counts observations dropped since the preceding drain.
pub fn drain_effects(output: &mut Vec<EffectSample>) -> Option<u64> {
    drain_effects_from(&EFFECTS, &DROPPED, output)
}

fn drain_effects_from(
    table: &Mutex<Effects>,
    dropped: &AtomicU64,
    output: &mut Vec<EffectSample>,
) -> Option<u64> {
    output.clear();
    output.reserve(EFFECT_CAPACITY);
    let Ok(mut effects) = table.try_lock() else {
        return None;
    };
    effects.drain(output);
    Some(dropped.swap(0, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_busy_sampler_drops_and_reports_without_waiting() {
        let table = Mutex::new(Effects::new());
        let dropped = AtomicU64::new(0);
        let guard = table.lock().unwrap();
        record_effect_into(&table, &dropped, EffectSample::default());
        drop(guard);
        let mut output = Vec::new();
        assert_eq!(drain_effects_from(&table, &dropped, &mut output), Some(1));
        assert!(output.is_empty());
    }

    #[test]
    fn variants_merge_without_losing_area_and_owner_attribution() {
        let mut effects = Effects::new();
        let base = EffectSample {
            definition: [7; 32],
            draws: 1,
            cache_misses: [1; CACHE_MISS_LABELS.len()],
            serial_draws: 1,
            serial_prior_drain_us: 17,
            serial_completion_drain_us: 23,
            serial_max_completion_drain_us: 23,
            scissor_pixels: 8,
            max_output: [4, 4],
            largest_owner: 12,
            ..Default::default()
        };
        assert!(effects.record(base));
        assert!(effects.record(EffectSample {
            max_output: [8, 4],
            largest_owner: 19,
            ..base
        }));
        assert!(effects.record(EffectSample {
            variant: 3,
            draws: 0,
            hits: 1,
            early_hits: 1,
            ..base
        }));
        let mut output = Vec::with_capacity(EFFECT_CAPACITY);
        effects.drain(&mut output);
        assert_eq!(output.len(), 2);
        assert_eq!(
            (
                output[0].draws,
                output[0].scissor_pixels,
                output[0].largest_owner
            ),
            (2, 16, 19)
        );
        assert_eq!((output[1].hits, output[1].early_hits), (1, 1));
        assert_eq!(output[0].cache_misses, [2; CACHE_MISS_LABELS.len()]);
        assert_eq!(output[1].cache_misses, [1; CACHE_MISS_LABELS.len()]);
        assert_eq!(
            (
                output[0].serial_draws,
                output[0].serial_prior_drain_us,
                output[0].serial_completion_drain_us,
                output[0].serial_max_completion_drain_us
            ),
            (2, 34, 46, 23)
        );
        effects.drain(&mut output);
        assert!(output.is_empty());
    }

    #[test]
    fn capacity_refuses_new_keys_but_keeps_existing_keys_and_reuses_storage() {
        let mut effects = Effects::new();
        for index in 0..EFFECT_CAPACITY {
            assert!(effects.record(EffectSample {
                definition: [index as u8; 32],
                draws: 1,
                ..Default::default()
            }));
        }
        assert!(!effects.record(EffectSample {
            definition: [255; 32],
            ..Default::default()
        }));
        assert!(effects.record(EffectSample {
            definition: [0; 32],
            hits: 1,
            ..Default::default()
        }));
        let mut output = Vec::with_capacity(EFFECT_CAPACITY);
        let capacity = output.capacity();
        effects.drain(&mut output);
        assert_eq!(output.len(), EFFECT_CAPACITY);
        assert_eq!(output[0].hits, 1);
        assert_eq!(output.capacity(), capacity);
        assert!(effects.record(EffectSample {
            definition: [255; 32],
            ..Default::default()
        }));
    }
}
