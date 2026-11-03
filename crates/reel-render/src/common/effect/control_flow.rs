//! Keep uniform feature branches and predicate short, spatially varying choices.
use super::*;

pub(super) fn varying(program: &EffectProgram) -> Vec<bool> {
    let mut values = Vec::with_capacity(program.nodes().len());
    for node in program.nodes() {
        values.push(match node.op {
            EffectOp::Input(EffectInput::Uv | EffectInput::Position) => true,
            _ => inputs(&node.op).iter().any(|id| values[id.0 as usize]),
        });
    }
    values
}

fn inputs(op: &EffectOp) -> Vec<EffectNodeId> {
    match *op {
        EffectOp::Select {
            condition,
            when_true,
            when_false,
        } => vec![condition, when_true, when_false],
        _ => dependencies(op),
    }
}

impl Emitter<'_> {
    pub(super) fn flatten_selection(
        &self,
        scope: usize,
        condition: EffectNodeId,
        arms: [EffectNodeId; 2],
    ) -> bool {
        if self.language != Language::Cg || !self.varying[condition.0 as usize] {
            return false;
        }
        let mut pending = arms.to_vec();
        let mut visited = BTreeSet::new();
        let mut cost = 0;
        while let Some(id) = pending.pop() {
            if self.lookup(scope, id).is_some() || !visited.insert(id.0) {
                continue;
            }
            let op = &self.program.nodes()[id.0 as usize].op;
            cost += match op {
                EffectOp::SourceSample { .. } | EffectOp::TextureSample { .. } => 32,
                _ => 1,
            };
            // Long sampling branches keep their lazy work and register lifetime.
            if cost > 64 {
                return false;
            }
            pending.extend(inputs(op));
        }
        true
    }
}
