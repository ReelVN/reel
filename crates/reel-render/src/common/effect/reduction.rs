//! Recover bounded sampling reductions from the expanded typed graph. Every lane
//! retains its initial value and left-to-right additions. A loop is admitted only
//! when the sampled subgraphs differ by one exactly representable integer float
//! constant whose complete original sequence matches the loop counter.

use super::*;

/// Keep compact sampling expressions unrolled. Larger proved reductions use a
/// fixed loop, bounding the compiler's simultaneous coordinate temporaries.
const UNROLLED_SAMPLES: usize = 16;

#[derive(Clone)]
pub(super) struct Reduction {
    pub initial: [EffectNodeId; 4],
    pub sample: EffectNodeId,
    pub constant: EffectNodeId,
    pub first: i32,
    pub end: i32,
    pub step: i32,
    /// All induction values, including the terminal value and increment, are
    /// exactly representable as f32. Each addition then reproduces the next
    /// literal without a float/integer conversion in the sampling loop.
    pub float_counter: bool,
    pub scale: Vec<(EffectBinary, EffectNodeId)>,
    invariant: Vec<EffectNodeId>,
    pub dependent: BTreeSet<u32>,
}
impl Reduction {
    pub fn iterations(&self) -> usize {
        ((i64::from(self.end) - i64::from(self.first)) / i64::from(self.step)) as usize
    }
    /// Four or five ordered taps amortize branch control with bounded liveness.
    /// Keep short reductions, integer-conversion boundaries and complex/branched
    /// coordinate bodies on the original single-tap loop.
    pub fn block_width(&self, program: &EffectProgram) -> usize {
        if self.float_counter
            && self.iterations() >= 32
            && self.dependent.len() <= 8
            && self.small_invariants(program)
            && matches!(
                program.nodes()[self.sample.0 as usize].op,
                EffectOp::SourceSample { .. }
            )
            && self.dependent.iter().all(|&id| {
                if matches!(
                    program.nodes()[id as usize].op,
                    EffectOp::SourceSample { .. }
                ) {
                    return id == self.sample.0;
                }
                matches!(
                    program.nodes()[id as usize].op,
                    EffectOp::Constant(EffectValue::Float(_))
                        | EffectOp::Extract { .. }
                        | EffectOp::Vec2(_)
                        | EffectOp::Binary {
                            op: EffectBinary::Add
                                | EffectBinary::Subtract
                                | EffectBinary::Multiply
                                | EffectBinary::Divide,
                            ..
                        }
                )
            })
        {
            // Five taps reduce loop control and expose more independent reads.
            // Keep four when five would enlarge the straight-line remainder.
            // Wider blocks substantially increase Sony Cg temporary registers.
            if self.iterations() % 5 <= self.iterations() % 4 {
                5
            } else {
                4
            }
        } else {
            1
        }
    }
    fn small_invariants(&self, program: &EffectProgram) -> bool {
        let mut visited = BTreeSet::new();
        let mut pending: Vec<_> = self.dependencies().collect();
        let mut components = 0;
        while let Some(id) = pending.pop() {
            if !visited.insert(id.0) {
                continue;
            }
            let node = &program.nodes()[id.0 as usize];
            components += match node.ty {
                EffectType::Bool | EffectType::Float => 1,
                EffectType::Vec2 => 2,
                EffectType::Vec3 => 3,
                EffectType::Vec4 => 4,
            };
            if components > 16
                || !matches!(
                    node.op,
                    EffectOp::Constant(_)
                        | EffectOp::Input(_)
                        | EffectOp::Parameter(_)
                        | EffectOp::Extract { .. }
                        | EffectOp::Vec2(_)
                        | EffectOp::Vec3(_)
                        | EffectOp::Vec4(_)
                        | EffectOp::Binary {
                            op: EffectBinary::Add
                                | EffectBinary::Subtract
                                | EffectBinary::Multiply
                                | EffectBinary::Divide,
                            ..
                        }
                )
            {
                return false;
            }
            pending.extend(dependencies(&node.op));
        }
        true
    }
    pub fn dependencies(&self) -> impl Iterator<Item = EffectNodeId> + '_ {
        self.initial
            .into_iter()
            .chain(self.scale.iter().map(|(_, id)| *id))
            .chain(self.invariant.iter().copied())
    }

    pub fn find(program: &EffectProgram, output: EffectNodeId) -> Option<Self> {
        let EffectOp::Vec4(mut lanes) = program.nodes()[output.0 as usize].op else {
            return None;
        };
        let mut scale = Vec::new();
        // Identical componentwise scaling stays after the complete reduction;
        // it is never distributed through the additions or reassociated.
        loop {
            let ops = lanes.map(|id| &program.nodes()[id.0 as usize].op);
            let EffectOp::Binary { op, right, .. } = *ops[0] else {
                break;
            };
            if !matches!(op, EffectBinary::Divide | EffectBinary::Multiply) {
                break;
            }
            let mut next = lanes;
            let mut matched = true;
            for (lane, operation) in ops.into_iter().enumerate() {
                if let EffectOp::Binary {
                    op: candidate,
                    left,
                    right: denominator,
                } = *operation
                    && candidate == op
                    && denominator == right
                {
                    next[lane] = left;
                } else {
                    matched = false;
                    break;
                }
            }
            if !matched {
                break;
            }
            scale.push((op, right));
            lanes = next;
        }
        let mut samples = Vec::new();
        loop {
            let mut previous = lanes;
            let mut sample = None;
            let mut matched = true;
            for (lane, id) in lanes.into_iter().enumerate() {
                let EffectOp::Binary {
                    op: EffectBinary::Add,
                    left,
                    right,
                } = program.nodes()[id.0 as usize].op
                else {
                    matched = false;
                    break;
                };
                let EffectOp::Extract {
                    value,
                    lane: component,
                } = program.nodes()[right.0 as usize].op
                else {
                    matched = false;
                    break;
                };
                if component as usize != lane || sample.is_some_and(|existing| existing != value) {
                    matched = false;
                    break;
                }
                previous[lane] = left;
                sample = Some(value);
            }
            if !matched {
                break;
            }
            let sample = sample?;
            if !matches!(
                program.nodes()[sample.0 as usize].op,
                EffectOp::SourceSample { .. }
                    | EffectOp::TextureSample {
                        channel: EffectTextureChannel::Color,
                        ..
                    }
            ) {
                break;
            }
            samples.push(sample);
            lanes = previous;
        }
        if samples.len() <= UNROLLED_SAMPLES {
            return None;
        }
        samples.reverse();
        let sample = samples[0];
        let mut replacements = Vec::with_capacity(samples.len());
        let mut varying = None;
        for &candidate in &samples {
            let changed = differences(program, sample, candidate)?;
            for &id in changed.keys() {
                if varying.is_some_and(|existing| existing != id) {
                    return None;
                }
                varying = Some(id);
            }
            replacements.push(changed);
        }
        let constant = EffectNodeId(varying?);
        let EffectOp::Constant(EffectValue::Float(original)) =
            program.nodes()[constant.0 as usize].op
        else {
            return None;
        };
        let integers = replacements
            .iter()
            .map(|changes| {
                let bits = *changes.get(&constant.0).unwrap_or(&original);
                let value = f32::from_bits(bits);
                if value.abs() > 16_777_216.0 {
                    return None;
                }
                let integer = value as i32;
                ((integer as f32).to_bits() == bits).then_some(integer)
            })
            .collect::<Option<Vec<_>>>()?;
        let first = integers[0];
        let step = integers[1].checked_sub(first)?;
        if step == 0 {
            return None;
        }
        // Check every original constant, rather than infer float arithmetic from
        // the endpoints. Conversion of each exact integer reproduces its bits.
        for (index, &value) in integers.iter().enumerate() {
            if first.checked_add(step.checked_mul(index as i32)?)? != value {
                return None;
            }
        }
        let end = integers.last()?.checked_add(step)?;
        scale.reverse();
        let (invariant, dependent) = invariants(program, sample, &[constant]);
        Some(Self {
            initial: lanes,
            sample,
            constant,
            first,
            end,
            step,
            float_counter: [step, end]
                .into_iter()
                .all(|value| f64::from(value as f32) == f64::from(value)),
            scale,
            invariant,
            dependent,
        })
    }
}

