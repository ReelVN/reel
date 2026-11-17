//! Value patterns use the same resumable comparisons and branches as expressions.
use super::*;

impl FunctionBuilder<'_, '_> {
    pub(super) fn bind_value(
        &mut self,
        pattern: h::NodeId,
        value: bc::Reg,
        depth: usize,
        fresh: bool,
    ) -> Result<(), Box<Error>> {
        if let Form::Name { .. } = self.l.m.hir().node(pattern).form {
            let symbol = self.l.m.facts()[pattern.0 as usize]
                .symbol
                .expect("analyzed binding");
            self.commit_binding(pattern, symbol, value, fresh)?;
            return Ok(());
        }
        if matches!(self.l.m.hir().node(pattern).form, Form::Wildcard) {
            return Ok(());
        }
        let mut bindings = Vec::new();
        self.prepare_binding(pattern, value, depth, &mut bindings)?;
        for &(node, symbol, value) in &bindings {
            self.commit_binding(node, symbol, value, fresh)?;
            self.release(value)?;
        }
        self.l.b.release(bindings);
        Ok(())
    }

    fn commit_binding(
        &mut self,
        node: h::NodeId,
        symbol: h::SymbolId,
        value: bc::Reg,
        fresh: bool,
    ) -> Result<(), Box<Error>> {
        if fresh {
            let binding = self.local(symbol)?.ok_or_else(|| {
                self.l.b.error(
                    ErrorKind::InvalidIr,
                    "for binding must belong to the current frame",
                )
            })?;
            match self.def.bindings[binding.index()].storage {
                bc::BindingStorage::Cell(_) => {
                    self.op(bc::Opcode::CellNew, binding.0, 0, 0, node)?
                }
                bc::BindingStorage::Value(reg) => self.op(bc::Opcode::Clear, reg.0, 0, 0, node)?,
            }
        }
        self.write(symbol, value, true, node)
    }

    fn prepare_binding(
        &mut self,
        node: h::NodeId,
        value: bc::Reg,
        depth: usize,
        bindings: &mut Vec<(h::NodeId, h::SymbolId, bc::Reg)>,
    ) -> Result<(), Box<Error>> {
        self.l.b.span = self.span(node);
        self.l.b.work(1)?;
        if depth >= self.l.b.limits.max_depth {
            return Err(self
                .l
                .b
                .error(ErrorKind::Limit, "pattern lowering depth exceeded"));
        }
        match self.l.m.hir().node(node).form {
            Form::Name { .. } | Form::PatternField { pattern: None, .. } => {
                let symbol = self.l.m.facts()[node.0 as usize]
                    .symbol
                    .expect("analyzed binding");
                let prepared = self.temp()?;
                self.op(bc::Opcode::Move, prepared.0, value.0, 0, node)?;
                self.l.b.push(bindings, (node, symbol, prepared))?;
            }
            Form::Wildcard => {}
            Form::Single { child } if self.l.m.hir().node(node).kind == H::GroupPattern => {
                self.prepare_binding(child, value, depth + 1, bindings)?;
            }
            Form::TypePattern {
                pattern,
                annotation,
            } => {
                let ty = self.l.ty(self.l.m.facts()[annotation.0 as usize].ty, 0)?;
                self.require_pattern_type(node, value, ty)?;
                self.prepare_binding(pattern, value, depth + 1, bindings)?;
            }
            Form::Sequence if self.l.m.hir().node(node).kind == H::TuplePattern => {
                self.l.b.work(self.l.m.hir().elements(node).len() as u64)?;
                let count = self.l.m.hir().children(node).count();
                let any = self.l.builtin(bc::BuiltinType::Any)?;
                let mut elements = Vec::new();
                self.l.b.reserve(&mut elements, count)?;
                elements.resize(count, any);
                let ty = bc::TypeId(self.l.package.types.len() as u32);
                self.l
                    .b
                    .push(&mut self.l.package.types, bc::TypeDef::Tuple(elements))?;
                self.require_pattern_type(node, value, ty)?;
                for (index, child) in self.l.m.hir().children(node).enumerate() {
                    let index = self.constant(bc::Const::Int(index as i64), child)?;
                    let item = self.temp()?;
                    self.op(bc::Opcode::IndexGet, item.0, value.0, index.0, child)?;
                    self.release(index)?;
                    self.prepare_binding(child, item, depth + 1, bindings)?;
                    self.release(item)?;
                }
            }
            Form::Sequence if self.l.m.hir().node(node).kind == H::ObjectPattern => {
                self.l.b.work(self.l.m.hir().elements(node).len() as u64)?;
                for field in self.l.m.hir().children(node) {
                    let Form::PatternField {
                        key,
                        pattern,
                        default: None,
                    } = self.l.m.hir().node(field).form
                    else {
                        self.l.b.span = self.span(field);
                        return Err(self.l.b.error(
                            ErrorKind::Unsupported,
                            "object binding defaults are not supported",
                        ));
                    };
                    let name = self.l.b.string(self.l.m.hir().text(key.span))?;
                    let index = self.constant(bc::Const::Utf8(name), field)?;
                    let item = self.temp()?;
                    self.op(bc::Opcode::IndexGet, item.0, value.0, index.0, field)?;
                    self.release(index)?;
                    self.prepare_binding(pattern.unwrap_or(field), item, depth + 1, bindings)?;
                    self.release(item)?;
                }
            }
            _ => {
                return Err(self.l.b.error(
                    ErrorKind::Unsupported,
                    "binding pattern requires names, wildcards, fixed tuples or named fields",
                ));
            }
        }
        Ok(())
    }

    fn require_pattern_type(
        &mut self,
        node: h::NodeId,
        value: bc::Reg,
        ty: bc::TypeId,
    ) -> Result<(), Box<Error>> {
        let condition = self.temp()?;
        self.op(bc::Opcode::Is, condition.0, value.0, ty.0, node)?;
        self.op(bc::Opcode::PatternRequire, condition.0, 0, 0, node)?;
        self.release(condition)
    }

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
