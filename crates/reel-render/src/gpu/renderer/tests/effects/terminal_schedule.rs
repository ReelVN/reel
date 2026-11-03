use super::*;
use crate::common::offscreen::test_support::{
    terminal_sources, terminal_sources_with_source_free, without_windows,
};

#[test]
#[ignore = "actual GPU repeated-source pixels, pass removal and frame-local recycling"]
fn repeated_source_windows_keep_ordered_pixels_and_remove_duplicate_passes() {
    use crate::common::offscreen::{reuse, test_support::without_atlas};
    let mut control = renderer(1_200_000);
    let mut subject = renderer(1_200_000);
    for transform in [
        Affine::IDENTITY,
        Affine::translate((30.3, 10.7))
            * Affine::rotate(0.19)
            * Affine::scale_non_uniform(0.83, 1.17),
        Affine::translate((180.2, 20.5)) * Affine::scale_non_uniform(-1.13, 0.89),
    ] {
        let frame = reuse::tests::repeated_sources(transform, false);
        control.effect_results.clear();
        subject.effect_results.clear();
        for iteration in 0..4 {
            let (reference, before) = crate::gpu::targets::pass_trace::collect(|| {
                without_atlas(|| reuse::testing::without(|| control.prepare(&frame).unwrap()))
            });
            let quality = reference.quality();
            let expected = control.readback(reference).unwrap();
            let (prepared, after) = crate::gpu::targets::pass_trace::collect(|| {
                without_atlas(|| subject.prepare(&frame).unwrap())
            });
            assert_eq!(prepared.quality(), quality);
            assert!(prepared.budget_scope.targets.usage().peak_bytes <= 1_200_000);
            let actual = subject.readback(prepared).unwrap();
            assert_eq!(
                bytemuck::cast_slice::<[f32; 4], u8>(&actual),
                bytemuck::cast_slice::<[f32; 4], u8>(&expected),
                "shared source changed pixels: {transform:?}, iteration={iteration}"
            );
            if iteration == 0 {
                assert!(
                    after.len() + 4 <= before.len(),
                    "duplicate effect chain still issued"
                );
                eprintln!(
                    "source_reuse transform={transform:?} passes={}->{} changed_bytes=0",
                    before.len(),
                    after.len()
                );
            }
        }
    }
}

#[test]
#[ignore = "actual GPU terminal order, original grids, pixels and attachment changes"]
fn terminal_source_windows_keep_pixels_and_reduce_parent_visits() {
    let mut control = renderer(1_200_000);
    let mut subject = renderer(1_200_000);
    for transform in [
        Affine::IDENTITY,
        Affine::translate((40.3, 10.7))
            * Affine::rotate(0.19)
            * Affine::scale_non_uniform(0.83, 1.17),
        Affine::translate((180.2, 20.5)) * Affine::scale_non_uniform(-1.13, 0.89),
    ] {
        let frame = terminal_sources(transform);
        control.effect_results.clear();
        subject.effect_results.clear();
        let (reference, before) = crate::gpu::targets::pass_trace::collect(|| {
            without_windows(|| control.prepare(&frame).unwrap())
        });
        let quality = reference.quality();
        assert_eq!(quality.samples, 1);
        let expected = control.readback(reference).unwrap();
        assert!(expected.iter().any(|pixel| pixel[3] > 0.));
        let (prepared, after) =
            crate::gpu::targets::pass_trace::collect(|| subject.prepare(&frame).unwrap());
        assert_eq!(prepared.quality(), quality);
        assert!(prepared.budget_scope.targets.usage().peak_bytes <= 1_200_000);
        let actual = subject.readback(prepared).unwrap();
        assert_eq!(
            actual, expected,
            "terminal painter order or grid changed: {transform:?}"
        );
        let switches =
            |trace: &[wgpu::Texture]| trace.windows(2).filter(|pair| pair[0] != pair[1]).count();
        assert!(
            switches(&after) < switches(&before),
            "no parent visit removed"
        );
        assert_eq!(
            after.len(),
            before.len(),
            "all original shader passes retained"
        );
        eprintln!(
            "terminal_window transform={transform:?} switches={}->{} passes={} changed_pixels=0",
            switches(&before),
            switches(&after),
            after.len()
        );
    }
}

