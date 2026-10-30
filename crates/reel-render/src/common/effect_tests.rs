use super::*;
use reel_ui::stage::effect::{EffectNode, EffectParameter};
use std::sync::Arc;

#[derive(Default)]
struct Graph {
    parameters: Vec<EffectParameter>,
    nodes: Vec<EffectNode>,
}

impl Graph {
    fn node(&mut self, ty: EffectType, op: EffectOp) -> EffectNodeId {
        let id = EffectNodeId(self.nodes.len() as u32);
        self.nodes.push(EffectNode { ty, op });
        id
    }

    fn parameter(&mut self, ty: EffectType) -> EffectNodeId {
        let index = self.parameters.len() as u32;
        self.parameters.push(EffectParameter {
            name: format!("value{index}").into(),
            ty,
            default: None,
        });
        self.node(ty, EffectOp::Parameter(index))
    }

    fn float(&mut self, value: f32) -> EffectNodeId {
        self.node(
            EffectType::Float,
            EffectOp::Constant(EffectValue::Float(value.to_bits())),
        )
    }

    fn finish(self, output: EffectNodeId) -> Arc<EffectProgram> {
        EffectProgram::new(self.parameters, self.nodes, output).unwrap()
    }
}

#[cfg(feature = "wgpu")]
fn wgsl_fragment(generated: &GeneratedEffect) -> String {
    let binding = if generated.parameters.slots.is_empty() {
        ""
    } else {
        "@group(0) @binding(0) var<uniform> reel_parameters: ReelEffectParameters;"
    };
    let result = if generated.output_type == EffectType::Float {
        "vec4<f32>(reel_effect(uv, uv * size, size))"
    } else {
        "reel_effect(uv, uv * size, size)"
    };
    format!(
        "{}\n{binding}\n@group(0) @binding(1) var source: texture_2d<f32>;\n@group(0) @binding(2) var source_sampler: sampler;\nfn reel_source_sample(uv: vec2<f32>) -> vec4<f32> {{ return textureSampleLevel(source, source_sampler, uv, 0.0); }}\n{}\n@fragment fn main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {{\n    let size = vec2<f32>(960.0, 544.0);\n    return {result};\n}}\n",
        generated.declarations, generated.function
    )
}

/// One ordinary expression covers source reads, logical coordinates, live values,
/// a shared dependency, and a lazy branch with branch-local sampling.
fn sampled_program() -> Arc<EffectProgram> {
    let mut g = Graph::default();
    let enabled = g.parameter(EffectType::Bool);
    let progress = g.parameter(EffectType::Float);
    let uv = g.node(EffectType::Vec2, EffectOp::Input(EffectInput::Uv));
    let pos = g.node(EffectType::Vec2, EffectOp::Input(EffectInput::Position));
    let size = g.node(EffectType::Vec2, EffectOp::Input(EffectInput::Size));
    let logical_uv = g.node(
        EffectType::Vec2,
        EffectOp::Binary {
            op: EffectBinary::Divide,
            left: pos,
            right: size,
        },
    );
    let sample = g.node(EffectType::Vec4, EffectOp::SourceSample { uv: logical_uv });
    let alpha = g.node(
        EffectType::Float,
        EffectOp::Extract {
            value: sample,
            lane: 3,
        },
    );
    let zero = g.float(0.0);
    let one = g.float(1.0);
    let coverage = g.node(
        EffectType::Float,
        EffectOp::Ternary {
            op: EffectTernary::Smoothstep,
            a: zero,
            b: progress,
            c: alpha,
        },
    );
    let rgba = g.node(
        EffectType::Vec4,
        EffectOp::Vec4([coverage, coverage, zero, one]),
    );
    let other = g.node(EffectType::Vec4, EffectOp::SourceSample { uv });
    let output = g.node(
        EffectType::Vec4,
        EffectOp::Select {
            condition: enabled,
            when_true: rgba,
            when_false: other,
        },
    );
    g.finish(output)
}

