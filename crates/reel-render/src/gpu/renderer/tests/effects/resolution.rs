//! Full-grid reconstruction keeps logical sampling and target padding isolated.
use super::*;
#[test]
#[ignore = "actual GPU reduced effect raster and pooled target lifetimes"]
fn authored_resolution_preserves_constant_color_through_nested_effects() {
    use crate::common::effect_plan::roi::testing::collect;
    for size in [[69, 51], [160, 120]] {
        let mut control = renderer(64 << 20);
        let (full, _full_bridge) = resolution_chain(size, Affine::IDENTITY, false, 1.);
        let prepared = control.prepare(&full).unwrap();
        let expected = control.readback(prepared).unwrap();
        for scale in [0.5, 0.32, 0.125] {
            let (frame, _bridge) = resolution_chain(size, Affine::IDENTITY, false, scale);
            let mut subject = renderer(64 << 20);
            let (prepared, observations) = collect(|| subject.prepare(&frame).unwrap());
            let actual = subject.readback(prepared).unwrap();
            same_pixels(&expected, &actual);
            assert!(!observations.is_empty());
            let reduced = size.map(|v| (f64::from(v) * scale).ceil() as u32);
            assert!(observations.iter().all(|(_, grid, _)| *grid == reduced));
            let prepared = subject.prepare(&frame).unwrap();
            let cached = subject.readback(prepared).unwrap();
            same_pixels(&actual, &cached);
        }
    }
}
