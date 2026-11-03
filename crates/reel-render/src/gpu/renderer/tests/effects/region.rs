//! Existing source pixels remain usable as the visible ROI moves inside them.
use super::*;
use reel_ui::{frame::FrameOp, resources::ResourceSnapshot};

#[test]
#[ignore = "actual GPU source coverage reuse and newly exposed pixels"]
fn moved_viewport_reuses_only_covered_source_pixels() {
    let (original, mut bridge) = transformed_chain([64, 48], Affine::IDENTITY, true);
    let owner = original
        .commands()
        .iter()
        .find_map(|command| {
            if let FrameOp::PushGroup(group) = &command.op {
                group.effect.as_ref().map(|_| group.owner)
            } else {
                None
            }
        })
        .unwrap();
    let pool = r01_fixture::pool();
    let mut subject = renderer(64 << 20);
    let mut control = renderer(64 << 20);
    let warm = subject.prepare(&original).unwrap();
    subject.readback(warm).unwrap();
    let mut coverage_hits = 0;
    for (index, offset) in [(8., 4.), (16., 9.), (-11., -6.), (0., 0.)]
        .into_iter()
        .enumerate()
    {
        bridge.edit_widget(owner, |mut widget| {
            widget.set_transform(Affine::translate(offset))
        });
        let mut build = FrameBuild::new(
            bridge.record(FrameId(index as u64 + 2)).unwrap(),
            ResourceSnapshot::capture(original.resources().leases().iter().cloned().map(Ok), &pool)
                .unwrap(),
            original.info(),
            r01_fixture::limits(),
            &pool,
        )
        .unwrap();
        while !build.step(r01_fixture::step_budget()).unwrap().complete {}
        let frame = build.finish().unwrap();
        control.effect_results.clear();
        let reference = control.prepare(&frame).unwrap();
        let expected = control.readback(reference).unwrap();
        let (candidate, hits) = crate::common::effect_result::coverage::testing::collect(|| {
            subject.prepare(&frame).unwrap()
        });
        coverage_hits += hits;
        let actual = subject.readback(candidate).unwrap();
        assert_eq!(
            actual, expected,
            "source coverage changed pixels at {offset:?}"
        );
    }
    assert!(
        coverage_hits > 0,
        "the moving viewport must exercise proved coverage reads"
    );
}