/// Match typed operations and edge positions, recording only changed float
/// literals. Shared IDs are already the same expression; no algebraic matching
/// or commutation can change the source operation order.
pub(super) fn differences(
    program: &EffectProgram,
    template: EffectNodeId,
    candidate: EffectNodeId,
) -> Option<BTreeMap<u32, u32>> {
    let mut changed = BTreeMap::new();
    let mut pending = vec![(template, candidate)];
    let mut visited = BTreeSet::new();
    while let Some((a, b)) = pending.pop() {
        if !visited.insert((a.0, b.0)) {
            continue;
        }
        let a_node = &program.nodes()[a.0 as usize];
        let b_node = &program.nodes()[b.0 as usize];
        if a_node.ty != b_node.ty {
            return None;
        }
        let matched = match (&a_node.op, &b_node.op) {
            (
                EffectOp::Constant(EffectValue::Float(_)),
                EffectOp::Constant(EffectValue::Float(y)),
            ) => {
                if changed
                    .insert(a.0, *y)
                    .is_some_and(|previous| previous != *y)
                {
                    return None;
                }
                true
            }
            (EffectOp::Constant(x), EffectOp::Constant(y)) => x == y,
            (EffectOp::Parameter(x), EffectOp::Parameter(y)) => x == y,
            (EffectOp::Input(x), EffectOp::Input(y)) => x == y,
            (EffectOp::SourceSample { .. }, EffectOp::SourceSample { .. })
            | (EffectOp::Vec2(_), EffectOp::Vec2(_))
            | (EffectOp::Vec3(_), EffectOp::Vec3(_))
            | (EffectOp::Vec4(_), EffectOp::Vec4(_))
            | (EffectOp::Select { .. }, EffectOp::Select { .. }) => true,
            (
                EffectOp::TextureSample {
                    binding: a,
                    channel: x,
                    ..
                },
                EffectOp::TextureSample {
                    binding: b,
                    channel: y,
                    ..
                },
            ) => a == b && x == y,
            (EffectOp::Extract { lane: x, .. }, EffectOp::Extract { lane: y, .. }) => x == y,
            (EffectOp::Unary { op: x, .. }, EffectOp::Unary { op: y, .. }) => x == y,
            (EffectOp::Binary { op: x, .. }, EffectOp::Binary { op: y, .. }) => x == y,
            (EffectOp::Ternary { op: x, .. }, EffectOp::Ternary { op: y, .. }) => x == y,
            _ => false,
        };
        if !matched {
            return None;
        }
        pending.extend(
            dependencies(&a_node.op)
                .into_iter()
                .zip(dependencies(&b_node.op)),
        );
    }
    changed.retain(|id, bits| !matches!(program.nodes()[*id as usize].op, EffectOp::Constant(EffectValue::Float(original)) if original == *bits));
    Some(changed)
}

