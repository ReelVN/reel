//! Normal frame compilation/submission for an existing distribution. No capture path.
use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
use reel_render::{
    effect::*,
    gpu::{DeviceGeneration, Gpu, GpuLimits, Renderer},
};
use reel_ui::{
    frame::{BuildLimits, FrameBuild, FrameInfo, FrameTransition, StepBudget},
    resources::{EffectShaderDescriptor, EffectShaderFormat, ResourceLease, ResourceSnapshot},
};
use std::path::PathBuf;

pub(super) struct Probe {
    renderer: Renderer,
    pool: BudgetPool,
    frames: u64,
}
impl Probe {
    pub(super) fn new(
        products: &reel_engine::aside::source::EffectProducts,
        overlay: &mut Option<PathBuf>,
    ) -> Self {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/source-crop-host")
            .join(std::process::id().to_string());
        assert!(
            overlay.is_none(),
            "host render probe retains the distribution's archive textures"
        );
        for definition in products.definitions() {
            let fragment = compile_fragment(&definition.program, Language::Wgsl, 4 << 20).unwrap();
            let mut raw = fragment.source.as_bytes().to_vec();
            let coverage = OutputTail::supports_coverage(
                fragment.output_type,
                definition.program.textures().len() as u32,
                fragment.source_sampler,
            )
            .then(|| {
                let offset = raw.len() as u32;
                raw.extend_from_slice(
                    compile_fragment_with_tail(
                        &definition.program,
                        Language::Wgsl,
                        OutputTail::AlphaCoverage,
                        4 << 20,
                    )
                    .unwrap()
                    .source
                    .as_bytes(),
                );
                offset
            });
            let full = raw.len() as u32;
            raw.extend_from_slice(
                compile_fragment_variant(
                    &definition.program,
                    Language::Wgsl,
                    OutputTail::Ordinary,
                    FragmentCoverage::FullTarget,
                    4 << 20,
                )
                .unwrap()
                .source
                .as_bytes(),
            );
            let descriptor = EffectShaderDescriptor::with_textures(
                definition.id,
                EffectShaderFormat::Wgsl,
                fragment
                    .parameters
                    .slots
                    .iter()
                    .map(|slot| slot.ty)
                    .collect(),
                definition.program.textures().len() as u32,
                fragment.source_sampler,
                fragment.output_type,
                &raw,
            )
            .unwrap();
            let descriptor = if let Some(offset) = coverage {
                descriptor.with_coverage_variant(offset, &raw).unwrap()
            } else {
                descriptor
            };
            let descriptor = descriptor
                .with_full_target_variant(full, &raw)
                .unwrap()
                .with_source_footprint(source_footprint(&definition.program), &raw)
                .unwrap();
            let path = directory.join(EffectShaderFormat::Wgsl.asset_path(definition.id));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, descriptor.encode_payload(&raw).unwrap()).unwrap();
        }
        *overlay = Some(directory);
        let gpu = pollster::block_on(Gpu::request(
            &wgpu::Instance::default(),
            None,
            DeviceGeneration(1),
            GpuLimits {
                target_bytes: 90 << 20,
                ..Default::default()
            },
        ))
        .unwrap();
        Self {
            renderer: Renderer::new(gpu),
            pool: BudgetPool::new(
                BudgetCategory::Temporary,
                QueueLimits {
                    max_requests: 4096,
                    max_bytes: 128 << 20,
                },
            ),
            frames: 0,
        }
    }

    pub(super) fn stage(
        &mut self,
        source: reel_ui::bridge::RecordedSource,
        resources: &[ResourceLease],
        transitions: &[FrameTransition],
    ) {
        let build_started = std::time::Instant::now();
        let snapshot =
            ResourceSnapshot::capture(resources.iter().cloned().map(Ok), &self.pool).unwrap();
        let mut build = FrameBuild::new(
            source,
            snapshot,
            FrameInfo {
                logical_size: [960., 544.],
                physical_size: [960, 544],
            },
            BuildLimits { max_bytes: 8 << 20 },
            &self.pool,
        )
        .unwrap();
        build.set_transitions(transitions.to_vec()).unwrap();
        let mut step = StepBudget {
            max_commands: 1024,
            max_units: 65536,
            max_bytes: 1 << 20,
        };
        loop {
            let progress = build.step(step).unwrap();
            if progress.complete {
                break;
            }
            step.max_units = step.max_units.max(progress.minimum_units);
            step.max_bytes = step.max_bytes.max(progress.minimum_bytes);
        }
        let frame = build.finish().unwrap();
        let build_ns = build_started.elapsed().as_nanos();
        let started = std::time::Instant::now();
        let prepared = loop {
            match self.renderer.prepare(&frame) {
                Ok(prepared) => break prepared,
                Err(
                    reel_render::gpu::GpuError::Preparing
                    | reel_render::gpu::GpuError::Busy
                    | reel_render::gpu::GpuError::Pressure { .. },
                ) if started.elapsed().as_secs() < 10 => {
                    self.renderer.poll().unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(error) => panic!("ordinary host prepare: {error:?}"),
            }
        };
        let prepare_ns = started.elapsed().as_nanos();
        let quality = prepared.quality();
        let submitted = std::time::Instant::now();
        self.renderer.submit(prepared).unwrap();
        let submit_ns = submitted.elapsed().as_nanos();
        let waiting = std::time::Instant::now();
        self.renderer
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        self.renderer.poll().unwrap();
        let completion_ns = waiting.elapsed().as_nanos();
        self.frames += 1;
        #[cfg(feature = "profiling")]
        {
            let mut effects = Vec::new();
            let dropped = reel_render::profiling::drain_effects(&mut effects);
            eprintln!(
                "host_frame={} quality={quality:?} build_ns={build_ns} prepare_ns={prepare_ns} submit_ns={submit_ns} completion_ns={completion_ns} commands={} effect_draws={} effect_hits={} effect_early_hits={} effect_scissor_pixels={} crop_draws={} crop_original_pixels={} effect_source_storage_pixels={} effect_output_storage_pixels={} dropped={dropped:?} usage={:?}",
                self.frames,
                frame.commands().len(),
                effects.iter().map(|e| e.draws).sum::<u64>(),
                effects.iter().map(|e| e.hits).sum::<u64>(),
                effects.iter().map(|e| e.early_hits).sum::<u64>(),
                effects.iter().map(|e| e.scissor_pixels).sum::<u64>(),
                effects.iter().map(|e| e.crop_source_draws).sum::<u64>(),
                effects
                    .iter()
                    .map(|e| e.crop_source_original_storage_pixels)
                    .sum::<u64>(),
                effects.iter().map(|e| e.source_storage_pixels).sum::<u64>(),
                effects.iter().map(|e| e.output_storage_pixels).sum::<u64>(),
                self.renderer.usage()
            );
            eprintln!(
                "host_paint_domain={:?}",
                reel_render::profiling::drain_paint_domain()
            );
            eprintln!(
                "host_source_crop={:?}",
                reel_render::profiling::drain_source_crop_plans()
            );
            eprintln!(
                "host_effect_inputs frame={} {:?}",
                self.frames,
                reel_render::profiling::drain_effect_input_plans()
            );
            for effect in effects.iter().filter(|effect| effect.variant == 3) {
                eprint!(
                    "host_effect_cache frame={} definition={:02x?} hits={} early_hits={}",
                    self.frames, effect.definition, effect.hits, effect.early_hits
                );
                for (label, count) in reel_render::profiling::CACHE_MISS_LABELS
                    .iter()
                    .zip(effect.cache_misses)
                {
                    if count != 0 {
                        eprint!(" {label}={count}");
                    }
                }
                eprintln!();
            }
        }
    }
}
