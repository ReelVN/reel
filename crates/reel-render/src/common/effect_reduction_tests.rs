//! Counter bit patterns, bounded policy and complete shader admission regressions.
use super::*;

#[test]
fn blocks_reproduce_all_original_counter_bits_and_keep_short_or_complex_loops() {
    for (first, step, count, width) in [
        (0, 1, 32, 4),
        (-16, 1, 34, 4),
        (-16, 1, 35, 4),
        (-30, 1, 61, 4),
        (30, -1, 61, 4),
        (-16_777_216, 2, 33, 4),
        (16_777_184, 1, 33, 1),
        (-8, 1, 17, 1),
    ] {
        let offsets: Vec<_> = (0..count).map(|i| (first + i * step) as f32).collect();
        let program = sampling_offsets(&offsets);
        let plan = reduction::Reduction::find(&program, program.output()).unwrap();
        assert_eq!(plan.block_width(&program), width);
        for language in [Language::Cg, Language::Wgsl] {
            let fragment = compile_fragment(&program, language, 1 << 20).unwrap();
            assert_eq!(
                fragment.source.matches(" = reel_source_sample(").count(),
                width + count as usize % width
            );
            #[cfg(feature = "wgpu")]
            if language == Language::Wgsl {
                let module = wgpu::naga::front::wgsl::parse_str(&fragment.source).unwrap();
                wgpu::naga::valid::Validator::new(
                    wgpu::naga::valid::ValidationFlags::all(),
                    wgpu::naga::valid::Capabilities::all(),
                )
                .validate(&module)
                .unwrap();
            }
        }
        if width == 4 {
            let mut actual = Vec::new();
            let mut counter = plan.first as f32;
            for _ in 0..plan.iterations() / width {
                for _ in 0..width {
                    actual.push(counter);
                    counter += plan.step as f32;
                }
            }
            for _ in 0..plan.iterations() % width {
                actual.push(counter);
                counter += plan.step as f32;
            }
            assert_eq!(
                actual.iter().map(|n| n.to_bits()).collect::<Vec<_>>(),
                offsets.iter().map(|n| n.to_bits()).collect::<Vec<_>>()
            );
            assert_eq!(counter.to_bits(), (plan.end as f32).to_bits());
        }
    }
    let offsets: Vec<_> = (-30..=30).map(|i| i as f32).collect();
    let complex = sampling_offsets_with_complexity(&offsets, true);
    let plan = reduction::Reduction::find(&complex, complex.output()).unwrap();
    assert_eq!(plan.block_width(&complex), 1);
    for language in [Language::Cg, Language::Wgsl] {
        let generated = generate(&complex, language, 1 << 20).unwrap();
        assert_eq!(
            generated.function.matches(" = reel_source_sample(").count(),
            1
        );
    }
}

#[test]
fn loop_blocks_preserve_body_and_complete_wrapper_source_admission() {
    let program = sampling_program(30);
    for language in [Language::Cg, Language::Wgsl] {
        let baseline = generate_inner(&program, language, 1 << 20, false).unwrap();
        let blocked = generate_inner(&program, language, 1 << 20, true).unwrap();
        assert!(blocked.function.len() > baseline.function.len());
        let admitted = generate(
            &program,
            language,
            baseline.function.len() + baseline.declarations.len(),
        )
        .unwrap();
        assert_eq!(admitted.function, baseline.function);
        assert_eq!(admitted.parameters, baseline.parameters);
        for (tail, coverage) in [
            (OutputTail::Ordinary, FragmentCoverage::Local),
            (OutputTail::AlphaCoverage, FragmentCoverage::Local),
            (OutputTail::Ordinary, FragmentCoverage::FullTarget),
        ] {
            let generated = generate_inner(&program, language, 1 << 20, false).unwrap();
            let original = match language {
                Language::Cg => cg::compile(&program, generated, tail, coverage, 1 << 20),
                Language::Wgsl => wgsl::compile(&program, generated, tail, coverage, 1 << 20),
            }
            .unwrap();
            if coverage == FragmentCoverage::Local {
                assert!(
                    blocked.function.len() + blocked.declarations.len() < original.source.len(),
                    "the wrapper, rather than the function, exercises fallback"
                );
            }
            let admitted =
                compile_fragment_variant(&program, language, tail, coverage, original.source.len())
                    .unwrap();
            assert_eq!(admitted.source, original.source);
            assert_eq!(admitted.parameters, original.parameters);
            assert_eq!(admitted.source_sampler, original.source_sampler);
        }
    }
}
