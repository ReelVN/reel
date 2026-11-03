//! Counter bit patterns, bounded policy and complete shader admission regressions.
use super::*;

#[test]
fn source_limits_retain_four_tap_blocks_before_falling_back_to_single_taps() {
    let program = sampling_program(30);
    for language in [Language::Cg, Language::Wgsl] {
        let four = generate_inner(&program, language, 1 << 20, 4).unwrap();
        let limit = four.function.len() + four.declarations.len();
        assert_eq!(
            generate(&program, language, limit).unwrap().function,
            four.function
        );
        for tail in [OutputTail::Ordinary, OutputTail::AlphaCoverage] {
            let four = generate_inner(&program, language, 1 << 20, 4).unwrap();
            let four = match language {
                Language::Cg => cg::compile(&program, four, tail, FragmentCoverage::Local, 1 << 20),
                Language::Wgsl => {
                    wgsl::compile(&program, four, tail, FragmentCoverage::Local, 1 << 20)
                }
            }
            .unwrap();
            let admitted =
                compile_fragment_with_tail(&program, language, tail, four.source.len()).unwrap();
            assert_eq!(admitted.source, four.source);
            assert_eq!(admitted.parameters, four.parameters);
        }
    }
}

#[test]
fn cg_parallel_clip_keeps_predicates_and_sampling_body_with_bounded_admission() {
    for count in [3, 17, 31, 32, 61] {
        let offsets: Vec<_> = (0..count).map(|i| i as f32).collect();
        let program = sampling_offsets(&offsets);
        for tail in [OutputTail::Ordinary, OutputTail::AlphaCoverage] {
            let local = compile_fragment_with_tail(&program, Language::Cg, tail, 1 << 20).unwrap();
            assert!(local.source.contains(cg::PIXEL_KILL));
            assert!(local.source.contains(cg::UV_KILL));
            let generated = generate_inner(&program, Language::Cg, 1 << 20, 5).unwrap();
            assert!(local.source.contains(&generated.function));
            let admitted =
                compile_fragment_with_tail(&program, Language::Cg, tail, local.source.len())
                    .unwrap();
            assert_eq!(admitted.source, local.source);
        }
        let complex = sampling_offsets_with_complexity(&offsets, true);
        let local = compile_fragment(&complex, Language::Cg, 1 << 20).unwrap();
        assert!(local.source.contains(cg::PIXEL_KILL));
        let full = compile_fragment_variant(
            &program,
            Language::Cg,
            OutputTail::Ordinary,
            FragmentCoverage::FullTarget,
            1 << 20,
        )
        .unwrap();
        assert!(!full.source.contains("clip("));
    }
}

#[test]
fn blocks_reproduce_all_original_counter_bits_and_keep_short_or_complex_loops() {
    for (first, step, count, width) in [
        (0, 1, 32, 4),
        (-16, 1, 34, 4),
        (-16, 1, 35, 5),
        (-20, 1, 41, 5),
        (-25, 1, 51, 5),
        (-30, 1, 61, 5),
        (30, -1, 61, 5),
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
        if width > 1 {
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
        let baseline = generate_inner(&program, language, 1 << 20, 1).unwrap();
        let blocked = generate_inner(&program, language, 1 << 20, 4).unwrap();
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
            let generated = generate_inner(&program, language, 1 << 20, 1).unwrap();
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
