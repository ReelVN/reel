//! Bounded recognition of masks that need one sample and an ordered scalar chain.
#![cfg_attr(
    not(any(test, all(feature = "gles2", feature = "wgpu"))),
    allow(
        dead_code,
        reason = "WGPU shares threshold classification; GLES also consumes the evaluated scalar chain"
    )
)]
use reel_ui::stage::{MaskChannel, MaskOp, MaskProgram};

const NODES: usize = 32;
pub(crate) const STEPS: usize = 8;

#[derive(Clone, Copy)]
enum Step {
    Add(usize),
    Subtract(usize),
    SubtractFrom(usize),
    Multiply(usize),
    Clamp(u32, u32),
}

pub(crate) enum Plan {
    ImageThreshold { alpha: usize, gray: usize },
    SampleChain(Chain),
}
pub(crate) struct Chain {
    sample: usize,
    steps: [Option<Step>; STEPS],
}
pub(crate) struct Evaluated {
    pub image: usize,
    pub channel: MaskChannel,
    pub coordinates: [f32; 4],
    pub repeat: u8,
    pub steps: [[f32; 4]; STEPS],
    pub threshold: bool,
}

/// Recognize an internal optimization subset; every other valid DAG stays on
/// its backend's general path. Keep each sample-dependent operation in order.
pub(crate) fn plan(program: &MaskProgram) -> Option<Plan> {
    let operations = program.operations();
    if program.is_image_threshold() {
        let MaskOp::ImageThreshold { alpha, gray } = *operations.last()? else {
            return None;
        };
        let (alpha, gray) = (alpha as usize, gray as usize);
        let MaskOp::Sample {
            image: a,
            channel: MaskChannel::Alpha,
            coordinates: ac,
            repeat: ar,
        } = operations[alpha]
        else {
            return None;
        };
        let MaskOp::Sample {
            image: b,
            channel: MaskChannel::Gray,
            coordinates: bc,
            repeat: br,
        } = operations[gray]
        else {
            return None;
        };
        return (a == b && ac == bc && ar == br).then_some(Plan::ImageThreshold { alpha, gray });
    }
    if operations.len() > NODES {
        return None;
    }
    let mut uniform = [false; NODES];
    for (index, operation) in operations.iter().enumerate() {
        uniform[index] = match *operation {
            MaskOp::Constant(_) | MaskOp::Parameter(_) => true,
            MaskOp::Add(a, b) | MaskOp::Subtract(a, b) | MaskOp::Multiply(a, b) => {
                uniform[a as usize] && uniform[b as usize]
            }
            MaskOp::Clamp(a, _, _) => uniform[a as usize],
            _ => false,
        };
    }
    let mut next = operations.len().checked_sub(1)?;
    let mut steps = [None; STEPS];
    let mut count = 0;
    loop {
        if uniform[next] {
            return None;
        }
        let operation = &operations[next];
        if let MaskOp::Sample { coordinates, .. } = operation {
            if !coordinates.iter().all(|&node| uniform[node as usize]) {
                return None;
            }
            steps[..count].reverse();
            return Some(Plan::SampleChain(Chain {
                sample: next,
                steps,
            }));
        }
        let (input, step) = match *operation {
            MaskOp::Add(a, b) if uniform[b as usize] => (a, Step::Add(b as usize)),
            MaskOp::Add(a, b) if uniform[a as usize] => (b, Step::Add(a as usize)),
            MaskOp::Subtract(a, b) if uniform[b as usize] => (a, Step::Subtract(b as usize)),
            MaskOp::Subtract(a, b) if uniform[a as usize] => (b, Step::SubtractFrom(a as usize)),
            MaskOp::Multiply(a, b) if uniform[b as usize] => (a, Step::Multiply(b as usize)),
            MaskOp::Multiply(a, b) if uniform[a as usize] => (b, Step::Multiply(a as usize)),
            MaskOp::Clamp(a, low, high) => (a, Step::Clamp(low, high)),
            _ => return None,
        };
        *steps.get_mut(count)? = Some(step);
        count += 1;
        next = input as usize;
    }
}

