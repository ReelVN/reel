// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Preserve ordinary mask DAGs in the shared typed drawing language.
use super::*;
use crate::stage::{MaskChannel, MaskOp, MaskProgram};

struct Builder {
    nodes: Vec<EffectNode>,
}
impl Builder {
    fn node(&mut self, ty: EffectType, op: EffectOp) -> Option<EffectNodeId> {
        let id = EffectNodeId(u32::try_from(self.nodes.len()).ok()?);
        self.nodes.push(EffectNode { ty, op });
        Some(id)
    }
    fn constant(&mut self, bits: u32) -> Option<EffectNodeId> {
        self.node(
            EffectType::Float,
            EffectOp::Constant(EffectValue::Float(bits)),
        )
    }
    fn binary(
        &mut self,
        op: EffectBinary,
        left: EffectNodeId,
        right: EffectNodeId,
    ) -> Option<EffectNodeId> {
        self.node(EffectType::Float, EffectOp::Binary { op, left, right })
    }
}

/// Lower a validated mask graph to the same immutable IR as authored effects.
///
/// The four numeric slots remain `progress`, `softness`, `x`, and `y`; whole-image
/// bindings retain their source order without embedding resource paths. Repeat is
/// applied to each requested coordinate before the existing texel-center clamp.
/// The internal string-mask threshold keeps its dedicated existing path.
pub fn lower_mask(program: &MaskProgram) -> Option<Arc<EffectProgram>> {
    if program.is_image_threshold() {
        return None;
    }
    let parameters = ["progress", "softness", "x", "y"]
        .map(|name| EffectParameter {
            name: name.into(),
            ty: EffectType::Float,
            default: Some(EffectValue::Float(0)),
        })
        .to_vec();
    let textures = (0..program.sources().len())
        .map(|index| EffectTextureParameter {
            name: format!("image_{index}").into(),
        })
        .collect();
    let mut builder = Builder { nodes: Vec::new() };
    let uv = builder.node(EffectType::Vec2, EffectOp::Input(EffectInput::Uv))?;
    let u = builder.node(EffectType::Float, EffectOp::Extract { value: uv, lane: 0 })?;
    let v = builder.node(EffectType::Float, EffectOp::Extract { value: uv, lane: 1 })?;
    let mut lowered: Vec<EffectNodeId> = Vec::with_capacity(program.operations().len());
    for operation in program.operations() {
        let at = |index: u32| lowered.get(index as usize).copied();
        let id = match *operation {
            MaskOp::Constant(bits) => builder.constant(bits)?,
            MaskOp::Parameter(index) => {
                builder.node(EffectType::Float, EffectOp::Parameter(u32::from(index)))?
            }
            MaskOp::Sample {
                image,
                channel,
                coordinates: [x, y, sx, sy],
                repeat,
            } => {
                let xs = builder.binary(EffectBinary::Multiply, u, at(sx)?)?;
                let mut xs = builder.binary(EffectBinary::Add, xs, at(x)?)?;
                let ys = builder.binary(EffectBinary::Multiply, v, at(sy)?)?;
                let mut ys = builder.binary(EffectBinary::Add, ys, at(y)?)?;
                if repeat & 1 != 0 {
                    xs = builder.node(
                        EffectType::Float,
                        EffectOp::Unary {
                            op: EffectUnary::Fract,
                            value: xs,
                        },
                    )?;
                }
                if repeat & 2 != 0 {
                    ys = builder.node(
                        EffectType::Float,
                        EffectOp::Unary {
                            op: EffectUnary::Fract,
                            value: ys,
                        },
                    )?;
                }
                let uv = builder.node(EffectType::Vec2, EffectOp::Vec2([xs, ys]))?;
                builder.node(
                    EffectType::Float,
                    EffectOp::TextureSample {
                        binding: image,
                        uv,
                        channel: match channel {
                            MaskChannel::Alpha => EffectTextureChannel::Alpha,
                            MaskChannel::Gray => EffectTextureChannel::Gray,
                        },
                    },
                )?
            }
            MaskOp::Add(a, b) => builder.binary(EffectBinary::Add, at(a)?, at(b)?)?,
            MaskOp::Subtract(a, b) => builder.binary(EffectBinary::Subtract, at(a)?, at(b)?)?,
            MaskOp::Multiply(a, b) => builder.binary(EffectBinary::Multiply, at(a)?, at(b)?)?,
            MaskOp::Pow(a, bits) => {
                // The mask contract defines x^0 as one even for x=0. Avoid the
                // backend-dependent pow(0,0), retaining the exact positive zero.
                if f32::from_bits(bits) == 0. {
                    builder.constant(1_f32.to_bits())?
                } else {
                    let zero = builder.constant(0)?;
                    let base = builder.binary(EffectBinary::Max, at(a)?, zero)?;
                    let power = builder.constant(bits)?;
                    builder.binary(EffectBinary::Pow, base, power)?
                }
            }
            MaskOp::Clamp(value, low, high) => {
                let low = builder.constant(low)?;
                let high = builder.constant(high)?;
                builder.node(
                    EffectType::Float,
                    EffectOp::Ternary {
                        op: EffectTernary::Clamp,
                        a: at(value)?,
                        b: low,
                        c: high,
                    },
                )?
            }
            MaskOp::Smoothstep(a, b, value) => builder.node(
                EffectType::Float,
                EffectOp::Ternary {
                    op: EffectTernary::Smoothstep,
                    a: at(a)?,
                    b: at(b)?,
                    c: at(value)?,
                },
            )?,
            MaskOp::ImageThreshold { .. } => return None,
        };
        lowered.push(id);
    }
    EffectProgram::with_textures(parameters, textures, builder.nodes, *lowered.last()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum Value {
        Float(f32),
        Vec2([f32; 2]),
    }
    impl Value {
        fn float(self) -> f32 {
            if let Self::Float(value) = self {
                value
            } else {
                panic!("scalar expected")
            }
        }
        fn vec2(self) -> [f32; 2] {
            if let Self::Vec2(value) = self {
                value
            } else {
                panic!("coordinate expected")
            }
        }
    }

    fn sample(binding: u32, channel: EffectTextureChannel, uv: [f32; 2]) -> f32 {
        let [x, y] = uv.map(|value| value.clamp(0., 1.));
        match channel {
            EffectTextureChannel::Alpha => 0.125 + x * 0.5 + y * 0.25,
            EffectTextureChannel::Gray => (x * y) * (binding as f32 * 0.25 + 0.5),
            _ => panic!("mask samples retain their scalar channel"),
        }
    }

    fn evaluate(
        program: &EffectProgram,
        parameters: [f32; 4],
        uv: [f32; 2],
    ) -> (f32, Vec<(u32, EffectTextureChannel, [f32; 2])>) {
        let mut values: Vec<Value> = Vec::new();
        let mut samples = Vec::new();
        for node in program.nodes() {
            let at = |id: EffectNodeId| values[id.0 as usize];
            let value = match node.op {
                EffectOp::Constant(EffectValue::Float(bits)) => Value::Float(f32::from_bits(bits)),
                EffectOp::Parameter(index) => Value::Float(parameters[index as usize]),
                EffectOp::Input(EffectInput::Uv) => Value::Vec2(uv),
                EffectOp::Extract { value, lane } => Value::Float(at(value).vec2()[lane as usize]),
                EffectOp::Vec2(ids) => Value::Vec2(ids.map(|id| at(id).float())),
                EffectOp::Unary {
                    op: EffectUnary::Fract,
                    value,
                } => {
                    let value = at(value).float();
                    Value::Float(value - value.floor())
                }
                EffectOp::Binary { op, left, right } => {
                    let (a, b) = (at(left).float(), at(right).float());
                    Value::Float(match op {
                        EffectBinary::Add => a + b,
                        EffectBinary::Subtract => a - b,
                        EffectBinary::Multiply => a * b,
                        EffectBinary::Max => a.max(b),
                        EffectBinary::Pow => a.powf(b),
                        _ => panic!("unexpected lowered binary"),
                    })
                }
                EffectOp::Ternary { op, a, b, c } => {
                    let (a, b, c) = (at(a).float(), at(b).float(), at(c).float());
                    Value::Float(match op {
                        EffectTernary::Clamp => a.clamp(b, c),
                        EffectTernary::Smoothstep if a == b => f32::from(c >= a),
                        EffectTernary::Smoothstep => {
                            let t = ((c - a) / (b - a)).clamp(0., 1.);
                            t * t * (3. - 2. * t)
                        }
                        _ => panic!("unexpected lowered ternary"),
                    })
                }
                EffectOp::TextureSample {
                    binding,
                    uv,
                    channel,
                } => {
                    let uv = at(uv).vec2();
                    samples.push((binding, channel, uv));
                    Value::Float(sample(binding, channel, uv))
                }
                _ => panic!("unexpected mask expression"),
            };
            values.push(value);
        }
        (values[program.output().0 as usize].float(), samples)
    }

    fn graph(repeat: u8, sources: [&str; 2]) -> Arc<MaskProgram> {
        MaskProgram::new(
            vec![
                MaskOp::Constant(0),
                MaskOp::Constant(1_f32.to_bits()),
                MaskOp::Parameter(2),
                MaskOp::Parameter(3),
                MaskOp::Sample {
                    image: 0,
                    channel: MaskChannel::Alpha,
                    coordinates: [2, 3, 1, 1],
                    repeat,
                },
                // The second texture's offset depends on the first sampled value.
                MaskOp::Sample {
                    image: 1,
                    channel: MaskChannel::Gray,
                    coordinates: [4, 0, 1, 1],
                    repeat: repeat ^ 3,
                },
                MaskOp::Add(4, 5),
                MaskOp::Parameter(0),
                MaskOp::Subtract(6, 7),
                MaskOp::Parameter(1),
                MaskOp::Multiply(8, 9),
                MaskOp::Pow(10, 2_f32.to_bits()),
                MaskOp::Clamp(11, 0, 1_f32.to_bits()),
                MaskOp::Smoothstep(0, 1, 12),
            ],
            sources.map(Arc::from).to_vec(),
        )
        .unwrap()
    }

    #[test]
    fn scalar_math_and_dependent_sampling_preserve_parameters_channels_and_repeat() {
        for repeat in 0..4 {
            let program = lower_mask(&graph(repeat, ["a", "b"])).unwrap();
            let wrap = |mut uv: [f32; 2], bits| {
                for (axis, value) in uv.iter_mut().enumerate() {
                    if bits & (1 << axis) != 0 {
                        *value -= value.floor();
                    }
                }
                uv
            };
            for uv in [[-0.25, 1.25], [0., 0.], [0.25, 0.75], [1., 1.]] {
                for parameters in [[0.2, 0.75, 0.125, -0.5], [0.9, 2., -1.25, 1.5]] {
                    let first_uv = wrap([uv[0] + parameters[2], uv[1] + parameters[3]], repeat);
                    let alpha = sample(0, EffectTextureChannel::Alpha, first_uv);
                    let second_uv = wrap([uv[0] + alpha, uv[1]], repeat ^ 3);
                    let gray = sample(1, EffectTextureChannel::Gray, second_uv);
                    let value = ((alpha + gray - parameters[0]) * parameters[1])
                        .max(0.)
                        .powf(2.)
                        .clamp(0., 1.);
                    let expected = value * value * (3. - 2. * value);
                    let (actual, samples) = evaluate(&program, parameters, uv);
                    assert_eq!(actual.to_bits(), expected.to_bits());
                    assert_eq!(
                        samples,
                        vec![
                            (0, EffectTextureChannel::Alpha, first_uv),
                            (1, EffectTextureChannel::Gray, second_uv)
                        ]
                    );
                }
            }
        }
    }

    #[test]
    fn equal_and_reversed_smoothstep_edges_and_zero_power_keep_mask_semantics() {
        let smooth = MaskProgram::new(
            vec![
                MaskOp::Parameter(0),
                MaskOp::Parameter(1),
                MaskOp::Parameter(2),
                MaskOp::Smoothstep(0, 1, 2),
            ],
            vec![],
        )
        .unwrap();
        let smooth = lower_mask(&smooth).unwrap();
        for (parameters, expected) in [
            ([0.5, 0.5, 0.499, 0.], 0.),
            ([0.5, 0.5, 0.5, 0.], 1.),
            ([1., 0., 0.25, 0.], 0.84375),
        ] {
            assert_eq!(evaluate(&smooth, parameters, [0.; 2]).0, expected);
        }
        let power =
            MaskProgram::new(vec![MaskOp::Parameter(0), MaskOp::Pow(0, 0)], vec![]).unwrap();
        let power = lower_mask(&power).unwrap();
        for base in [-2., -0., 0., 1.] {
            assert_eq!(evaluate(&power, [base, 0., 0., 0.], [0.; 2]).0, 1.);
        }
        assert!(!power.nodes().iter().any(|node| matches!(
            node.op,
            EffectOp::Binary {
                op: EffectBinary::Pow,
                ..
            }
        )));
    }

    #[test]
    fn structure_excludes_paths_and_live_values_while_threshold_keeps_its_fast_path() {
        let a = lower_mask(&graph(3, ["a", "b"])).unwrap();
        let b = lower_mask(&graph(3, ["different-a", "different-b"])).unwrap();
        assert!(a.same_structure(&b));
        assert_eq!(a.structure_hash(), b.structure_hash());
        assert_eq!(
            a.parameters()
                .iter()
                .map(|p| p.name.as_ref())
                .collect::<Vec<_>>(),
            ["progress", "softness", "x", "y"]
        );
        assert!(
            a.parameters()
                .iter()
                .all(|p| p.ty == EffectType::Float && p.default == Some(EffectValue::Float(0)))
        );
        assert_eq!(
            a.textures()
                .iter()
                .map(|p| p.name.as_ref())
                .collect::<Vec<_>>(),
            ["image_0", "image_1"]
        );
        assert!(lower_mask(&MaskProgram::image("a".into())).is_none());
    }
}
