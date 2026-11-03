//! Output selection admission and its final UNORM observation boundary.
use super::*;

fn has_weighted_output(program: &EffectProgram) -> bool {
    compile_fragment(program, Language::Cg, 1 << 20)
        .unwrap()
        .source
        .contains("float quantize = float(mode == 1.0);")
}

fn convolution(weights: &[f32], divisor: f32, clamp: bool) -> Arc<EffectProgram> {
    let mut g = Graph::default();
    let uv = g.node(EffectType::Vec2, EffectOp::Input(EffectInput::Uv));
    let zero = g.float(0.0);
    let mut channels = [zero; 4];
    for &weight in weights {
        let sample = g.node(EffectType::Vec4, EffectOp::SourceSample { uv });
        let weight = g.float(weight);
        for (lane, channel) in channels.iter_mut().enumerate() {
            let value = g.node(
                EffectType::Float,
                EffectOp::Extract {
                    value: sample,
                    lane: lane as u8,
                },
            );
            let value = g.node(
                EffectType::Float,
                EffectOp::Binary {
                    op: EffectBinary::Multiply,
                    left: value,
                    right: weight,
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
    let divisor = g.float(divisor);
    let one = g.float(1.0);
    for channel in &mut channels {
        *channel = g.node(
            EffectType::Float,
            EffectOp::Binary {
                op: EffectBinary::Divide,
                left: *channel,
                right: divisor,
            },
        );
        if clamp {
            *channel = g.node(
                EffectType::Float,
                EffectOp::Ternary {
                    op: EffectTernary::Clamp,
                    a: *channel,
                    b: zero,
                    c: one,
                },
            );
        }
    }
    let output = g.node(EffectType::Vec4, EffectOp::Vec4(channels));
    g.finish(output)
}

#[test]
fn cg_normalized_output_accepts_ordered_means_and_static_nonnegative_convolutions() {
    for count in [1, 3, 9, 17, 31, 32, 61, 127] {
        let offsets: Vec<_> = (0..count).map(|n| n as f32).collect();
        let program = sampling_offsets(&offsets);
        assert!(has_weighted_output(&program), "{count} taps");
        let function = generate(&program, Language::Cg, 1 << 20).unwrap().function;
        let fragment = compile_fragment(&program, Language::Cg, 1 << 20).unwrap();
        assert!(
            fragment.source.contains(&function),
            "original f32 expression body"
        );
        assert!(fragment.source.contains(cg::UV_DISCARD) || fragment.source.contains(cg::UV_KILL));
        assert!(
            fragment
                .source
                .contains("float3 p = float3(clamp(uv, 0.0, 1.0), 1.0);")
        );
        assert!(
            fragment
                .source
                .contains("q = clamp(q, reel_context[6].xy + half_texel")
        );
        assert!(
            fragment
                .source
                .contains("return tex2Dlod(reel_source, float4(q, 0.0, 0.0));")
        );
    }
    for (weights, divisor) in [
        (&[0.25, 0.75][..], 1.0),
        (&[1.0, 4.0, 6.0, 4.0, 1.0][..], 16.0),
        (&[0.125; 8][..], 1.0),
    ] {
        assert!(has_weighted_output(&convolution(weights, divisor, false)));
    }
}

#[test]
fn cg_normalized_output_uses_ordered_f32_bounds_and_rejects_hidden_nonfinite_work() {
    // The mathematical weight sum is one, but its original ordered f32 sum
    // exceeds one. A following explicit clamp establishes the missing bound.
    assert!(
        [0.1_f32; 10]
            .into_iter()
            .fold(0.0, |sum, value| sum + value)
            > 1.0
    );
    assert!(!has_weighted_output(&convolution(&[0.1; 10], 1.0, false)));
    assert!(has_weighted_output(&convolution(&[0.1; 10], 1.0, true)));
    for (weights, divisor, clamp) in [
        (&[1.0][..], 0.0, false),
        (&[1.0][..], f32::from_bits(1), false),
        (&[f32::MAX, f32::MAX][..], f32::MAX, true),
        (&[-0.0][..], 1.0, false),
        (&[-1.0, 2.0][..], 1.0, true),
        (&[1.0, 1.0][..], 1.0, false),
    ] {
        assert!(!has_weighted_output(&convolution(weights, divisor, clamp)));
    }
    let mut g = Graph::default();
    let parameter = g.parameter(EffectType::Vec4);
    g.parameters[0].default = Some(EffectValue::Vec4([0.5_f32.to_bits(); 4]));
    assert!(
        !has_weighted_output(&g.finish(parameter)),
        "a default is not a live range"
    );
    for value in [-0.0, -1.0, f32::from_bits(1.0_f32.to_bits() + 1)] {
        let mut g = Graph::default();
        let output = g.node(
            EffectType::Vec4,
            EffectOp::Constant(EffectValue::Vec4([value.to_bits(); 4])),
        );
        assert!(!has_weighted_output(&g.finish(output)));
    }
}

#[test]
fn cg_normalized_output_checks_every_selected_lane_and_preserves_unknown_texture_ranges() {
    use reel_ui::stage::effect::{EffectTextureChannel, EffectTextureParameter};
    for unknown_branch in [false, true] {
        let mut g = Graph::default();
        let condition = g.parameter(EffectType::Bool);
        let unknown = g.parameter(EffectType::Vec4);
        let a = g.node(
            EffectType::Vec2,
            EffectOp::Constant(EffectValue::Vec2([0.25_f32.to_bits(), 0.5_f32.to_bits()])),
        );
        let b = g.node(
            EffectType::Vec3,
            EffectOp::Constant(EffectValue::Vec3([0.0_f32.to_bits(); 3])),
        );
        let x = g.node(EffectType::Float, EffectOp::Extract { value: a, lane: 0 });
        let y = g.node(EffectType::Float, EffectOp::Extract { value: b, lane: 2 });
        let two = g.node(EffectType::Vec2, EffectOp::Vec2([x, y]));
        let three = g.node(EffectType::Vec3, EffectOp::Vec3([x, y, x]));
        let z = g.node(
            EffectType::Float,
            EffectOp::Extract {
                value: two,
                lane: 1,
            },
        );
        let w = g.node(
            EffectType::Float,
            EffectOp::Extract {
                value: three,
                lane: 2,
            },
        );
        let rgba = g.node(EffectType::Vec4, EffectOp::Vec4([x, y, z, w]));
        let one = g.node(
            EffectType::Vec4,
            EffectOp::Constant(EffectValue::Vec4([1.0_f32.to_bits(); 4])),
        );
        let maximum = g.node(
            EffectType::Vec4,
            EffectOp::Binary {
                op: EffectBinary::Max,
                left: rgba,
                right: one,
            },
        );
        let minimum = g.node(
            EffectType::Vec4,
            EffectOp::Binary {
                op: EffectBinary::Min,
                left: rgba,
                right: one,
            },
        );
        let output = g.node(
            EffectType::Vec4,
            EffectOp::Select {
                condition,
                when_true: maximum,
                when_false: if unknown_branch { unknown } else { minimum },
            },
        );
        assert_eq!(has_weighted_output(&g.finish(output)), !unknown_branch);
    }
    let program = EffectProgram::with_textures(
        vec![],
        vec![EffectTextureParameter {
            name: "external".into(),
        }],
        vec![
            EffectNode {
                ty: EffectType::Vec2,
                op: EffectOp::Input(EffectInput::Uv),
            },
            EffectNode {
                ty: EffectType::Vec4,
                op: EffectOp::TextureSample {
                    binding: 0,
                    uv: EffectNodeId(0),
                    channel: EffectTextureChannel::Color,
                },
            },
        ],
        EffectNodeId(1),
    )
    .unwrap();
    assert!(
        !has_weighted_output(&program),
        "an external texture may hold HDR/nonfinite data"
    );
}

#[test]
fn cg_normalized_output_keeps_other_tails_and_original_source_admission() {
    let program = sampling_program(1);
    let ordinary = compile_fragment(&program, Language::Cg, 1 << 20).unwrap();
    let weighted = "float quantize = float(mode == 1.0);";
    assert!(ordinary.source.contains(weighted));
    let masked =
        compile_fragment_with_tail(&program, Language::Cg, OutputTail::AlphaCoverage, 1 << 20)
            .unwrap();
    assert!(!masked.source.contains(weighted));
    assert!(masked.source.contains("if (mode == 0.0) return color;"));
    assert!(
        masked
            .source
            .contains("stored *= reel_context[2].w; stored = floor")
    );
    let limited = compile_fragment(&program, Language::Cg, ordinary.source.len() - 1).unwrap();
    assert!(!limited.source.contains(weighted));
    assert!(limited.source.contains("if (mode == 0.0) return color;"));
    let function = generate(&program, Language::Cg, 1 << 20).unwrap().function;
    assert!(limited.source.contains(&function));
    let mut g = Graph::default();
    let output = g.float(0.5);
    assert!(
        !has_weighted_output(&g.finish(output)),
        "scalar coverage keeps its tail"
    );
}

fn quantized(value: f32) -> f32 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5).floor() / 255.0
}
fn original_output(color: f32, mode: u8, coverage: f32, opacity: f32) -> f32 {
    if mode == 0 {
        return color;
    }
    let stored = if mode == 1 { quantized(color) } else { color };
    stored * coverage * opacity
}
fn weighted_output(color: f32, mode: u8, coverage: f32, opacity: f32) -> f32 {
    let quantize = f32::from(mode == 1);
    let terminal = f32::from(mode != 0);
    let stored = color * (1.0 - quantize) + quantized(color) * quantize;
    let composed = stored * coverage * opacity;
    color * (1.0 - terminal) + composed * terminal
}

#[test]
fn cg_normalized_output_preserves_nonzero_bits_and_final_unorm_zero_through_blending() {
    let mut colors = vec![0.0, 1.0, f32::from_bits(1)];
    for byte in 0..255 {
        let halfway = (byte as f32 + 0.5) / 255.0;
        colors.extend([
            f32::from_bits(halfway.to_bits() - 1),
            halfway,
            f32::from_bits(halfway.to_bits() + 1),
        ]);
    }
    for color in colors {
        for mode in [0, 1, 2] {
            for coverage in [0.0, 0.25, 1.0] {
                for opacity in [-0.0, 0.0, 0.1, 0.5, 1.0] {
                    let a = original_output(color, mode, coverage, opacity);
                    let b = weighted_output(color, mode, coverage, opacity);
                    if a != 0.0 {
                        assert_eq!(a.to_bits(), b.to_bits());
                    } else {
                        assert_eq!(b, 0.0);
                    }
                    // GXM consumes this final return through UCHAR4 replacement
                    // or fixed-function source-over, then UNORM storage. There
                    // is no later author IEEE division of the returned zero.
                    assert_eq!(quantized(a).to_bits(), quantized(b).to_bits());
                    for destination in [0.0, 31.0 / 255.0, 1.0] {
                        let alpha_a = original_output(0.5, mode, coverage, opacity);
                        let alpha_b = weighted_output(0.5, mode, coverage, opacity);
                        assert_eq!(
                            quantized(a + destination * (1.0 - alpha_a)).to_bits(),
                            quantized(b + destination * (1.0 - alpha_b)).to_bits(),
                        );
                    }
                }
            }
        }
    }
    // This is why the range proof cannot be replaced by an eager general tail.
    assert_eq!(original_output(f32::INFINITY, 1, 1.0, 1.0), 1.0);
    assert!(weighted_output(f32::INFINITY, 1, 1.0, 1.0).is_nan());
}
