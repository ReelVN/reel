//! Value patterns use the same resumable comparisons and branches as expressions.
use super::*;

impl FunctionBuilder<'_, '_> {
    pub(super) fn match_value(
        &mut self,
        node: h::NodeId,
        source: h::NodeId,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let Some(source) = self.value(source, depth)? else {
            return Ok(None);
        };
        let out = self.temp()?;
        let join = self.block()?;
        let mut falls = false;
        self.l.b.work(self.l.m.hir().elements(node).len() as u64)?;
        for arm in self.l.m.hir().children(node).skip(1) {
            let Form::MatchArm {
                pattern,
                guard,
                body,
            } = self.l.m.hir().node(arm).form
            else {
                unreachable!("analyzed match arm");
            };
            let next = self.block()?;
            self.enter_scope(arm)?;
            self.value_pattern(pattern, source, next, depth)?;
            // Every supported value pattern binds the original value. OR alternatives
            // share these symbols; commit once, after the complete test succeeds.
            let scope = self.l.m.facts()[arm.0 as usize].scope.expect("match scope");
            let symbols = &self.l.m.scopes()[scope.0 as usize].symbols;
            self.l.b.work(symbols.len() as u64)?;
            for &symbol in symbols {
                self.write(symbol, source, true, pattern)?;
            }
            if let Some(guard) = guard {
                let no_wait = std::mem::replace(&mut self.no_wait, true);
                let value = self.value(guard, depth)?;
                self.no_wait = no_wait;
                if let Some(value) = value {
                    self.pattern_branch(guard, value, next)?;
                } else {
                    self.start(next)?;
                    continue;
                }
            }
            let value = self.expr(body, depth)?;
            if self.current.is_some() {
                let value = match value {
                    Some(value) => value,
                    None => self.constant(bc::Const::Null, body)?,
                };
                if let Some(collector) = self.collector
                    && self.collects(body)
                {
                    self.op(bc::Opcode::ChildrenPush, collector.0, value.0, 0, body)?;
                }
                self.op(bc::Opcode::Move, out.0, value.0, 0, body)?;
                self.release(value)?;
                self.terminate(Terminator::Jump(join), body)?;
                falls = true;
            }
            self.start(next)?;
        }
        self.terminate(Terminator::MatchFail, node)?;
        if falls {
            self.start(join)?;
            self.release(source)?;
            Ok(Some(out))
        } else {
            self.release(source)?;
            self.release(out)?;
            Ok(None)
        }
    }

    fn pattern_branch(
        &mut self,
        node: h::NodeId,
        condition: bc::Reg,
        failed: BlockId,
    ) -> Result<(), Box<Error>> {
        let accepted = self.block()?;
        self.terminate(
            Terminator::BranchBool {
                value: condition,
                yes: accepted,
                no: failed,
            },
            node,
        )?;
        self.release(condition)?;
        self.start(accepted)
    }

    fn value_pattern(
        &mut self,
        node: h::NodeId,
        value: bc::Reg,
        failed: BlockId,
        depth: usize,
    ) -> Result<(), Box<Error>> {
        self.l.b.span = self.span(node);
        self.l.b.work(1)?;
        if depth >= self.l.b.limits.max_depth {
            return Err(self
                .l
                .b
                .error(ErrorKind::Limit, "pattern lowering depth exceeded"));
        }
        let depth = depth + 1;
        match self.l.m.hir().node(node).form {
            Form::Name { .. } | Form::Wildcard => Ok(()),
            Form::Single { child } if self.l.m.hir().node(node).kind == H::GroupPattern => {
                self.value_pattern(child, value, failed, depth)
            }
            Form::LiteralPattern {
                value: literal,
                negative,
            } => {
                let literal = if negative {
                    let Form::Literal { token } = self.l.m.hir().node(literal).form else {
                        unreachable!("negative numeric pattern");
                    };
                    let constant = self.number(self.l.m.hir().text(token.span), true)?;
                    self.constant(constant, node)?
                } else {
                    self.value(literal, depth)?.expect("literal value")
                };
                let condition = self.temp()?;
                self.op(bc::Opcode::Eq, condition.0, value.0, literal.0, node)?;
                self.release(literal)?;
                self.pattern_branch(node, condition, failed)
            }
            Form::TypePattern {
                pattern,
                annotation,
            } => {
                let ty = self.l.ty(self.l.m.facts()[annotation.0 as usize].ty, 0)?;
                let condition = self.temp()?;
                self.op(bc::Opcode::Is, condition.0, value.0, ty.0, node)?;
                self.pattern_branch(node, condition, failed)?;
                self.value_pattern(pattern, value, failed, depth)
            }
            Form::Or { left, right } => {
                let alternative = self.block()?;
                let accepted = self.block()?;
                self.value_pattern(left, value, alternative, depth)?;
                self.terminate(Terminator::Jump(accepted), node)?;
                self.start(alternative)?;
                self.value_pattern(right, value, failed, depth)?;
                self.terminate(Terminator::Jump(accepted), node)?;
                self.start(accepted)
            }
            _ => Err(self.l.b.error(
                ErrorKind::Unsupported,
                "structural pattern lowering is not connected",
            )),
        }
    }
}
