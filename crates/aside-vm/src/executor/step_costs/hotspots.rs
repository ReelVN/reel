use crate::{HeapBudgetUsage, HeapError, root_ranges::ChargedVec};
use std::cell::Cell;

/// Maximum distinct sampled instruction locations pending a drain.
pub const STEP_HOTSPOT_CAPACITY: usize = 8192;
/// Execution mode names used by `StepPoint::mode`.
pub const STEP_MODES: [&str; 24] = [
    "body",
    "copy",
    "rest",
    "bind",
    "defaults",
    "parameter",
    "install",
    "default_call",
    "compact_begin",
    "exit",
    "exit_apply",
    "release",
    "native",
    "generator",
    "search",
    "sort",
    "class",
    "constant",
    "spread",
    "table",
    "host_method",
    "builtin",
    "text",
    "equality",
];

/// Logical location captured before a sampled operation. Native entries identify
/// their native declaration and caller; preparation positions denote the function.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepPoint {
    /// Function ID in this VM's immutable verified program.
    pub function: u32,
    /// Word PC in the function, suitable for joining its bytecode/source map.
    pub pc: u32,
    /// Native declaration ID when the current frame is native.
    pub native: Option<u32>,
    /// Index in `STEP_MODES`; separates preparation from body instructions.
    pub mode: u8,
}

/// Timed samples at one exact function/PC/stage. These are sampled wall durations
/// including instrumentation, not exclusive CPU time or an instruction histogram.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepHotspot {
    /// Captured logical execution position.
    pub point: StepPoint,
    /// Index in `STEP_NAMES`; fetch/cost/execute overlap the outer stage.
    pub phase: u8,
    /// Number of sampled entries at this location.
    pub samples: u64,
    /// Total elapsed nanoseconds for those samples only.
    pub sample_wall_ns: u64,
}

pub(super) struct Hotspots {
    rows: ChargedVec<Cell<StepHotspot>>,
    dropped: Cell<u64>,
}
impl Hotspots {
    pub(super) fn new(usage: &HeapBudgetUsage) -> Result<Self, HeapError> {
        let mut rows = ChargedVec::new(usage, STEP_HOTSPOT_CAPACITY)?;
        rows.data.resize_with(STEP_HOTSPOT_CAPACITY, Cell::default);
        Ok(Self {
            rows,
            dropped: Cell::new(0),
        })
    }
}
impl Hotspots {
    fn start(point: StepPoint, phase: u8) -> usize {
        let mut hash = point.function.wrapping_mul(0x9e37_79b9)
            ^ point.pc.wrapping_mul(0x85eb_ca6b)
            ^ point.native.unwrap_or(u32::MAX).wrapping_mul(0xc2b2_ae35)
            ^ u32::from(phase).wrapping_mul(17)
            ^ u32::from(point.mode);
        hash = (hash ^ (hash >> 16)).wrapping_mul(0x7feb_352d);
        hash ^= hash >> 15;
        (hash as usize & (STEP_HOTSPOT_CAPACITY / 8 - 1)) * 8
    }
    pub(super) fn record(&self, point: StepPoint, phase: u8, elapsed: u64) -> bool {
        let start = Self::start(point, phase);
        for cell in &self.rows.data[start..start + 8] {
            let mut row = cell.get();
            if row.samples == 0 || (row.point == point && row.phase == phase) {
                row.point = point;
                row.phase = phase;
                row.samples += 1;
                row.sample_wall_ns += elapsed;
                cell.set(row);
                return true;
            }
        }
        self.dropped.set(self.dropped.get() + 1);
        false
    }
    pub(super) fn drain(&self, output: &mut [StepHotspot; STEP_HOTSPOT_CAPACITY]) -> u64 {
        for (out, cell) in output.iter_mut().zip(&self.rows.data) {
            *out = cell.replace(StepHotspot::default());
        }
        self.dropped.replace(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn storage_is_admitted_before_allocation_and_refunded_on_drop() {
        let usage = HeapBudgetUsage::new(u64::MAX);
        let initial = usage.charged_bytes();
        let rows = Hotspots::new(&usage).unwrap();
        assert!(
            usage.charged_bytes()
                >= initial + (STEP_HOTSPOT_CAPACITY * size_of::<Cell<StepHotspot>>()) as u64
        );
        drop(rows);
        assert_eq!(usage.charged_bytes(), initial);
        let small = HeapBudgetUsage::new(1);
        assert!(Hotspots::new(&small).is_err());
    }

    #[test]
    fn keys_keep_function_pc_native_and_mode_and_bound_collisions() {
        let rows = Hotspots::new(&HeapBudgetUsage::new(u64::MAX)).unwrap();
        let base = StepPoint {
            function: 3,
            pc: 7,
            native: None,
            mode: 0,
        };
        rows.record(base, 1, 20);
        rows.record(base, 1, 30);
        let mut changed = base;
        changed.mode = 1;
        rows.record(changed, 1, 40);
        let mut output = [StepHotspot::default(); STEP_HOTSPOT_CAPACITY];
        assert_eq!(rows.drain(&mut output), 0);
        let row = output
            .iter()
            .find(|row| row.point == base && row.samples != 0)
            .unwrap();
        assert_eq!((row.samples, row.sample_wall_ns), (2, 50));
        assert!(
            output
                .iter()
                .any(|row| row.point == changed && row.samples == 1)
        );
        for pc in 0..100_000 {
            rows.record(StepPoint { pc, ..base }, 1, 1);
        }
        assert!(rows.drain(&mut output) > 0);
        assert!(output.iter().filter(|row| row.samples != 0).count() <= STEP_HOTSPOT_CAPACITY);
        assert_eq!(rows.drain(&mut output), 0);
        assert!(output.iter().all(|row| row.samples == 0));
    }
}
