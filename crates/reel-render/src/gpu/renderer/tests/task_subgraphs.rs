//! Record real attachment changes while submitting nested independent sources.
use super::*;
use crate::common::offscreen::test_support::{nested_sources, without_windows};

#[test]
#[ignore = "requires actual graphics adapter; no pixel readback"]
fn nested_effect_sources_reduce_attachment_switches_within_the_admitted_budget() {
    let frame = nested_sources();
    let mut baseline = renderer(Default::default());
    let (old, old_passes) = super::super::super::targets::pass_trace::collect(|| {
        without_windows(|| baseline.prepare(&frame).unwrap())
    });
    let mut optimized = renderer(Default::default());
    let (prepared, passes) =
        super::super::super::targets::pass_trace::collect(|| optimized.prepare(&frame).unwrap());
    assert_eq!(prepared.quality(), old.quality());
    let switches =
        |trace: &[wgpu::Texture]| trace.windows(2).filter(|pair| pair[0] != pair[1]).count();
    let old_switches = switches(&old_passes);
    let new_switches = switches(&passes);
    assert!(
        new_switches < old_switches,
        "old={old_switches} new={new_switches}"
    );
    assert!(passes.len() <= old_passes.len());
    let old_peak = old.budget_scope.targets.usage().peak_bytes;
    let peak = prepared.budget_scope.targets.usage().peak_bytes;
    assert!(peak <= optimized.gpu.limits.target_bytes);
    let mut tight = renderer(crate::gpu::GpuLimits {
        target_bytes: old_peak,
        ..Default::default()
    });
    let (tight_prepared, tight_passes) =
        super::super::super::targets::pass_trace::collect(|| tight.prepare(&frame).unwrap());
    assert_eq!(
        tight_prepared.quality(),
        old.quality(),
        "narrow the source window before changing the original quality"
    );
    assert!(tight_prepared.budget_scope.targets.usage().peak_bytes <= old_peak);
    let format = prepared.quality().format;
    let samples = prepared.quality().samples;
    baseline.submit(old).unwrap();
    optimized.submit(prepared).unwrap();
    tight.submit(tight_prepared).unwrap();
    for r in [&mut baseline, &mut optimized, &mut tight] {
        r.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        r.poll().unwrap();
    }
    eprintln!(
        "nested_effect_sources=2 format={format:?} samples={samples} actual_passes={}->{} actual_attachment_switches={old_switches}->{new_switches} target_peak={old_peak}->{peak}; tight_passes={} tight_switches={}",
        old_passes.len(),
        passes.len(),
        tight_passes.len(),
        switches(&tight_passes)
    );
}
