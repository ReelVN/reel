//! Recover ordered scalar sampling loops from expanded sums of weighted taps.
use super::*;
use std::fmt::Write;

const MIN_TAPS: usize = 17;
const MAX_TAPS: usize = 64;
const MAX_LITERALS: usize = 4;

pub(super) struct ScalarReduction {
    initial: EffectNodeId,
    term: EffectNodeId,
    literals: Vec<(EffectNodeId, Vec<u32>)>,
    invariant: Vec<EffectNodeId>,
    dependent: BTreeSet<u32>,
}

impl ScalarReduction {
    pub(super) fn find(program: &EffectProgram, output: EffectNodeId) -> Option<Self> {
        let nodes = program.nodes();
        let mut terms = SmallVec::<[EffectNodeId; MIN_TAPS]>::new();
        let mut initial = output;
        while terms.len() < MAX_TAPS {
            let node = &nodes[initial.0 as usize];
            let EffectOp::Binary {
                op: EffectBinary::Add,
                left,
                right,
            } = node.op
            else {
                break;
            };
            if node.ty != EffectType::Float {
                break;
            }
            terms.push(right);
            initial = left;
        }
        if terms.len() < MIN_TAPS {
            return None;
        }
        terms.reverse();
        // Equal literals can share an ID in a tap. Try several templates so
        // e.g. a tap on the diagonal does not tie both offsets to one value.
        for &term in terms.iter().take(8) {
            let Some(literals) = Self::match_terms(program, term, &terms) else {
                continue;
            };
            let ids = literals.iter().map(|(id, _)| *id).collect::<Vec<_>>();
            let (invariant, dependent) = reduction::invariants(program, term, &ids);
            if dependent.iter().any(|&id| {
                matches!(
                    nodes[id as usize].op,
                    EffectOp::SourceSample { .. } | EffectOp::TextureSample { .. }
                )
            }) {
                return Some(Self {
                    initial,
                    term,
                    literals,
                    invariant,
                    dependent,
                });
            }
        }
        None
    }

    fn match_terms(
        program: &EffectProgram,
        template: EffectNodeId,
        terms: &[EffectNodeId],
    ) -> Option<Vec<(EffectNodeId, Vec<u32>)>> {
        let mut changes = Vec::with_capacity(terms.len());
        let mut literals = BTreeSet::new();
        for &term in terms {
            let differences = reduction::differences(program, template, term)?;
            literals.extend(differences.keys().copied());
            if literals.len() > MAX_LITERALS {
                return None;
            }
            changes.push(differences);
        }
        if literals.is_empty() {
            return None;
        }
        Some(
            literals
                .into_iter()
                .map(|id| {
                    let EffectOp::Constant(EffectValue::Float(original)) =
                        program.nodes()[id as usize].op
                    else {
                        unreachable!()
                    };
                    let values = changes
                        .iter()
                        .map(|change| change.get(&id).copied().unwrap_or(original))
                        .collect();
                    (EffectNodeId(id), values)
                })
                .collect(),
        )
    }

    pub(super) fn dependencies(&self) -> impl Iterator<Item = EffectNodeId> + '_ {
        std::iter::once(self.initial).chain(self.invariant.iter().copied())
    }

    pub(super) fn emit(
        &self,
        emitter: &mut Emitter<'_>,
        scope: usize,
        id: EffectNodeId,
        tasks: &mut Vec<Task>,
    ) -> Result<(), GenerateError> {
        let name = emitter.next_name;
        emitter.next_name += 1;
        let initial = emitter.value(scope, self.initial);
        emitter.line(scope, &format!("var reel_n{name} = {initial};"))?;
        let count = self.literals[0].1.len();
        for (column, (_, values)) in self.literals.iter().enumerate() {
            let mut line = format!("let reel_taps{name}_{column} = array<f32, {count}>(");
            for &bits in values {
                write!(
                    line,
                    "{}, ",
                    constant(Language::Wgsl, EffectValue::Float(bits))
                )
                .unwrap();
            }
            line.push_str(");");
            emitter.line(scope, &line)?;
        }
        emitter.line(
            scope,
            &format!("for (var reel_i{name} = 0u; reel_i{name} < {count}u; reel_i{name} += 1u) {{"),
        )?;
        let body = emitter.child(scope);
        emitter.scopes[body].blocked.clone_from(&self.dependent);
        emitter.scopes[body].substituted = true;
        for (column, (id, _)) in self.literals.iter().enumerate() {
            let value = emitter.next_name;
            emitter.next_name += 1;
            emitter.line(
                body,
                &format!("let reel_n{value} = reel_taps{name}_{column}[reel_i{name}];"),
            )?;
            emitter.scopes[body].values.insert(*id, value);
        }
        tasks.push(Task::ScalarSum {
            scope,
            body,
            id,
            name,
            term: self.term,
        });
        tasks.push(Task::Scope(body, self.term));
        Ok(())
    }
}