/// Independent RGBA accumulators share each fetched vector. Scheduling one
/// output channel at a time would keep all 61 vectors live across the others.
fn sampling_program(radius: i32) -> Arc<EffectProgram> {
    sampling_offsets(&(-radius..=radius).map(|n| n as f32).collect::<Vec<_>>())
}
#[test]
fn source_footprints_bound_every_original_tap_and_reject_nonlinear_coordinates() {
    use reel_ui::resources::EffectSourceFootprint;
    assert_eq!(
        source_footprint(&sampling_program(30)),
        Some(EffectSourceFootprint {
            logical_halo: [0, 30, 0, 30],
        })
    );
    assert_eq!(
        source_footprint(&sampling_program(1)),
        Some(EffectSourceFootprint {
            logical_halo: [0, 1, 0, 1],
        })
    );
    let asymmetric = sampling_offsets(&[-2.25, 0., 7.5]);
    assert_eq!(
        source_footprint(&asymmetric),
        Some(EffectSourceFootprint {
            logical_halo: [0, 3, 0, 8],
        })
    );
    assert!(source_footprint(&sampling_offsets_with_complexity(&[-30., 30.], true)).is_none());
    let mut g = Graph::default();
    let uv = g.node(EffectType::Vec2, EffectOp::Input(EffectInput::Uv));
    let sample = g.node(EffectType::Vec4, EffectOp::SourceSample { uv });
    assert_eq!(
        source_footprint(&g.finish(sample)),
        Some(EffectSourceFootprint {
            logical_halo: [0; 4]
        })
    );
}

fn sampling_offsets(offsets: &[f32]) -> Arc<EffectProgram> {
    sampling_offsets_with_complexity(offsets, false)
}
fn sampling_offsets_with_complexity(offsets: &[f32], complex: bool) -> Arc<EffectProgram> {
    let mut g = Graph::default();
    let uv = g.node(EffectType::Vec2, EffectOp::Input(EffectInput::Uv));
    let x = g.node(EffectType::Float, EffectOp::Extract { value: uv, lane: 0 });
    let y = g.node(EffectType::Float, EffectOp::Extract { value: uv, lane: 1 });
    let size = g.node(EffectType::Vec2, EffectOp::Input(EffectInput::Size));
    let height = g.node(
        EffectType::Float,
        EffectOp::Extract {
            value: size,
            lane: 1,
        },
    );
    let zero = g.float(0.0);
    let mut channels = [zero; 4];
    for &tap in offsets {
        let offset = g.float(tap);
        let offset = g.node(
            EffectType::Float,
            EffectOp::Binary {
                op: EffectBinary::Divide,
                left: offset,
                right: height,
            },
        );
        let local_y = g.node(
            EffectType::Float,
            EffectOp::Binary {
                op: EffectBinary::Add,
                left: y,
                right: offset,
            },
        );
        let local_y = if complex {
            g.node(
                EffectType::Float,
                EffectOp::Unary {
                    op: EffectUnary::Sin,
                    value: local_y,
                },
            )
        } else {
            local_y
        };
        let at = g.node(EffectType::Vec2, EffectOp::Vec2([x, local_y]));
        let sample = g.node(EffectType::Vec4, EffectOp::SourceSample { uv: at });
        for (lane, channel) in channels.iter_mut().enumerate() {
            let value = g.node(
                EffectType::Float,
                EffectOp::Extract {
                    value: sample,
                    lane: lane as u8,
                },
            );
            *channel = g.node(
                EffectType::Float,
                EffectOp::Binary {
                    op: EffectBinary::Add,
                    left: *channel,
                    right: value,
                },
            );
        }
    }
    let divisor = g.float(offsets.len() as f32);
    for channel in &mut channels {
        *channel = g.node(
            EffectType::Float,
            EffectOp::Binary {
                op: EffectBinary::Divide,
                left: *channel,
                right: divisor,
            },
        );
    }
    let output = g.node(EffectType::Vec4, EffectOp::Vec4(channels));
    g.finish(output)
}

#[test]
fn parallel_channels_consume_each_sample_before_the_next_fetch() {
    for language in [Language::Wgsl, Language::Cg] {
        let generated = generate(&sampling_program(5), language, 1 << 20).unwrap();
        let mut taps = generated.function.split(" = reel_source_sample(");
        taps.next().unwrap();
        let taps = taps.collect::<Vec<_>>();
        assert_eq!(taps.len(), 11);
        for tap in taps {
            for lane in [".x;", ".y;", ".z;", ".w;"] {
                assert!(
                    tap.contains(lane),
                    "all channels consume the current sample in {language:?}"
                );
            }
        }
    }
}

