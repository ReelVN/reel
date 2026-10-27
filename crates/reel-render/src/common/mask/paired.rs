//! Two independent sample chains joined once, without indexed pixel registers.
use super::{MaskChannel, MaskOp, MaskProgram, NODES, STEPS};

pub(crate) const MATH: usize = 8;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Step {
    Add(u32),
    Subtract(u32),
    SubtractFrom(u32),
    Multiply(u32),
    Pow(u32),
    Clamp(u32, u32),
    Smoothstep(u32, u32),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Sample {
    pub image: u32,
    pub channel: MaskChannel,
    pub coordinates: [u32; 4],
    pub repeat: u8,
}

/// Chains preserve the source arithmetic order and leave the join unclamped.
pub(crate) struct Plan {
    uniform: u32,
    pub samples: [Sample; 2],
    pub chains: [[Option<Step>; STEPS]; 3],
    /// One is addition, two is subtraction, and three is multiplication.
    pub join: u8,
}

pub(crate) struct Values {
    pub coordinates: [[f32; 4]; 2],
    pub chains: [[[f32; 4]; STEPS]; 3],
}

pub(crate) struct Stream {
    pub operations: [[f32; 4]; MATH],
    pub phases: [f32; 4],
}

impl Values {
    pub(crate) fn stream(&self, join: u8) -> Stream {
        let mut operations = [[0.; 4]; MATH];
        let mut phases = [0.; 4];
        let mut count = 0;
        for (chain, output) in self.chains.iter().zip(&mut phases) {
            for &step in chain.iter().take_while(|step| step[0] != 0.) {
                operations[count] = step;
                count += 1;
            }
            *output = count as f32;
        }
        phases[3] = f32::from(join);
        Stream { operations, phases }
    }
}

impl Plan {
    pub(crate) fn new(program: &MaskProgram) -> Option<Self> {
        let operations = program.operations();
        if operations.len() > NODES || program.is_image_threshold() {
            return None;
        }
        let mut uniform = 0_u32;
        for (index, operation) in operations.iter().enumerate() {
            let at = |node: u32| uniform & (1 << node) != 0;
            if match *operation {
                MaskOp::Constant(_) | MaskOp::Parameter(_) => true,
                MaskOp::Add(a, b) | MaskOp::Subtract(a, b) | MaskOp::Multiply(a, b) => {
                    at(a) && at(b)
                }
                MaskOp::Clamp(a, _, _) => at(a),
                _ => false,
            } {
                uniform |= 1 << index;
            }
        }
        let peel = |mut node: u32| {
            let mut steps = [None; STEPS];
            let mut count = 0;
            let at = |node: u32| uniform & (1 << node) != 0;
            loop {
                let (input, step) = match operations[node as usize] {
                    MaskOp::Add(a, b) if at(b) => (a, Step::Add(b)),
                    MaskOp::Add(a, b) if at(a) => (b, Step::Add(a)),
                    MaskOp::Subtract(a, b) if at(b) => (a, Step::Subtract(b)),
                    MaskOp::Subtract(a, b) if at(a) => (b, Step::SubtractFrom(a)),
                    MaskOp::Multiply(a, b) if at(b) => (a, Step::Multiply(b)),
                    MaskOp::Multiply(a, b) if at(a) => (b, Step::Multiply(a)),
                    MaskOp::Pow(a, exponent) => (a, Step::Pow(exponent)),
                    MaskOp::Clamp(a, low, high) => (a, Step::Clamp(low, high)),
                    MaskOp::Smoothstep(a, b, x) if at(a) && at(b) => (x, Step::Smoothstep(a, b)),
                    _ => {
                        steps[..count].reverse();
                        return Some((node, steps));
                    }
                };
                if at(node) {
                    return None;
                }
                *steps.get_mut(count)? = Some(step);
                count += 1;
                node = input;
            }
        };
        let (root, after) = peel(operations.len().checked_sub(1)? as u32)?;
        let (left, right, join) = match operations[root as usize] {
            MaskOp::Add(a, b) => (a, b, 1),
            MaskOp::Subtract(a, b) => (a, b, 2),
            MaskOp::Multiply(a, b) => (a, b, 3),
            _ => return None,
        };
        let (left, first) = peel(left)?;
        let (right, second) = peel(right)?;
        let sample = |node: u32| match operations[node as usize] {
            MaskOp::Sample {
                image,
                channel,
                coordinates,
                repeat,
            } if coordinates.iter().all(|&node| uniform & (1 << node) != 0) => Some(Sample {
                image,
                channel,
                coordinates,
                repeat,
            }),
            _ => None,
        };
        if first.iter().chain(&second).chain(&after).flatten().count() > MATH {
            return None;
        }
        Some(Self {
            uniform,
            samples: [sample(left)?, sample(right)?],
            chains: [first, second, after],
            join,
        })
    }

    pub(crate) fn is_uniform(&self, index: usize) -> bool {
        self.uniform & (1 << index) != 0
    }

    /// Fold draw-uniform arithmetic in its original f32 order. Transcendentals
    /// stay in the pixel program, as in the complete mask interpreter.
    pub(crate) fn evaluate(&self, program: &MaskProgram, parameters: [f32; 5]) -> Option<Values> {
        let mut values = [0.; NODES];
        for (index, operation) in program.operations().iter().enumerate() {
            if !self.is_uniform(index) {
                continue;
            }
            let at = |index: u32| values[index as usize];
            let value = match *operation {
                MaskOp::Constant(bits) => f32::from_bits(bits),
                MaskOp::Parameter(index) => parameters[usize::from(index)],
                MaskOp::Add(a, b) => at(a) + at(b),
                MaskOp::Subtract(a, b) => at(a) - at(b),
                MaskOp::Multiply(a, b) => at(a) * at(b),
                MaskOp::Clamp(a, low, high) => {
                    at(a).clamp(f32::from_bits(low), f32::from_bits(high))
                }
                _ => unreachable!("uniform mask nodes contain basic scalar arithmetic"),
            };
            if !value.is_finite() {
                return None;
            }
            values[index] = value;
        }
        Some(Values {
            coordinates: self
                .samples
                .map(|sample| sample.coordinates.map(|n| values[n as usize])),
            chains: self.chains.map(|chain| {
                chain.map(|step| match step {
                    None => [0.; 4],
                    Some(Step::Add(a)) => [1., values[a as usize], 0., 0.],
                    Some(Step::Subtract(a)) => [2., values[a as usize], 0., 0.],
                    Some(Step::SubtractFrom(a)) => [3., values[a as usize], 0., 0.],
                    Some(Step::Multiply(a)) => [4., values[a as usize], 0., 0.],
                    Some(Step::Pow(bits)) => [5., f32::from_bits(bits), 0., 0.],
                    Some(Step::Clamp(low, high)) => {
                        [6., f32::from_bits(low), f32::from_bits(high), 0.]
                    }
                    Some(Step::Smoothstep(a, b)) => {
                        [7., values[a as usize], values[b as usize], 0.]
                    }
                })
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::operation_value;
    use super::*;

    fn value(mut value: f32, steps: [[f32; 4]; STEPS]) -> f32 {
        for [op, a, b, _] in steps {
            value = match op as u8 {
                0 => break,
                1 => value + a,
                2 => value - a,
                3 => a - value,
                4 => value * a,
                5 if a == 0. => 1.,
                5 => value.max(0.).powf(a),
                6 => value.clamp(a, b),
                7 => operation_value(&MaskOp::Smoothstep(0, 1, 2), [0.; 5], 0., 0., |i| {
                    [a, b, value][i as usize]
                }),
                _ => unreachable!(),
            };
        }
        value
    }

    fn streamed(input: &Values, join: u8, mut a: f32, mut b: f32) -> f32 {
        let stream = input.stream(join);
        let [first, second, count, _] = stream.phases;
        let combine = |a, b| match join {
            1 => a + b,
            2 => a - b,
            3 => a * b,
            _ => unreachable!(),
        };
        for (index, step) in stream
            .operations
            .into_iter()
            .enumerate()
            .take(count as usize)
        {
            let mut steps = [[0.; 4]; STEPS];
            steps[0] = step;
            if (index as f32) < first {
                a = value(a, steps);
            } else if (index as f32) < second {
                b = value(b, steps);
            } else {
                if index as f32 == second {
                    a = combine(a, b);
                }
                a = value(a, steps);
            }
        }
        if second == count {
            a = combine(a, b);
        }
        a
    }

    #[test]
    fn paired_chains_keep_unclamped_intermediates_operand_order_and_dynamic_uniforms() {
        let constant = |value: f32| MaskOp::Constant(value.to_bits());
        for join in [
            MaskOp::Add(10, 12),
            MaskOp::Subtract(10, 12),
            MaskOp::Multiply(10, 12),
        ] {
            let graph = MaskProgram::new(
                vec![
                    constant(0.),
                    constant(1.),
                    MaskOp::Parameter(0),
                    MaskOp::Parameter(2),
                    MaskOp::Sample {
                        image: 0,
                        channel: MaskChannel::Gray,
                        coordinates: [3, 0, 1, 1],
                        repeat: 1,
                    },
                    MaskOp::Subtract(2, 4),
                    constant(3.),
                    MaskOp::Multiply(5, 6),
                    MaskOp::Add(7, 1),
                    MaskOp::Clamp(8, 0, 1_f32.to_bits()),
                    MaskOp::Pow(9, 2_f32.to_bits()),
                    MaskOp::Sample {
                        image: 1,
                        channel: MaskChannel::Alpha,
                        coordinates: [0, 0, 1, 1],
                        repeat: 2,
                    },
                    MaskOp::Subtract(0, 11),
                    join,
                    MaskOp::Smoothstep(1, 0, 13),
                    MaskOp::Pow(14, 0),
                ],
                vec!["a".into(), "b".into()],
            )
            .unwrap();
            let plan = Plan::new(&graph).unwrap();
            for progress in [-0., 0.35, 1., f32::MAX] {
                let params = [progress, 0.2, -0.25, 0.75, 0.];
                let folded = plan.evaluate(&graph, params).unwrap();
                assert_eq!(folded.coordinates, [[-0.25, 0., 1., 1.], [0., 0., 1., 1.]]);
                for alpha in [0., 0.37, 1.] {
                    for gray in [0., 0.73, 1.] {
                        let mut expected = Vec::new();
                        for op in graph.operations() {
                            expected.push(operation_value(op, params, alpha, gray, |n| {
                                expected[n as usize]
                            }));
                        }
                        let first = value(gray, folded.chains[0]);
                        let second = value(alpha, folded.chains[1]);
                        assert_eq!(first.to_bits(), expected[10].to_bits());
                        assert_eq!(second.to_bits(), expected[12].to_bits());
                        let joined = match plan.join {
                            1 => first + second,
                            2 => first - second,
                            3 => first * second,
                            _ => unreachable!(),
                        };
                        assert_eq!(joined.to_bits(), expected[13].to_bits());
                        let mut intermediate = joined;
                        for (index, step) in folded.chains[2]
                            .iter()
                            .filter(|step| step[0] != 0.)
                            .enumerate()
                        {
                            let mut one = [[0.; 4]; STEPS];
                            one[0] = *step;
                            intermediate = value(intermediate, one);
                            assert_eq!(intermediate.to_bits(), expected[14 + index].to_bits());
                        }
                        assert_eq!(
                            value(joined, folded.chains[2]).to_bits(),
                            expected.last().unwrap().to_bits()
                        );
                        assert_eq!(
                            streamed(&folded, plan.join, gray, alpha).to_bits(),
                            expected.last().unwrap().to_bits()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn dependent_coordinates_and_multiple_joins_keep_the_complete_graph_path() {
        let mut ops = vec![
            MaskOp::Constant(0),
            MaskOp::Constant(1_f32.to_bits()),
            MaskOp::Sample {
                image: 0,
                channel: MaskChannel::Alpha,
                coordinates: [0, 0, 1, 1],
                repeat: 0,
            },
            MaskOp::Sample {
                image: 0,
                channel: MaskChannel::Gray,
                coordinates: [2, 0, 1, 1],
                repeat: 0,
            },
            MaskOp::Multiply(2, 3),
        ];
        let graph = MaskProgram::new(ops.clone(), vec!["a".into()]).unwrap();
        assert!(Plan::new(&graph).is_none());
        ops[3] = MaskOp::Sample {
            image: 0,
            channel: MaskChannel::Gray,
            coordinates: [0, 0, 1, 1],
            repeat: 0,
        };
        ops.push(MaskOp::Add(4, 2));
        let graph = MaskProgram::new(ops, vec!["a".into()]).unwrap();
        assert!(Plan::new(&graph).is_none());
    }

    #[test]
    fn paired_uniform_overflow_and_arithmetic_capacity_keep_the_complete_graph_path() {
        let mut ops = vec![
            MaskOp::Constant(0),
            MaskOp::Constant(1_f32.to_bits()),
            MaskOp::Parameter(0),
            MaskOp::Constant(2_f32.to_bits()),
            MaskOp::Multiply(2, 3),
            MaskOp::Sample {
                image: 0,
                channel: MaskChannel::Alpha,
                coordinates: [4, 0, 1, 1],
                repeat: 0,
            },
            MaskOp::Sample {
                image: 0,
                channel: MaskChannel::Gray,
                coordinates: [0, 0, 1, 1],
                repeat: 0,
            },
            MaskOp::Multiply(5, 6),
        ];
        for _ in 0..MATH {
            ops.push(MaskOp::Add(ops.len() as u32 - 1, 1));
        }
        let graph = MaskProgram::new(ops.clone(), vec!["a".into()]).unwrap();
        let plan = Plan::new(&graph).unwrap();
        assert!(plan.evaluate(&graph, [f32::MAX, 0., 0., 0., 0.]).is_none());
        assert!(plan.evaluate(&graph, [0.25, 0., 0., 0., 0.]).is_some());
        ops.push(MaskOp::Add(ops.len() as u32 - 1, 1));
        let graph = MaskProgram::new(ops, vec!["a".into()]).unwrap();
        assert!(Plan::new(&graph).is_none());
    }

    #[test]
    fn empty_chain_phases_join_once_without_clamping_or_reassociation() {
        for lengths in [
            [0, 0, 0],
            [1, 0, 0],
            [0, 1, 0],
            [0, 0, 1],
            [1, 1, 1],
            [8, 0, 0],
        ] {
            for join in 1..=3 {
                let mut input = Values {
                    coordinates: [[0.; 4]; 2],
                    chains: [[[0.; 4]; STEPS]; 3],
                };
                for (steps, count) in input.chains.iter_mut().zip(lengths) {
                    for step in steps.iter_mut().take(count) {
                        *step = [3., 0.25, 0., 0.];
                    }
                }
                let a = value(-0.0, input.chains[0]);
                let b = value(1.25, input.chains[1]);
                let expected = value(
                    match join {
                        1 => a + b,
                        2 => a - b,
                        3 => a * b,
                        _ => unreachable!(),
                    },
                    input.chains[2],
                );
                assert_eq!(
                    streamed(&input, join, -0.0, 1.25).to_bits(),
                    expected.to_bits()
                );
            }
        }
    }
}
