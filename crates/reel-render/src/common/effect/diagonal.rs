//! A single unchanged coordinate lane of an ordered source reduction.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct Proof {
    pub sample: EffectNodeId,
    pub fixed: EffectNodeId,
    pub axis: u8,
}

impl Proof {
    pub fn find(program: &EffectProgram) -> Option<Self> {
        // Retain the original source-coordinate contract, including its finite
        // logical-pixel offsets. This is not an inference from the halo alone.
        source_footprint(program)?;
        let reduction = reduction::Reduction::find(program, program.output())?;
        let EffectOp::SourceSample { uv } = program.nodes()[reduction.sample.0 as usize].op else {
            return None;
        };
        let EffectOp::Vec2(lanes) = program.nodes()[uv.0 as usize].op else {
            return None;
        };
        // Initial values and final scaling must not reference a source sample:
        // the fixed-axis local is introduced only at this reduction's entry.
        let mut pending: Vec<_> = reduction.dependencies().collect();
        let mut visited = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id.0) {
                continue;
            }
            let op = &program.nodes()[id.0 as usize].op;
            if matches!(op, EffectOp::SourceSample { .. }) {
                return None;
            }
            pending.extend(dependencies(op));
        }
        for axis in 0..2u8 {
            let fixed = lanes[axis as usize];
            if reduction.dependent.contains(&fixed.0)
                || !reduction.dependent.contains(&lanes[1 - axis as usize].0)
            {
                continue;
            }
            let EffectOp::Extract { value, lane } = program.nodes()[fixed.0 as usize].op else {
                continue;
            };
            if lane == axis
                && matches!(
                    program.nodes()[value.0 as usize].op,
                    EffectOp::Input(EffectInput::Uv)
                )
            {
                return Some(Self {
                    sample: reduction.sample,
                    fixed,
                    axis,
                });
            }
        }
        None
    }
}