#[test]
fn large_proved_reduction_keeps_integer_constants_and_final_scaling() {
    let program = sampling_program(30);
    let plan = reduction::Reduction::find(&program, program.output()).unwrap();
    assert_eq!((plan.first, plan.end, plan.step), (-30, 31, 1));
    for language in [Language::Wgsl, Language::Cg] {
        let fragment = compile_fragment(&program, language, 1 << 20).unwrap();
        let function = generate(&program, language, 1 << 20).unwrap().function;
        assert_eq!(function.matches(" = reel_source_sample(").count(), 6);
        assert!(function.contains("!= 30;"));
        assert!(function.contains("+= 1;"));
        assert!(function.contains("= -30;"));
        assert!(function.find(" / ").unwrap() < function.rfind(" / ").unwrap());
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
        if language == Language::Cg {
            assert!(fragment.source.contains("#pragma loop (unroll: never)"));
            assert!(fragment.source.contains("tex2Dlod(reel_source"));
        }
    }
}

#[test]
fn sampling_counter_preserves_literals_at_float_precision_boundaries() {
    for (first, step, count, float_counter) in [
        (-30, 1, 61, true),
        (30, -1, 61, true),
        (16_777_200, 1, 17, false),
        (-16_777_200, -1, 17, false),
        (-16_777_216, 2, 17, true),
    ] {
        let offsets: Vec<_> = (0..count).map(|i| (first + step * i) as f32).collect();
        let program = sampling_offsets(&offsets);
        let plan = reduction::Reduction::find(&program, program.output()).unwrap();
        assert_eq!(plan.float_counter, float_counter);
        if float_counter {
            let mut counter = plan.first as f32;
            for offset in offsets {
                assert_ne!(counter, plan.end as f32);
                assert_eq!(counter.to_bits(), offset.to_bits());
                counter += plan.step as f32;
            }
            assert_eq!(counter, plan.end as f32);
        }
        for language in [Language::Wgsl, Language::Cg] {
            let fragment = compile_fragment(&program, language, 1 << 20).unwrap();
            let expected = match (language, float_counter) {
                (Language::Cg, true) => "for (float reel_i",
                (Language::Cg, false) => "for (int reel_i",
                (Language::Wgsl, true) => ": f32 =",
                (Language::Wgsl, false) => ": i32 =",
            };
            assert!(fragment.source.contains(expected));
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
    }
}

#[test]
fn sampling_reduction_rejects_noninteger_sequences_and_conflicting_literal_aliases() {
    let program = sampling_program(30);
    let rebuild = |nodes| EffectProgram::new(vec![], nodes, program.output()).unwrap();
    let mut nodes = program.nodes().to_vec();
    let varying = nodes.iter_mut().find(|node| matches!(node.op, EffectOp::Constant(EffectValue::Float(bits)) if bits == (-29f32).to_bits())).unwrap();
    varying.op = EffectOp::Constant(EffectValue::Float((-29.25f32).to_bits()));
    let changed = rebuild(nodes);
    assert!(reduction::Reduction::find(&changed, changed.output()).is_none());

    // The first tap's -30 literal is also the fixed X coordinate. Replacing all
    // references to that ID with the loop counter would incorrectly move X.
    let mut nodes = program.nodes().to_vec();
    let fixed = EffectNodeId(nodes.iter().position(|node| matches!(node.op, EffectOp::Constant(EffectValue::Float(bits)) if bits == (-30f32).to_bits())).unwrap() as u32);
    for node in &mut nodes {
        if let EffectOp::Vec2([_, y]) = node.op {
            node.op = EffectOp::Vec2([fixed, y]);
        }
    }
    let aliased = rebuild(nodes);
    assert!(reduction::Reduction::find(&aliased, aliased.output()).is_none());
}

#[test]
fn reduction_iteration_recomputes_a_first_tap_also_used_outside_the_loop() {
    let program = sampling_program(30);
    let mut nodes = program.nodes().to_vec();
    let first = EffectNodeId(
        nodes
            .iter()
            .position(|node| matches!(node.op, EffectOp::SourceSample { .. }))
            .unwrap() as u32,
    );
    let output = EffectNodeId(nodes.len() as u32);
    nodes.push(EffectNode {
        ty: EffectType::Vec4,
        op: EffectOp::Binary {
            op: EffectBinary::Add,
            left: program.output(),
            right: first,
        },
    });
    let program = EffectProgram::new(vec![], nodes, output).unwrap();
    for language in [Language::Wgsl, Language::Cg] {
        let generated = generate(&program, language, 1 << 20).unwrap();
        let (before, body) = generated.function.split_once("for (").unwrap();
        assert_eq!(before.matches(" = reel_source_sample(").count(), 1);
        assert_eq!(body.matches(" = reel_source_sample(").count(), 1);
    }
}

#[test]
fn parameter_layout_matches_all_uniform_abis() {
    let mut g = Graph::default();
    for ty in [
        EffectType::Bool,
        EffectType::Float,
        EffectType::Vec2,
        EffectType::Vec3,
        EffectType::Vec4,
    ] {
        g.parameter(ty);
    }
    let output = g.float(1.0);
    let program = g.finish(output);
    let wgsl = generate(&program, Language::Wgsl, 16384).unwrap();
    let cg = generate(&program, Language::Cg, 16384).unwrap();
    assert_eq!(wgsl.parameters, cg.parameters);
    for index in 0..5 {
        assert!(
            cg.declarations
                .contains(&format!("uniform float4 reel_parameter_{index};"))
        );
    }
    assert_eq!(wgsl.parameters.byte_len(), 80);
    assert_eq!(
        wgsl.parameters
            .slots
            .iter()
            .map(|s| s.byte_offset)
            .collect::<Vec<_>>(),
        [0, 16, 32, 48, 64]
    );
    let mut values = [
        EffectValue::Bool(true),
        EffectValue::Float((-0.0_f32).to_bits()),
        EffectValue::Vec2([1.0_f32.to_bits(), 2.0_f32.to_bits()]),
        EffectValue::Vec3([3.0_f32.to_bits(); 3]),
        EffectValue::Vec4([4.0_f32.to_bits(); 4]),
    ];
    let packed = wgsl.parameters.pack(&values).unwrap();
    assert_eq!(packed[0], [1., 0., 0., 0.]);
    assert_eq!(packed[1][0].to_bits(), (-0.0_f32).to_bits());
    assert_eq!(packed[2], [1., 2., 0., 0.]);
    assert_eq!(packed[3], [3., 3., 3., 0.]);
    assert_eq!(packed[4], [4.; 4]);
    assert_eq!(wgsl.parameters.pack(&[]), Err(ParameterError::Count));
    values[0] = EffectValue::Float(0);
    assert_eq!(
        wgsl.parameters.pack(&values),
        Err(ParameterError::Type { index: 0 })
    );
    values[0] = EffectValue::Bool(false);
    values[1] = EffectValue::Float(f32::NAN.to_bits());
    assert_eq!(
        wgsl.parameters.pack(&values),
        Err(ParameterError::NonFinite { index: 1 })
    );
}

#[test]
fn emits_only_root_dependencies_and_keeps_branch_work_local() {
    for language in [Language::Wgsl, Language::Cg] {
        let generated = generate(&sampled_program(), language, 16384).unwrap();
        assert!(generated.samples_source);
        let function = generated.function.split("reel_effect(").nth(1).unwrap();
        let (before, branches) = function.split_once("if (").unwrap();
        assert!(!before.contains("reel_source_sample"));
        let (accept, reject) = branches.split_once("} else {").unwrap();
        assert_eq!(accept.matches("reel_source_sample(").count(), 1);
        assert_eq!(reject.matches("reel_source_sample(").count(), 1);
        assert_eq!(accept.matches("reel_smoothstep_f(").count(), 1);
        assert!(!reject.contains("reel_smoothstep_f("));
    }
    let mut g = Graph::default();
    let uv = g.node(EffectType::Vec2, EffectOp::Input(EffectInput::Uv));
    g.node(EffectType::Vec4, EffectOp::SourceSample { uv }); // Unreachable sample.
    let one = g.float(1.0);
    let sum = g.node(
        EffectType::Float,
        EffectOp::Binary {
            op: EffectBinary::Add,
            left: one,
            right: one,
        },
    );
    let program = g.finish(sum);
    let generated = generate(&program, Language::Wgsl, 1024).unwrap();
    assert!(!generated.samples_source);
    assert_eq!(generated.function.matches(" = 1e0;").count(), 1);
    assert!(!generated.function.contains("reel_source_sample"));
}

#[test]
fn deep_graph_and_branch_expansion_respect_source_limit() {
    let mut g = Graph::default();
    let mut value = g.float(1.0);
    for _ in 0..10000 {
        value = g.node(
            EffectType::Float,
            EffectOp::Unary {
                op: EffectUnary::Negate,
                value,
            },
        );
    }
    let program = g.finish(value);
    let generated = generate(&program, Language::Wgsl, 1_000_000).unwrap();
    assert!(generated.function.contains("reel_n10000"));
    let exact = generated.declarations.len() + generated.function.len();
    assert!(generate(&program, Language::Wgsl, exact).is_ok());
    assert!(
        matches!(generate(&program, Language::Wgsl, exact - 1), Err(GenerateError::SourceLimit { limit }) if limit == exact - 1)
    );
    assert!(matches!(
        generate(&program, Language::Wgsl, 0),
        Err(GenerateError::SourceLimit { limit: 0 })
    ));

    let mut g = Graph::default();
    let condition = g.parameter(EffectType::Bool);
    let mut value = g.float(0.5);
    for _ in 0..20 {
        value = g.node(
            EffectType::Float,
            EffectOp::Select {
                condition,
                when_true: value,
                when_false: value,
            },
        );
    }
    assert!(matches!(
        generate(&g.finish(value), Language::Wgsl, 4096),
        Err(GenerateError::SourceLimit { limit: 4096 })
    ));
}

#[test]
fn complete_fragments_keep_source_sampling_and_parameter_abi() {
    for language in [Language::Wgsl, Language::Cg] {
        let output = compile_fragment(&sampled_program(), language, 16384).unwrap();
        assert_eq!(output.parameters.byte_len(), 32);
        assert!(compile_fragment(&sampled_program(), language, output.source.len()).is_ok());
        assert!(matches!(
            compile_fragment(&sampled_program(), language, output.source.len() - 1),
            Err(GenerateError::SourceLimit { .. })
        ));
        match language {
            Language::Cg => {
                assert!(output.source.contains("uniform float4 reel_context[10]"));
                assert!(
                    output
                        .source
                        .contains("tex2Dlod(reel_source, float4(q, 0.0, 0.0))")
                );
                assert!(output.source.contains("fragment_position : WPOS"));
                assert!(!output.source.contains("1.0 - q.y"));
            }
            Language::Wgsl => {
                assert!(output.source.contains("textureSampleLevel(reel_source"));
                assert!(output.source.contains("@fragment fn fragment"));
            }
        }
    }
}

#[test]
fn scalar_coverage_inversion_follows_clamping_in_all_shader_languages() {
    let mut graph = Graph::default();
    let output = graph.float(2.0);
    let program = graph.finish(output);
    for language in [Language::Wgsl, Language::Cg] {
        let shader = compile_fragment(&program, language, 16384).unwrap();
        let clamp = shader.source.find("coverage = clamp(").unwrap();
        let invert = shader.source.find("coverage = 1.0 - coverage").unwrap();
        assert!(
            clamp < invert,
            "inverted coverage is one minus clamped output"
        );
        assert!(shader.source.contains("mode >= 2.0"));
        assert!(shader.source.contains("mode == 1.0 || mode == 2.0"));
        assert!(shader.source.contains("reel_implicit_source * coverage"));
        #[cfg(feature = "wgpu")]
        if language == Language::Wgsl {
            let module = wgpu::naga::front::wgsl::parse_str(&shader.source).unwrap();
            wgpu::naga::valid::Validator::new(
                wgpu::naga::valid::ValidationFlags::all(),
                wgpu::naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap();
        }
    }
}

#[test]
#[ignore = "requires the Sony SDK offline compiler; compiles GXP without device or readback"]
fn cg_fragments_compile_all_typed_operations() {
    let compiler = gxp_compiler::Compiler::new().expect("locate Sony SDK compiler");
    let programs = operation_programs()
        .into_iter()
        .chain(external_texture_programs())
        .chain([("wide_sampling".into(), sampling_program(30))])
        .collect::<Vec<_>>();
    for (name, program) in &programs {
        let fragment = compile_fragment(program, Language::Cg, 65536).unwrap();
        let binary = compiler
            .compile(gxp_compiler::Stage::Fragment, &fragment.source)
            .unwrap_or_else(|error| panic!("{name}: {error}\n{}", fragment.source));
        assert_eq!(&binary[..4], b"GXP\0");
        if OutputTail::supports_coverage(
            fragment.output_type,
            program.textures().len() as u32,
            fragment.source_sampler,
        ) {
            let masked =
                compile_fragment_with_tail(program, Language::Cg, OutputTail::AlphaCoverage, 65536)
                    .unwrap();
            let code = compiler
                .compile(gxp_compiler::Stage::Fragment, &masked.source)
                .unwrap_or_else(|e| panic!("{name} masked: {e}"));
            assert_eq!(&code[..4], b"GXP\0");
        }
    }
    println!(
        "compiled {} typed Cg operation programs to GXP",
        programs.len()
    );
}

fn external_texture_programs() -> Vec<(String, Arc<EffectProgram>)> {
    use reel_ui::stage::effect::EffectTextureParameter;
    let mut programs = Vec::new();
    for binding in 0..2 {
        for channel in [
            EffectTextureChannel::Color,
            EffectTextureChannel::Alpha,
            EffectTextureChannel::Gray,
        ] {
            let ty = if channel == EffectTextureChannel::Color {
                EffectType::Vec4
            } else {
                EffectType::Float
            };
            let program = EffectProgram::with_textures(
                vec![],
                vec![
                    EffectTextureParameter { name: "ink".into() },
                    EffectTextureParameter {
                        name: "noise".into(),
                    },
                ],
                vec![
                    EffectNode {
                        ty: EffectType::Vec2,
                        op: EffectOp::Input(EffectInput::Uv),
                    },
                    EffectNode {
                        ty,
                        op: EffectOp::TextureSample {
                            binding,
                            uv: EffectNodeId(0),
                            channel,
                        },
                    },
                ],
                EffectNodeId(1),
            )
            .unwrap();
            programs.push((format!("texture_{binding}_{channel:?}"), program));
        }
    }
    programs
}

/// Every operation is the root of its own graph: dead-code elimination cannot
/// hide an unsupported spelling or overload from the real shader compilers.
fn operation_programs() -> Vec<(String, Arc<EffectProgram>)> {
    let mut programs = Vec::new();
    for ty in [
        EffectType::Float,
        EffectType::Vec2,
        EffectType::Vec3,
        EffectType::Vec4,
    ] {
        let mut operations = Vec::new();
        for op in [
            EffectUnary::Negate,
            EffectUnary::Abs,
            EffectUnary::Floor,
            EffectUnary::Ceil,
            EffectUnary::Fract,
            EffectUnary::Sin,
            EffectUnary::Cos,
            EffectUnary::Tan,
            EffectUnary::Asin,
            EffectUnary::Acos,
            EffectUnary::Atan,
            EffectUnary::Sqrt,
            EffectUnary::Exp,
            EffectUnary::Log,
            EffectUnary::Exp2,
            EffectUnary::Log2,
        ] {
            operations.push((
                ty,
                EffectOp::Unary {
                    op,
                    value: EffectNodeId(0),
                },
            ));
        }
        for op in [
            EffectBinary::Add,
            EffectBinary::Subtract,
            EffectBinary::Multiply,
            EffectBinary::Divide,
            EffectBinary::Remainder,
            EffectBinary::Pow,
            EffectBinary::Min,
            EffectBinary::Max,
            EffectBinary::Atan2,
            EffectBinary::Equal,
            EffectBinary::NotEqual,
        ] {
            let result = if matches!(op, EffectBinary::Equal | EffectBinary::NotEqual) {
                EffectType::Bool
            } else {
                ty
            };
            operations.push((
                result,
                EffectOp::Binary {
                    op,
                    left: EffectNodeId(0),
                    right: EffectNodeId(1),
                },
            ));
        }
        for op in [
            EffectTernary::Clamp,
            EffectTernary::Mix,
            EffectTernary::Smoothstep,
        ] {
            operations.push((
                ty,
                EffectOp::Ternary {
                    op,
                    a: EffectNodeId(0),
                    b: EffectNodeId(1),
                    c: EffectNodeId(2),
                },
            ));
        }
        if ty == EffectType::Float {
            for op in [
                EffectBinary::Less,
                EffectBinary::LessEqual,
                EffectBinary::Greater,
                EffectBinary::GreaterEqual,
            ] {
                operations.push((
                    EffectType::Bool,
                    EffectOp::Binary {
                        op,
                        left: EffectNodeId(0),
                        right: EffectNodeId(1),
                    },
                ));
            }
        } else {
            operations.push((
                EffectType::Float,
                EffectOp::Unary {
                    op: EffectUnary::Length,
                    value: EffectNodeId(0),
                },
            ));
            operations.push((
                ty,
                EffectOp::Unary {
                    op: EffectUnary::Normalize,
                    value: EffectNodeId(0),
                },
            ));
            for op in [EffectBinary::Dot, EffectBinary::Distance] {
                operations.push((
                    EffectType::Float,
                    EffectOp::Binary {
                        op,
                        left: EffectNodeId(0),
                        right: EffectNodeId(1),
                    },
                ));
            }
        }
        for (result_ty, op) in operations {
            let name = format!("{ty:?}_{op:?}");
            let mut g = Graph::default();
            for _ in 0..3 {
                g.parameter(ty);
            }
            let mut output = g.node(result_ty, op);
            if result_ty == EffectType::Bool {
                let yes = g.float(1.0);
                let no = g.float(0.0);
                output = g.node(
                    EffectType::Float,
                    EffectOp::Select {
                        condition: output,
                        when_true: yes,
                        when_false: no,
                    },
                );
            } else if matches!(result_ty, EffectType::Vec2 | EffectType::Vec3) {
                output = g.node(
                    EffectType::Float,
                    EffectOp::Extract {
                        value: output,
                        lane: 0,
                    },
                );
            }
            programs.push((name, g.finish(output)));
        }
    }
    // Boolean operators, all vector constructors, and finite literal boundaries.
    let mut g = Graph::default();
    let a = g.parameter(EffectType::Bool);
    let b = g.node(
        EffectType::Bool,
        EffectOp::Unary {
            op: EffectUnary::Not,
            value: a,
        },
    );
    let equal = g.node(
        EffectType::Bool,
        EffectOp::Binary {
            op: EffectBinary::Equal,
            left: a,
            right: b,
        },
    );
    let unequal = g.node(
        EffectType::Bool,
        EffectOp::Binary {
            op: EffectBinary::NotEqual,
            left: a,
            right: b,
        },
    );
    let choose = g.node(
        EffectType::Bool,
        EffectOp::Select {
            condition: a,
            when_true: equal,
            when_false: unequal,
        },
    );
    let x = g.float(-0.0);
    let y = g.float(f32::MAX);
    let v2 = g.node(EffectType::Vec2, EffectOp::Vec2([x, y]));
    let x = g.node(EffectType::Float, EffectOp::Extract { value: v2, lane: 0 });
    let v3 = g.node(EffectType::Vec3, EffectOp::Vec3([x, y, x]));
    let y = g.node(EffectType::Float, EffectOp::Extract { value: v3, lane: 1 });
    let v4 = g.node(EffectType::Vec4, EffectOp::Vec4([x, y, x, y]));
    let tiny = g.node(
        EffectType::Vec4,
        EffectOp::Constant(EffectValue::Vec4([
            1,
            f32::MIN_POSITIVE.to_bits(),
            (-f32::MAX).to_bits(),
            0,
        ])),
    );
    let output = g.node(
        EffectType::Vec4,
        EffectOp::Select {
            condition: choose,
            when_true: v4,
            when_false: tiny,
        },
    );
    programs.push(("constructors_and_literals".into(), g.finish(output)));
    programs.push(("sampled".into(), sampled_program()));
    programs
}

#[cfg(feature = "wgpu")]
#[test]
fn wgsl_fragments_validate_and_source_calls_stay_in_branches() {
    use wgpu::naga;
    for (name, program) in operation_programs() {
        let generated = generate(&program, Language::Wgsl, 65536).unwrap();
        let source = wgsl_fragment(&generated);
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("{name}: {}\n{source}", e.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{name}: {e:?}\n{source}"));
        if !generated.parameters.slots.is_empty() {
            let parameter_type = module
                .types
                .iter()
                .find(|(_, ty)| ty.name.as_deref() == Some("ReelEffectParameters"))
                .unwrap()
                .1;
            let naga::TypeInner::Struct { members, span } = &parameter_type.inner else {
                panic!("parameter layout must be a uniform struct")
            };
            assert_eq!(*span as usize, generated.parameters.byte_len());
            for (member, slot) in members.iter().zip(&generated.parameters.slots) {
                assert_eq!(member.offset as usize, slot.byte_offset);
            }
        }
        if name == "sampled" {
            fn calls(
                module: &naga::Module,
                block: &naga::Block,
                depth: usize,
                output: &mut Vec<usize>,
            ) {
                for statement in block.iter() {
                    match statement {
                        naga::Statement::Call { function, .. }
                            if module.functions[*function].name.as_deref()
                                == Some("reel_source_sample") =>
                        {
                            output.push(depth)
                        }
                        naga::Statement::If { accept, reject, .. } => {
                            calls(module, accept, depth + 1, output);
                            calls(module, reject, depth + 1, output);
                        }
                        naga::Statement::Block(block) => calls(module, block, depth, output),
                        _ => {}
                    }
                }
            }
            let effect = module
                .functions
                .iter()
                .find(|(_, f)| f.name.as_deref() == Some("reel_effect"))
                .unwrap()
                .1;
            let mut depths = Vec::new();
            calls(&module, &effect.body, 0, &mut depths);
            assert_eq!(depths, [1, 1]);
        }
    }
}

#[test]
#[ignore = "exports generated sources to REEL_EFFECT_SHADER_DIR for offline target compilation"]
fn export_anonymous_effect_fragments() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("REEL_EFFECT_SHADER_DIR")
            .expect("set REEL_EFFECT_SHADER_DIR to an ignored artifact directory"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    for (name, program) in [
        ("sampled", sampled_program()),
        ("sampling61", sampling_program(30)),
    ] {
        for language in [Language::Wgsl, Language::Cg] {
            let generated = compile_fragment(&program, language, 16384).unwrap();
            let extension = if language == Language::Wgsl {
                "wgsl"
            } else {
                "cg"
            };
            std::fs::write(
                directory.join(format!("{name}.{extension}")),
                &generated.source,
            )
            .unwrap();
            let full = compile_fragment_variant(
                &program,
                language,
                OutputTail::Ordinary,
                FragmentCoverage::FullTarget,
                16384,
            )
            .unwrap();
            std::fs::write(
                directory.join(format!("{name}-full.{extension}")),
                full.source,
            )
            .unwrap();
            let masked =
                compile_fragment_with_tail(&program, language, OutputTail::AlphaCoverage, 16384)
                    .unwrap();
            std::fs::write(
                directory.join(format!("{name}-masked.{extension}")),
                masked.source,
            )
            .unwrap();
            println!(
                "{name} {language:?}: generated_bytes={}, parameters={}B",
                generated.source.len(),
                generated.parameters.byte_len()
            );
        }
    }
}

#[cfg(feature = "wgpu")]
#[test]
fn typed_external_textures_emit_fixed_bindings_and_validate_wgsl() {
    use reel_ui::stage::effect::{EffectTextureChannel, EffectTextureParameter};
    use wgpu::naga;
    let mut nodes = vec![EffectNode {
        ty: EffectType::Vec2,
        op: EffectOp::Input(EffectInput::Uv),
    }];
    for (index, channel) in [
        EffectTextureChannel::Color,
        EffectTextureChannel::Alpha,
        EffectTextureChannel::Gray,
    ]
    .into_iter()
    .enumerate()
    {
        let ty = if channel == EffectTextureChannel::Color {
            EffectType::Vec4
        } else {
            EffectType::Float
        };
        nodes.push(EffectNode {
            ty,
            op: EffectOp::TextureSample {
                binding: 0,
                uv: EffectNodeId(0),
                channel,
            },
        });
        let program = EffectProgram::with_textures(
            vec![],
            vec![EffectTextureParameter { name: "ink".into() }],
            nodes.clone(),
            EffectNodeId(index as u32 + 1),
        )
        .unwrap();
        for language in [Language::Wgsl, Language::Cg] {
            let fragment = compile_fragment(&program, language, 32768).unwrap();
            assert!(fragment.source.contains("reel_texture_0"));
            assert!(!fragment.source.contains("u_ops"));
            assert_eq!(
                fragment.source_sampler,
                channel != EffectTextureChannel::Color
            );
            if language == Language::Wgsl {
                let module = naga::front::wgsl::parse_str(&fragment.source).unwrap();
                naga::valid::Validator::new(
                    naga::valid::ValidationFlags::all(),
                    naga::valid::Capabilities::all(),
                )
                .validate(&module)
                .unwrap();
            }
        }
    }
}

#[test]
fn independent_coverage_tail_preserves_typed_author_program_and_sampler_bound() {
    for language in [Language::Wgsl, Language::Cg] {
        let program = sampled_program();
        let ordinary = compile_fragment(&program, language, 16384).unwrap();
        let masked =
            compile_fragment_with_tail(&program, language, OutputTail::AlphaCoverage, 16384)
                .unwrap();
        assert_eq!(ordinary.parameters, masked.parameters);
        assert!(masked.source.contains("reel_composite_mask"));
        assert!(masked.source.contains("stored * coverage *"));
        assert!(
            compile_fragment_with_tail(
                &program,
                language,
                OutputTail::AlphaCoverage,
                masked.source.len() - 1
            )
            .is_err()
        );
    }
    assert!(OutputTail::supports_coverage(EffectType::Vec4, 14, true));
    assert!(!OutputTail::supports_coverage(EffectType::Vec4, 15, true));
    assert!(!OutputTail::supports_coverage(EffectType::Float, 0, true));
}

#[path = "effect_reduction_tests.rs"]
mod reduction_tests;

#[path = "effect_coverage_tests.rs"]
mod coverage_tests;
