//! Ordered multi-image sources use the same result ownership as single images.
use super::*;
use crate::common::effect_result::{Cache, Grid, Probe, source_end};
use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
use reel_ui::frame::FrameOp;

#[test]
#[ignore = "actual GPU multi-image cache, pixel, pass and source-budget checks"]
fn ordered_image_compositions_reuse_exact_pixels_and_release_source_metadata() {
    let mut control = renderer(64 << 20);
    let mut subject = renderer(64 << 20);
    for transform in [
        Affine::IDENTITY,
        Affine::translate((0.25, -0.375)),
        Affine::translate((32., 24.)) * Affine::rotate(0.13) * Affine::translate((-32., -24.)),
    ] {
        let (frame, _recording) = image_composition_chain([64, 48], transform, true, 1., 4);
        control.effect_results.clear();
        let expected = control.readback(control.prepare(&frame).unwrap()).unwrap();
        subject.effect_results.clear();
        let (cold, cold_passes, _) =
            crate::gpu::targets::pass_trace::collect_counts(|| subject.prepare(&frame).unwrap());
        assert_eq!(subject.readback(cold).unwrap(), expected);
        let (warm, warm_passes, _) =
            crate::gpu::targets::pass_trace::collect_counts(|| subject.prepare(&frame).unwrap());
        assert!(
            !warm.effect_pins.is_empty(),
            "multi-image result must become a reusable source"
        );
        assert_eq!(subject.readback(warm).unwrap(), expected);
        assert!(
            warm_passes < cold_passes,
            "passes: {cold_passes} -> {warm_passes}"
        );
        eprintln!("four-image source passes: {cold_passes} -> {warm_passes}");

        let quality = subject.gpu().quality([64, 48]).unwrap();
        let grid = Grid {
            source_transform: transform.as_coeffs().map(f64::to_bits),
            effect_transform: transform.as_coeffs().map(f64::to_bits),
            effect_context: None,
            source_size: [64, 48],
            source_storage: [64, 48],
            source_scissor: [0, 0, 64, 48],
            output_size: [64, 48],
            output_storage: [64, 48],
            scissor: [0, 0, 64, 48],
            quality: [
                u32::from(quality.format == wgpu::TextureFormat::Rgba16Float),
                quality.samples,
            ],
        };
        let push = frame
            .commands()
            .iter()
            .rposition(|c| matches!(&c.op, FrameOp::PushGroup(g) if g.effect.is_some()))
            .unwrap();
        let FrameOp::PushGroup(group) = &frame.commands()[push].op else {
            unreachable!()
        };
        let effect = group.effect.as_ref().unwrap();
        let range = push..source_end(frame.commands(), push).unwrap();
        let probe = || Probe::new(frame.drawing(), range.clone(), effect, grid, true).unwrap();
        let pool = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: 128,
                max_bytes: 1 << 20,
            },
        );
        let cache = Cache::<u32>::default();
        let candidate = cache.prepare(probe(), &pool).unwrap();
        assert!(cache.publish(candidate.finish(7), &pool));
        let charged = pool.usage().bytes;
        assert!(charged > (4 * size_of::<usize>()) as u64);
        for _ in 0..64 {
            assert_eq!(cache.lookup(&probe()).unwrap().1, 7);
            assert_eq!(pool.usage().bytes, charged);
        }
        let mut changed_grid = grid;
        changed_grid.source_size[0] += 1;
        let changed =
            Probe::new(frame.drawing(), range.clone(), effect, changed_grid, true).unwrap();
        assert!(cache.lookup(&changed).is_none());
        drop(cache);
        assert_eq!(pool.usage().bytes, 0);
        let tiny = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: 128,
                max_bytes: 32,
            },
        );
        assert!(Cache::<u32>::default().prepare(probe(), &tiny).is_none());
        assert_eq!(tiny.usage().bytes, 0);
    }
}
