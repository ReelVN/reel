//! Keep uniform feature branches and predicate short, spatially varying choices.
use super::*;

pub(super) fn varying(program: &EffectProgram) -> Vec<bool> {
    let mut values = Vec::with_capacity(program.nodes().len());
    for node in program.nodes() {
        values.push(match node.op {
            EffectOp::Input(EffectInput::Uv | EffectInput::Position) => true,
            _ => dependencies(&node.op)
                .iter()
                .any(|id| values[id.0 as usize]),
        });
    }
    values
}

impl Emitter<'_> {
    /// Values required by both arms can be evaluated once before a uniform
    /// branch. Traversal stops at nested choices unless both arms demand the
    /// choice itself, so conditional texture reads stay inside their branches.
    pub(super) fn common_arm_dependencies(
        &mut self,
        scope: usize,
        condition: EffectNodeId,
        when_true: EffectNodeId,
        when_false: EffectNodeId,
    ) -> Vec<EffectNodeId> {
        if self.language != Language::Wgsl
            || self.varying[condition.0 as usize]
            || self.scopes[scope].substituted
        {
            return Vec::new();
        }
        let accept = self.required_values(scope, when_true);
        let reject = self.required_values(scope, when_false);
        accept
            .intersection(&reject)
            .copied()
            .map(EffectNodeId)
            .collect()
    }

    fn required_values(&mut self, scope: usize, root: EffectNodeId) -> BTreeSet<u32> {
        let mut required = BTreeSet::new();
        let mut pending = vec![root];
        while let Some(id) = pending.pop() {
            if self.lookup(scope, id).is_some() || !required.insert(id.0) {
                continue;
            }
            if let Some(reduction) = self.reduction(scope, id) {
                pending.extend(reduction.dependencies());
            } else if let Some(reduction) = self.scalar_reduction(scope, id) {
                pending.extend(reduction.dependencies());
            } else {
                let op = &self.program.nodes()[id.0 as usize].op;
                if let EffectOp::Select { condition, .. } = *op {
                    pending.push(condition);
                } else {
                    pending.extend(dependencies(op));
                }
            }
        }
        required
    }

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
            pending.extend(dependencies(op));
        }
        true
    }
}
