//! Bounded loop blocks keep the original counter updates and accumulation order.
use super::*;
use std::rc::Rc;
type Proof = Rc<reduction::Reduction>;

pub(super) enum Continuation {
    Tap {
        parent: usize,
        name: usize,
        proof: Proof,
        advance: bool,
    },
    Add {
        scope: usize,
        name: usize,
        sample: EffectNodeId,
        step: Option<i32>,
    },
    LoopEnd {
        parent: usize,
        id: EffectNodeId,
        name: usize,
        proof: Proof,
        width: usize,
    },
    Finish {
        parent: usize,
        id: EffectNodeId,
        name: usize,
        proof: Proof,
    },
}
impl Emitter<'_> {
    pub(super) fn begin_reduction(
        &mut self,
        scope: usize,
        id: EffectNodeId,
        reduction: reduction::Reduction,
        tasks: &mut Vec<Task>,
    ) -> Result<(), GenerateError> {
        let name = self.next_name;
        self.next_name += 1;
        self.line(scope, &self.language.mutable(EffectType::Vec4, name))?;
        let initial = reduction.initial.map(|id| self.value(scope, id));
        self.line(
            scope,
            &format!(
                "reel_n{name} = {};",
                construct(self.language, EffectType::Vec4, &initial)
            ),
        )?;
        if let Some(proof) = self
            .diagonal
            .filter(|proof| proof.sample == reduction.sample)
        {
            self.line(
                scope,
                &format!(
                    "float reel_source_fixed = reel_source_fixed_axis({});",
                    self.value(scope, proof.fixed)
                ),
            )?;
        }
        if self.language == Language::Cg {
            self.line(scope, "#pragma loop (unroll: never)")?;
        }
        let width = if id == self.program.output() {
            reduction
                .block_width(self.program)
                .min(self.max_block_width)
        } else {
            1
        };
        let main_end = loop_end(&reduction, width);
        let declaration = declaration(self.language, name, reduction.float_counter);
        let increment = if width == 1 {
            format!("reel_i{name} += {}", reduction.step)
        } else {
            String::new()
        };
        self.line(
            scope,
            &format!(
                "for ({declaration} = {}; reel_i{name} != {main_end}; {increment}) {{",
                reduction.first
            ),
        )?;
        let proof = Rc::new(reduction);
        tasks.push(Task::Reduction(Continuation::LoopEnd {
            parent: scope,
            id,
            name,
            proof: proof.clone(),
            width,
        }));
        for _ in 0..width {
            tasks.push(Task::Reduction(Continuation::Tap {
                parent: scope,
                name,
                proof: proof.clone(),
                advance: width != 1,
            }));
        }
        Ok(())
    }
    pub(super) fn emit_reduction(
        &mut self,
        task: Continuation,
        tasks: &mut Vec<Task>,
    ) -> Result<(), GenerateError> {
        match task {
            Continuation::Tap {
                parent,
                name,
                proof,
                advance,
            } => {
                let child = self.child(parent);
                self.scopes[child].blocked = proof.dependent.clone();
                self.scopes[child].substituted = true;
                let counter = self.next_name;
                self.next_name += 1;
                self.line(
                    child,
                    &self.language.declaration(
                        EffectType::Float,
                        counter,
                        &format!("{}(reel_i{name})", self.language.ty(EffectType::Float)),
                    ),
                )?;
                self.scopes[child].values.insert(proof.constant, counter);
                tasks.push(Task::Reduction(Continuation::Add {
                    scope: child,
                    name,
                    sample: proof.sample,
                    step: advance.then_some(proof.step),
                }));
                tasks.push(Task::Scope(child, proof.sample));
            }
            Continuation::Add {
                scope,
                name,
                sample,
                step,
            } => {
                self.line(
                    scope,
                    &format!(
                        "reel_n{name} = reel_n{name} + {};",
                        self.value(scope, sample)
                    ),
                )?;
                if let Some(step) = step {
                    self.line(scope, &format!("reel_i{name} += {step};"))?;
                }
            }
            Continuation::LoopEnd {
                parent,
                id,
                name,
                proof,
                width,
            } => {
                self.line(parent, "}")?;
                let remainder = proof.iterations() % width;
                if remainder == 0 {
                    self.finish_reduction(parent, id, name, &proof)?;
                } else {
                    self.line(parent, "{")?;
                    let tail = self.child(parent);
                    self.line(
                        tail,
                        &format!(
                            "{} = {};",
                            declaration(self.language, name, proof.float_counter),
                            loop_end(&proof, width)
                        ),
                    )?;
                    tasks.push(Task::Reduction(Continuation::Finish {
                        parent,
                        id,
                        name,
                        proof: proof.clone(),
                    }));
                    for _ in 0..remainder {
                        tasks.push(Task::Reduction(Continuation::Tap {
                            parent: tail,
                            name,
                            proof: proof.clone(),
                            advance: true,
                        }));
                    }
                }
            }
            Continuation::Finish {
                parent,
                id,
                name,
                proof,
            } => {
                self.line(parent, "}")?;
                self.finish_reduction(parent, id, name, &proof)?;
            }
        }
        Ok(())
    }
    fn finish_reduction(
        &mut self,
        parent: usize,
        id: EffectNodeId,
        name: usize,
        proof: &reduction::Reduction,
    ) -> Result<(), GenerateError> {
        for &(op, right) in &proof.scale {
            self.line(
                parent,
                &format!(
                    "reel_n{name} = reel_n{name} {} {}({});",
                    binary_name(op),
                    self.language.ty(EffectType::Vec4),
                    self.value(parent, right)
                ),
            )?;
        }
        self.scopes[parent].values.insert(id, name);
        Ok(())
    }
}
fn declaration(language: Language, name: usize, float: bool) -> String {
    match (language, float) {
        (Language::Cg, true) => format!("float reel_i{name}"),
        (Language::Cg, false) => format!("int reel_i{name}"),
        (Language::Wgsl, true) => format!("var reel_i{name}: f32"),
        (Language::Wgsl, false) => format!("var reel_i{name}: i32"),
    }
}
fn loop_end(proof: &reduction::Reduction, width: usize) -> i32 {
    (i64::from(proof.first) + (proof.iterations() / width * width) as i64 * i64::from(proof.step))
        as i32
}