/// Hoist only unconditional invariant inputs. Branch-local source reads and
/// undefined arithmetic keep their selected scope inside the loop body.
pub(super) fn invariants(
    program: &EffectProgram,
    sample: EffectNodeId,
    literals: &[EffectNodeId],
) -> (Vec<EffectNodeId>, BTreeSet<u32>) {
    let mut reachable = BTreeSet::new();
    let mut pending = vec![sample];
    while let Some(id) = pending.pop() {
        if reachable.insert(id.0) {
            pending.extend(dependencies(&program.nodes()[id.0 as usize].op));
        }
    }
    let mut dependent = BTreeSet::new();
    for id in reachable {
        if literals.contains(&EffectNodeId(id))
            || dependencies(&program.nodes()[id as usize].op)
                .iter()
                .any(|input| dependent.contains(&input.0))
        {
            dependent.insert(id);
        }
    }
    let mut invariant = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut pending = vec![sample];
    while let Some(id) = pending.pop() {
        if literals.contains(&id) || !visited.insert(id.0) {
            continue;
        }
        if !dependent.contains(&id.0) {
            invariant.insert(id.0);
        } else if let EffectOp::Select { condition, .. } = program.nodes()[id.0 as usize].op {
            pending.push(condition);
        } else {
            pending.extend(dependencies(&program.nodes()[id.0 as usize].op));
        }
    }
    (invariant.into_iter().map(EffectNodeId).collect(), dependent)
}