#[test]
#[ignore = "actual GPU mixed terminal and source-free parent painter order"]
fn terminal_windows_keep_source_free_parent_and_nested_effect_pixels_exact() {
    let mut control = renderer(1_200_000);
    let mut subject = renderer(1_200_000);
    for nested in [false, true] {
        for transform in [
            Affine::IDENTITY,
            Affine::translate((40.3, 10.7))
                * Affine::rotate(0.19)
                * Affine::scale_non_uniform(0.83, 1.17),
        ] {
            let frame = terminal_sources_with_source_free(transform, nested);
            control.effect_results.clear();
            subject.effect_results.clear();
            let reference = without_windows(|| control.prepare(&frame).unwrap());
            let quality = reference.quality();
            assert_eq!(quality.samples, 1);
            let expected = control.readback(reference).unwrap();
            let prepared = subject.prepare(&frame).unwrap();
            assert_eq!(prepared.quality(), quality);
            let actual = subject.readback(prepared).unwrap();
            assert!(expected.iter().any(|pixel| pixel[3] > 0.));
            assert_eq!(
                bytemuck::cast_slice::<[f32; 4], u8>(&actual),
                bytemuck::cast_slice::<[f32; 4], u8>(&expected),
                "source-free effect moved across a terminal producer: nested={nested} {transform:?}"
            );
        }
    }
}

#[test]
#[ignore = "actual GPU warm inputs and deferred multi-chain scratch retirement"]
fn warm_terminal_chain_windows_keep_input_hits_budget_and_pixels_exact() {
    use crate::common::{
        effect_inputs::testing::collect, target_plan::macrotile::warm_terminal_sources,
    };
    let limit = 1_800_000;
    let (frame, _recording) = warm_terminal_sources();
    let mut control = renderer(limit);
    let mut subject = renderer(limit);
    let reference = without_windows(|| control.prepare(&frame).unwrap());
    let quality = reference.quality();
    assert_eq!(quality.samples, 1);
    assert_eq!(quality.format, wgpu::TextureFormat::Rgba8Unorm);
    let expected = control.readback(reference).unwrap();
    assert!(expected.iter().any(|pixel| pixel[3] > 0.));
    let (schedule, _, _) = crate::common::offscreen::Plan::choose(
        &frame,
        None,
        [640, 480],
        false,
        quality.samples,
        4096,
        &Default::default(),
        limit,
        &|size| crate::gpu::targets::target_bytes(size, quality),
        crate::common::target_plan::MaskExecution::Inline,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    let window_entries = (0..frame.commands().len())
        .filter_map(|index| schedule.entry(index))
        .count();
    assert_eq!(
        window_entries, 3,
        "three terminal producers must share a window"
    );
    let mut hits = 0;
    for iteration in 0..4 {
        let (prepared, inputs) = collect(|| subject.prepare(&frame).unwrap());
        assert_eq!(prepared.quality(), quality);
        let iteration_hits: usize = inputs.iter().map(|input| input.inputs).sum();
        hits += iteration_hits;
        let local = prepared.budget_scope.targets.usage().bytes;
        assert!(local <= limit && subject.usage().targets <= limit);
        let mut owners = Vec::new();
        for owner in prepared.targets.iter().chain(&prepared.retained_inputs) {
            if !owners.iter().any(|old| Arc::ptr_eq(old, owner)) {
                owners.push(owner.clone());
            }
        }
        let physical: u64 = owners
            .iter()
            .map(|owner| {
                crate::gpu::targets::target_bytes(owner.target.storage_size(), quality).unwrap()
            })
            .sum();
        assert_eq!(
            local, physical,
            "every warm input and scratch owner is charged once"
        );
        let actual = subject.readback(prepared).unwrap();
        assert_eq!(
            bytemuck::cast_slice::<[f32; 4], u8>(&actual),
            bytemuck::cast_slice::<[f32; 4], u8>(&expected),
            "warm terminal painter order changed at iteration {iteration}"
        );
        eprintln!(
            "warm_terminal_windows iteration={iteration} inputs={iteration_hits} local={local} physical={physical} limit={limit} cache={:?} changed_bytes=0",
            subject.effect_results.test_stats(),
        );
    }
    assert!(
        hits > 0,
        "the warm liveness path must select prepared inputs"
    );
}