impl Plan {
    /// Fold only uniform arithmetic, with the original float32 operation order.
    /// Non-finite intermediates retain the original GPU evaluation path.
    pub(crate) fn evaluate(
        &self,
        program: &MaskProgram,
        parameters: [f32; 5],
    ) -> Option<Evaluated> {
        let operations = program.operations();
        if operations.len() > NODES {
            return None;
        }
        let mut values = [None; NODES];
        for (index, operation) in operations.iter().enumerate() {
            let read = |a: u32| values[a as usize];
            values[index] = match *operation {
                MaskOp::Constant(bits) => Some(f32::from_bits(bits)),
                MaskOp::Parameter(p) => Some(parameters[p as usize]),
                MaskOp::Add(a, b) => read(a).zip(read(b)).map(|(a, b)| a + b),
                MaskOp::Subtract(a, b) => read(a).zip(read(b)).map(|(a, b)| a - b),
                MaskOp::Multiply(a, b) => read(a).zip(read(b)).map(|(a, b)| a * b),
                MaskOp::Clamp(a, low, high) => {
                    read(a).map(|a: f32| a.clamp(f32::from_bits(low), f32::from_bits(high)))
                }
                _ => None,
            }
            .filter(|value| value.is_finite());
        }
        let sample = match self {
            Self::ImageThreshold { alpha, .. } => *alpha,
            Self::SampleChain(chain) => chain.sample,
        };
        let MaskOp::Sample {
            image,
            channel,
            coordinates,
            repeat,
        } = operations[sample]
        else {
            return None;
        };
        let mut output = Evaluated {
            image: image as usize,
            channel,
            coordinates: [0.; 4],
            repeat,
            steps: [[0.; 4]; STEPS],
            threshold: matches!(self, Self::ImageThreshold { .. }),
        };
        for (output, node) in output.coordinates.iter_mut().zip(coordinates) {
            *output = values[node as usize]?;
        }
        if let Self::SampleChain(chain) = self {
            for (output, step) in output.steps.iter_mut().zip(chain.steps) {
                *output = match step {
                    None => break,
                    Some(Step::Add(a)) => [1., values[a]?, 0., 0.],
                    Some(Step::Subtract(a)) => [2., values[a]?, 0., 0.],
                    Some(Step::SubtractFrom(a)) => [3., values[a]?, 0., 0.],
                    Some(Step::Multiply(a)) => [4., values[a]?, 0., 0.],
                    Some(Step::Clamp(low, high)) => {
                        [5., f32::from_bits(low), f32::from_bits(high), 0.]
                    }
                };
            }
        }
        Some(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(value: f32) -> MaskOp {
        MaskOp::Constant(value.to_bits())
    }
    fn sample(channel: MaskChannel) -> MaskOp {
        MaskOp::Sample {
            image: 0,
            channel,
            coordinates: [0, 0, 1, 1],
            repeat: 0,
        }
    }
    fn oracle(program: &MaskProgram, parameters: [f32; 5], alpha: f32, gray: f32) -> f32 {
        let mut nodes: Vec<f32> = Vec::new();
        for operation in program.operations() {
            let at = |index: u32| nodes[index as usize];
            nodes.push(match *operation {
                MaskOp::Constant(bits) => f32::from_bits(bits),
                MaskOp::Parameter(index) => parameters[index as usize],
                MaskOp::Sample { channel, .. } => match channel {
                    MaskChannel::Alpha => alpha,
                    MaskChannel::Gray => gray,
                },
                MaskOp::Add(a, b) => at(a) + at(b),
                MaskOp::Subtract(a, b) => at(a) - at(b),
                MaskOp::Multiply(a, b) => at(a) * at(b),
                MaskOp::Clamp(a, low, high) => {
                    at(a).clamp(f32::from_bits(low), f32::from_bits(high))
                }
                MaskOp::ImageThreshold { alpha, gray } => {
                    if parameters[0] <= 0. {
                        0.
                    } else if parameters[0] >= 1. {
                        1.
                    } else {
                        let gray = if parameters[4] != 0. {
                            1. - at(gray)
                        } else {
                            at(gray)
                        };
                        at(alpha)
                            * if parameters[1] > 0. {
                                ((parameters[0] * (1. + parameters[1]) - gray) / parameters[1])
                                    .clamp(0., 1.)
                            } else {
                                f32::from(gray <= parameters[0])
                            }
                    }
                }
                _ => panic!("unexpected oracle operation"),
            });
        }
        nodes.last().unwrap().clamp(0., 1.)
    }
    fn chain_value(input: &Evaluated, mut sample: f32) -> f32 {
        for [operation, operand, high, _] in input.steps {
            sample = match operation as u8 {
                0 => sample,
                1 => sample + operand,
                2 => sample - operand,
                3 => operand - sample,
                4 => sample * operand,
                5 => sample.clamp(operand, high),
                _ => unreachable!(),
            };
        }
        sample.clamp(0., 1.)
    }

    #[test]
    fn uniform_folding_preserves_sample_chain_order_and_coordinates() {
        let program = MaskProgram::new(
            vec![
                constant(0.),
                constant(1.),
                MaskOp::Parameter(0),
                MaskOp::Parameter(2),
                MaskOp::Sample {
                    image: 0,
                    channel: MaskChannel::Gray,
                    coordinates: [3, 0, 1, 1],
                    repeat: 3,
                },
                constant(2.),
                MaskOp::Multiply(2, 5),
                MaskOp::Subtract(6, 1),
                MaskOp::Add(4, 7),
                MaskOp::Clamp(8, 0_f32.to_bits(), 1_f32.to_bits()),
                MaskOp::Subtract(1, 9),
                MaskOp::Multiply(10, 5),
                MaskOp::Clamp(11, 0.1_f32.to_bits(), 0.8_f32.to_bits()),
            ],
            vec!["mask".into()],
        )
        .unwrap();
        let plan = plan(&program).unwrap();
        for progress in [0., 0.1, 0.5, 0.9, 1.] {
            let parameters = [progress, 0., 0.125, 0., 0.];
            let evaluated = plan.evaluate(&program, parameters).unwrap();
            assert_eq!(evaluated.coordinates, [0.125, 0., 1., 1.]);
            assert_eq!(evaluated.repeat, 3);
            assert_eq!(evaluated.image, 0);
            assert_eq!(evaluated.channel, MaskChannel::Gray);
            assert!(!evaluated.threshold);
            for sample in [-0.1, 0., 0.2, 0.6, 1., 2.] {
                assert_eq!(
                    chain_value(&evaluated, sample).to_bits(),
                    oracle(&program, parameters, 1., sample).to_bits()
                );
            }
        }
        // Reassociating this chain into scale*sample+bias would return sample.
        let cancellation = MaskProgram::new(
            vec![
                constant(0.),
                constant(1.),
                sample(MaskChannel::Alpha),
                constant(16777216.),
                MaskOp::Add(2, 3),
                MaskOp::Subtract(4, 3),
            ],
            vec!["mask".into()],
        )
        .unwrap();
        let evaluated = super::plan(&cancellation)
            .unwrap()
            .evaluate(&cancellation, [0.; 5])
            .unwrap();
        assert_eq!(chain_value(&evaluated, 0.25), 0.);
        assert_eq!(
            chain_value(&evaluated, 0.25),
            oracle(&cancellation, [0.; 5], 0.25, 0.)
        );
    }

    #[test]
    fn threshold_plan_preserves_alpha_gray_sampling_and_progress_endpoints() {
        let program = MaskProgram::image("mask".into());
        let plan = plan(&program).unwrap();
        assert!(matches!(plan, Plan::ImageThreshold { alpha: 2, gray: 3 }));
        let evaluated = plan.evaluate(&program, [0.; 5]).unwrap();
        assert!(evaluated.threshold);
        assert_eq!(evaluated.coordinates, [0., 0., 1., 1.]);
        assert_eq!(evaluated.repeat, 0);
        for alpha in [0_f32, 0.25, 1.] {
            for gray in [0., 0.4, 1.] {
                for progress in [-0.1, 0., 0.4, 1., 1.1] {
                    for softness in [0., 0.2] {
                        for invert in [0., 1.] {
                            let parameters: [f32; 5] = [progress, softness, 0., 0., invert];
                            let result = if progress <= 0. {
                                0.
                            } else if progress >= 1. {
                                1.
                            } else {
                                let gray = if invert != 0. { 1. - gray } else { gray };
                                alpha
                                    * if softness > 0. {
                                        ((progress * (1. + softness) - gray) / softness)
                                            .clamp(0., 1.)
                                    } else {
                                        f32::from(gray <= progress)
                                    }
                            };
                            assert_eq!(
                                result.to_bits(),
                                oracle(&program, parameters, alpha, gray).to_bits()
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn nonlinear_multiple_sample_and_long_chains_retain_general_dag() {
        let base = vec![constant(0.), constant(1.), sample(MaskChannel::Alpha)];
        for tail in [
            vec![MaskOp::Pow(2, 2_f32.to_bits())],
            vec![sample(MaskChannel::Gray), MaskOp::Multiply(2, 3)],
            vec![MaskOp::Sample {
                image: 0,
                channel: MaskChannel::Alpha,
                coordinates: [2, 0, 1, 1],
                repeat: 0,
            }],
        ] {
            let mut operations = base.clone();
            operations.extend(tail);
            let program = MaskProgram::new(operations, vec!["mask".into()]).unwrap();
            assert!(plan(&program).is_none());
        }
        let mut operations = base.clone();
        for _ in 0..STEPS {
            operations.push(MaskOp::Add(operations.len() as u32 - 1, 1));
        }
        let program = MaskProgram::new(operations.clone(), vec!["mask".into()]).unwrap();
        assert!(plan(&program).is_some());
        operations.push(MaskOp::Add(operations.len() as u32 - 1, 1));
        let program = MaskProgram::new(operations, vec!["mask".into()]).unwrap();
        assert!(plan(&program).is_none());
        let mut operations = base;
        operations.extend([
            constant(f32::MAX),
            constant(2.),
            MaskOp::Multiply(3, 4),
            MaskOp::Add(2, 5),
        ]);
        let program = MaskProgram::new(operations, vec!["mask".into()]).unwrap();
        assert!(
            plan(&program)
                .unwrap()
                .evaluate(&program, [0.; 5])
                .is_none()
        );
    }
}
