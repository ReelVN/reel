//! Full-attachment variants preserve the author body and sampler/parameter ABI.
use super::*;
fn coverage_programs() -> [Arc<EffectProgram>; 4] {
    let mut scalar = Graph::default();
    let scalar_output = scalar.float(0.5);
    let scalar = scalar.finish(scalar_output);
    let mut solid = Graph::default();
    let lane = solid.float(0.5);
    let output = solid.node(EffectType::Vec4, EffectOp::Vec4([lane; 4]));
    [
        sampled_program(),
        sampling_program(30),
        scalar,
        solid.finish(output),
    ]
}

#[test]
fn full_target_changes_only_wrapper_discards_and_retains_local_entry_points() {
    for language in [Language::Cg, Language::Wgsl] {
        for program in coverage_programs() {
            let local = compile_fragment(&program, language, 1 << 20).unwrap();
            let explicit = compile_fragment_variant(
                &program,
                language,
                OutputTail::Ordinary,
                FragmentCoverage::Local,
                1 << 20,
            )
            .unwrap();
            assert_eq!(local.source, explicit.source);
            let full = compile_fragment_variant(
                &program,
                language,
                OutputTail::Ordinary,
                FragmentCoverage::FullTarget,
                1 << 20,
            )
            .unwrap();
            let normalized = local
                .source
                .replace(cg::PIXEL_KILL, cg::PIXEL_DISCARD)
                .replace(cg::UV_KILL, cg::UV_DISCARD);
            let stripped = normalized
                .lines()
                .filter(|line| !line.contains("if (any(fragment_position"))
                .map(|line| {
                    format!(
                        "{}\n",
                        line.replace(
                            "discard;",
                            if language == Language::Cg {
                                "return float4(0.0);"
                            } else {
                                "return vec4<f32>(0.0);"
                            }
                        )
                    )
                })
                .collect::<String>();
            assert_eq!(full.source, stripped);
            assert_eq!(full.parameters, local.parameters);
            assert_eq!(full.output_type, local.output_type);
            assert_eq!(full.source_sampler, local.source_sampler);
            assert!(!full.source.contains("discard;"));
            #[cfg(feature = "wgpu")]
            if language == Language::Wgsl {
                let module = wgpu::naga::front::wgsl::parse_str(&full.source).unwrap();
                wgpu::naga::valid::Validator::new(
                    wgpu::naga::valid::ValidationFlags::all(),
                    wgpu::naga::valid::Capabilities::all(),
                )
                .validate(&module)
                .unwrap();
            }
            assert!(matches!(
                compile_fragment_variant(
                    &program,
                    language,
                    OutputTail::AlphaCoverage,
                    FragmentCoverage::FullTarget,
                    1 << 20
                ),
                Err(GenerateError::UnsupportedTail)
            ));
        }
    }
}

#[test]
#[ignore = "requires Sony SDK offline compiler; no device or readback"]
fn full_target_cg_fragments_compile_the_actual_entry_points() {
    let compiler = gxp_compiler::Compiler::new().unwrap();
    for program in coverage_programs() {
        let full = compile_fragment_variant(
            &program,
            Language::Cg,
            OutputTail::Ordinary,
            FragmentCoverage::FullTarget,
            16384,
        )
        .unwrap();
        let binary = compiler
            .compile(gxp_compiler::Stage::Fragment, &full.source)
            .unwrap();
        assert_eq!(&binary[..4], b"GXP\0");
    }
}
