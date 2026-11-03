//! Completed target bindings yield under physical pressure while buffers stay warm.
use super::*;
use std::sync::atomic::Ordering;

#[test]
#[ignore = "actual GPU retired binding reclamation, exact budget and uniform reuse"]
fn changing_rasters_reclaim_idle_bindings_without_replaying_the_frame() {
    let (old_frame, _old_recording) = transformed_chain([64, 48], Affine::IDENTITY, true);
    let (frame, _recording) = transformed_chain([96, 64], Affine::IDENTITY, true);
    let mut gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        crate::gpu::DeviceGeneration(1),
        crate::gpu::GpuLimits::default(),
    ))
    .unwrap();
    let quality = gpu.quality([96, 64]).unwrap();
    let (_, required, _) = crate::common::offscreen::Plan::choose(
        &frame,
        None,
        [96, 64],
        false,
        quality.samples,
        gpu.device.limits().max_texture_dimension_2d,
        &Default::default(),
        gpu.limits.target_bytes,
        &|size| crate::gpu::targets::target_bytes(size, quality),
        crate::common::target_plan::MaskExecution::Inline,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    let mut control = renderer(64 << 20);
    let expected = control.prepare(&frame).unwrap();
    assert_eq!(expected.quality(), quality);
    let expected = control.readback(expected).unwrap();
    gpu.limits.target_bytes = required;
    let mut subject = Renderer::new(gpu);
    let old = subject.prepare(&old_frame).unwrap();
    assert_eq!(old.quality(), quality);
    subject.readback(old).unwrap();
    assert!(subject.effect_uniforms.test_len() > 0);
    let counts = &subject.gpu.upload_creations;
    let buffers = counts.effect_uniform_buffers.load(Ordering::Relaxed);
    counts.geometry_retries.store(0, Ordering::Relaxed);
    #[cfg(feature = "profiling")]
    crate::profiling::drain_effect_input_plans();
    let prepared = subject.prepare(&frame).unwrap();
    assert_eq!(prepared.quality(), quality);
    assert!(prepared.budget_scope.targets.usage().bytes <= required);
    assert!(subject.usage().targets <= required);
    assert_eq!(counts.geometry_retries.load(Ordering::Relaxed), 0);
    assert_eq!(
        counts.effect_uniform_buffers.load(Ordering::Relaxed),
        buffers
    );
    #[cfg(feature = "profiling")]
    {
        let plans = crate::profiling::drain_effect_input_plans();
        assert!(plans.target_pool_pressure > 0);
        assert!(plans.idle_target_bindings_released > 0);
        assert_eq!(plans.prepare_retries, 0);
        eprintln!("effect retired binding recovery budget={required} {plans:?}");
    }
    assert_eq!(subject.readback(prepared).unwrap(), expected);
}
