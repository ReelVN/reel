//! Intermediate storage retains the original shader grid across cold and warm frames.
use super::*;

#[test]
#[ignore = "actual GPU; intermediate ROI backing, blur halos and warm content reuse"]
fn intermediate_backing_preserves_pixels_and_warm_source_reuse() {
    use crate::common::{effect_inputs::testing::collect, effect_output::testing::without};
    let transforms = [
        Affine::translate((0., 128.)),
        Affine::translate((-16.5, 128.25)),
        Affine::new([1., 0.025, 0.04, 1., -12.25, 128.5]),
        Affine::translate((24.25, 143.5)) * Affine::rotate(-0.035),
    ];
    for depth in [2, 4] {
        for (transform, complex_source) in transforms
            .into_iter()
            .map(|transform| (transform, true))
            .chain([(transforms[0], false), (transforms[2], false)])
        {
            let (frame, _recording) = frame_config(true, depth, transform, true, complex_source);
            let make_renderer = || {
                Renderer::new(
                    pollster::block_on(Gpu::request(
                        &wgpu::Instance::default(),
                        None,
                        DeviceGeneration(1),
                        GpuLimits {
                            target_bytes: 10 << 20,
                            ..Default::default()
                        },
                    ))
                    .unwrap(),
                )
            };
            let mut control = make_renderer();
            let mut subject = make_renderer();
            let (reference, reference_targets) = crate::gpu::targets::pass_trace::collect(|| {
                without(|| control.prepare(&frame).unwrap())
            });
            let quality = reference.quality();
            assert_eq!(quality.format, wgpu::TextureFormat::Rgba8Unorm);
            assert_eq!(quality.samples, 1);
            let expected = control.readback(reference).unwrap();
            assert!(expected.iter().any(|pixel| pixel[3] > 0.));
            let area = |targets: &[wgpu::Texture]| {
                targets
                    .iter()
                    .map(|target| {
                        let size = target.size();
                        u64::from(size.width) * u64::from(size.height)
                    })
                    .sum::<u64>()
            };
            let mut hits = 0;
            let mut planned_hits = 0;
            let mut effects = Vec::new();
            for iteration in 0..4 {
                effects.clear();
                crate::profiling::drain_effects(&mut effects);
                effects.clear();
                let ((prepared, targets), inputs) = collect(|| {
                    crate::gpu::targets::pass_trace::collect(|| subject.prepare(&frame).unwrap())
                });
                assert_eq!(prepared.quality(), quality);
                let input_hits: usize = inputs.iter().map(|input| input.inputs).sum();
                planned_hits += input_hits;
                crate::profiling::drain_effects(&mut effects);
                let cache_hits: u64 = effects.iter().map(|effect| effect.hits).sum();
                hits += cache_hits;
                if iteration == 0 {
                    assert!(
                        area(&targets) < area(&reference_targets),
                        "intermediate backing must shrink: depth={depth} transform={transform:?} {} -> {}",
                        area(&reference_targets),
                        area(&targets)
                    );
                }
                let actual = subject.readback(prepared).unwrap();
                assert!(subject.usage().targets <= 10 << 20);
                assert_eq!(
                    bytemuck::cast_slice::<[f32; 4], u8>(&actual),
                    bytemuck::cast_slice::<[f32; 4], u8>(&expected),
                    "original pixels and blur halo: depth={depth} iteration={iteration} transform={transform:?}"
                );
                eprintln!(
                    "intermediate_crop depth={depth} iteration={iteration} transform={transform:?} target_pixels={}->{} inputs={input_hits} cache_hits={cache_hits} changed_bytes=0",
                    area(&reference_targets),
                    area(&targets)
                );
            }
            if !complex_source {
                assert!(
                    hits > 0,
                    "cropped image content must remain eligible for source reuse"
                );
                assert!(
                    planned_hits > 0,
                    "physical cropped sources must be selected before allocating their raw writer"
                );
            }
        }
    }
}
