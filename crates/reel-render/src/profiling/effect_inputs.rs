//! CPU selection cost and coverage of already submitted effect inputs.
use std::sync::atomic::{AtomicU64, Ordering};
static TOTALS: [AtomicU64; 20] = [const { AtomicU64::new(0) }; 20];
#[cfg(any(feature = "wgpu", feature = "gxm"))]
pub(crate) enum EffectInputEvent {
    #[cfg(feature = "wgpu")]
    BusyReturns = 0,
    #[cfg(feature = "wgpu")]
    PreparingReturns = 1,
    #[cfg(feature = "wgpu")]
    PressureReturns = 2,
    #[cfg(feature = "wgpu")]
    PrepareRetries = 3,
    #[cfg(feature = "wgpu")]
    OffscreenRetries = 4,
    #[cfg(feature = "wgpu")]
    TargetPoolPressure = 5,
    #[cfg(feature = "wgpu")]
    TargetCreations = 6,
    TailOutputsReleased = 7,
    #[cfg(feature = "wgpu")]
    InputInstalls = 8,
    #[cfg(feature = "wgpu")]
    IdleTargetBindingsReleased = 9,
}
#[cfg(any(feature = "wgpu", feature = "gxm"))]
pub(crate) fn record_effect_input_event(event: EffectInputEvent, count: u64) {
    TOTALS[10 + event as usize].fetch_add(count, Ordering::Relaxed);
}
#[derive(Default, Clone, Copy, Debug)]
pub struct EffectInputPlans {
    pub preparations: u64,
    pub source_scans: u64,
    pub candidates: u64,
    pub events: u64,
    pub recounts: u64,
    pub rejected_owners: u64,
    pub scan_ns: u64,
    pub recount_ns: u64,
    pub selected_inputs: u64,
    /// Sum of per-preparation unique owner charges, rather than current residency.
    pub pinned_bytes: u64,
    pub busy_returns: u64,
    pub preparing_returns: u64,
    pub pressure_returns: u64,
    pub prepare_retries: u64,
    pub offscreen_retries: u64,
    pub target_pool_pressure: u64,
    /// Physical target owners created, including their selected MSAA storage.
    pub target_creations: u64,
    /// Cache pins released; submitted readers may still hold the same owner.
    pub tail_outputs_released: u64,
    pub input_installs: u64,
    /// Completed uniform slots whose target bindings yielded to scratch demand.
    pub idle_target_bindings_released: u64,
}
#[cfg(any(feature = "wgpu", feature = "gxm"))]
pub(crate) fn record_effect_input_plan(
    stats: crate::common::effect_inputs::Stats,
    selected: u64,
    bytes: u64,
) {
    let values = [
        1,
        stats.scans,
        stats.candidates,
        stats.events,
        stats.recounts,
        stats.rejected_owners,
        stats.scan_ns,
        stats.recount_ns,
        selected,
        bytes,
    ];
    for (counter, value) in TOTALS.iter().zip(values) {
        counter.fetch_add(value, Ordering::Relaxed);
    }
}
/// Drain host planning work, including unsuccessful optional selections.
pub fn drain_effect_input_plans() -> EffectInputPlans {
    let row = TOTALS
        .each_ref()
        .map(|counter| counter.swap(0, Ordering::Relaxed));
    EffectInputPlans {
        preparations: row[0],
        source_scans: row[1],
        candidates: row[2],
        events: row[3],
        recounts: row[4],
        rejected_owners: row[5],
        scan_ns: row[6],
        recount_ns: row[7],
        selected_inputs: row[8],
        pinned_bytes: row[9],
        busy_returns: row[10],
        preparing_returns: row[11],
        pressure_returns: row[12],
        prepare_retries: row[13],
        offscreen_retries: row[14],
        target_pool_pressure: row[15],
        target_creations: row[16],
        tail_outputs_released: row[17],
        input_installs: row[18],
        idle_target_bindings_released: row[19],
    }
}
