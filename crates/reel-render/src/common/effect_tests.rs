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
                assert!(output.source.contains("uniform float4 reel_context[9]"));
                assert!(output.source.contains("tex2D(reel_source, q)"));
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
        .collect::<Vec<_>>();
    for (name, program) in &programs {
        let fragment = compile_fragment(program, Language::Cg, 65536).unwrap();
        let binary = compiler
            .compile(gxp_compiler::Stage::Fragment, &fragment.source)
            .unwrap_or_else(|error| panic!("{name}: {error}\n{}", fragment.source));
        assert_eq!(&binary[..4], b"GXP\0");
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
    for language in [Language::Wgsl, Language::Cg] {
        let generated = compile_fragment(&sampled_program(), language, 16384).unwrap();
        let extension = if language == Language::Wgsl {
            "wgsl"
        } else {
            "cg"
        };
        std::fs::write(
            directory.join(format!("sampled.{extension}")),
            &generated.source,
        )
        .unwrap();
        println!(
            "{language:?}: generated_bytes={}, parameters={}B",
            generated.source.len(),
            generated.parameters.byte_len()
        );
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
