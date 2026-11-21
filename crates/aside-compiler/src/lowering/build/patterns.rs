//! Value patterns use the same resumable comparisons and branches as expressions.
use super::*;

const TEXT_MATCH_MIN_ARMS: usize = 8;
// Includes the two quote bytes. Decoding escapes does not grow the literal.
const TEXT_MATCH_MAX_SOURCE_BYTES: u32 = 66;

#[derive(Default)]
struct MatchProjection {
    bindings: Vec<(h::SymbolId, bc::Reg)>,
    scratch: Vec<bc::Reg>,
    active: usize,
}

fn projects_match_fields(hir: &h::Hir, node: h::NodeId) -> bool {
    let part = hir.node(node);
    match part.form {
        Form::Sequence => matches!(part.kind, H::TuplePattern | H::ObjectPattern),
        Form::Single { child } => projects_match_fields(hir, child),
        Form::TypePattern { pattern, .. } => projects_match_fields(hir, pattern),
        Form::EnumCase { payload, .. } => payload.is_some(),
        Form::Or { left, right } => {
            projects_match_fields(hir, left) || projects_match_fields(hir, right)
        }
        _ => false,
    }
}

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
                let ty = self.tuple_pattern_type(count)?;
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

    fn tuple_pattern_type(&mut self, count: usize) -> Result<bc::TypeId, Box<Error>> {
        let any = self.l.builtin(bc::BuiltinType::Any)?;
        let mut elements = Vec::new();
        self.l.b.reserve(&mut elements, count)?;
        elements.resize(count, any);
        let ty = bc::TypeId(self.l.package.types.len() as u32);
        self.l
            .b
            .push(&mut self.l.package.types, bc::TypeDef::Tuple(elements))?;
        Ok(ty)
    }

    fn record_pattern_type(&mut self, node: h::NodeId) -> Result<bc::TypeId, Box<Error>> {
        let any = self.l.builtin(bc::BuiltinType::Any)?;
        let mut fields = Vec::new();
        for field in self.l.m.hir().children(node) {
            let Form::PatternField { key, .. } = self.l.m.hir().node(field).form else {
                unreachable!("analyzed record pattern");
            };
            let name = self.l.name(self.l.m.hir().text(key.span))?;
            self.l
                .b
                .push(&mut fields, bc::RecordFieldType { name, ty: any, optional: false })?;
        }
        let ty = bc::TypeId(self.l.package.types.len() as u32);
        self.l
            .b
            .push(&mut self.l.package.types, bc::TypeDef::Record(fields))?;
        Ok(ty)
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
        let subject_type = self.l.m.facts()[source.0 as usize].ty;
        let typed_text = matches!(
            self.l.m.types()[subject_type.0 as usize],
            h::Type::String | h::Type::StringLiteral(_)
        );
        let Some(source) = self.value(source, depth)? else {
            return Ok(None);
        };
        let out = self.temp()?;
        let join = self.block()?;
        let mut falls = false;
        self.l.b.work(self.l.m.hir().elements(node).len() as u64)?;
        let use_text_checks = typed_text
            && self
                .l
                .m
                .hir()
                .children(node)
                .nth(TEXT_MATCH_MIN_ARMS)
                .is_some();
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
            self.bind_match_pattern(arm, pattern, source, next, depth, use_text_checks)?;
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

    pub(super) fn bind_match_pattern(
        &mut self,
        scope_node: h::NodeId,
        pattern: h::NodeId,
        source: bc::Reg,
        failed: BlockId,
        depth: usize,
        use_text_checks: bool,
    ) -> Result<(), Box<Error>> {
        let scope = self.l.m.facts()[scope_node.0 as usize]
            .scope
            .expect("pattern scope");
        let symbols = &self.l.m.scopes()[scope.0 as usize].symbols;
        self.l.b.work(symbols.len() as u64)?;
        let structured = projects_match_fields(self.l.m.hir(), pattern);
        let mut projection = MatchProjection::default();
        let mismatch = if structured {
            for &symbol in symbols {
                let slot = self.temp()?;
                self.l.b.push(&mut projection.bindings, (symbol, slot))?;
            }
            self.block()?
        } else {
            failed
        };
        self.value_pattern(
            pattern,
            source,
            mismatch,
            depth,
            use_text_checks,
            &mut projection,
        )?;
        self.enter_scope(scope_node)?;
        if structured {
            // Every successful alternative supplies the same symbols. Commit
            // once and release projected values on both pattern edges.
            for &(symbol, slot) in &projection.bindings {
                self.write(symbol, slot, true, pattern)?;
                self.release(slot)?;
            }
            for &slot in &projection.scratch {
                self.release(slot)?;
            }
            let matched = self.block()?;
            self.terminate(Terminator::Jump(matched), scope_node)?;
            self.start(mismatch)?;
            for slot in projection
                .bindings
                .iter()
                .map(|&(_, slot)| slot)
                .chain(projection.scratch.iter().copied())
            {
                self.op(bc::Opcode::Clear, slot.0, 0, 0, pattern)?;
            }
            self.terminate(Terminator::Jump(failed), scope_node)?;
            self.start(matched)?;
            self.l.b.release(projection.bindings);
            self.l.b.release(projection.scratch);
        } else {
            for &symbol in symbols {
                self.write(symbol, source, true, pattern)?;
            }
        }
        Ok(())
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
        use_text_checks: bool,
        projection: &mut MatchProjection,
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
            Form::Name { .. } | Form::PatternField { pattern: None, .. } => {
                if !projection.bindings.is_empty() {
                    let symbol = self.l.m.facts()[node.0 as usize]
                        .symbol
                        .expect("match binding");
                    let index = projection
                        .bindings
                        .binary_search_by_key(&symbol, |&(symbol, _)| symbol)
                        .expect("known match binding symbol");
                    self.op(
                        bc::Opcode::Move,
                        projection.bindings[index].1.0,
                        value.0,
                        0,
                        node,
                    )?;
                }
                Ok(())
            }
            Form::Wildcard => Ok(()),
            Form::Single { child } if self.l.m.hir().node(node).kind == H::GroupPattern => {
                self.value_pattern(child, value, failed, depth, use_text_checks, projection)
            }
            Form::LiteralPattern {
                value: literal,
                negative,
            } => {
                // Short text patterns can test the exact existing string
                // contract without materializing an object or comparison frame.
                if use_text_checks
                    && !negative
                    && let Some(ty) = self.short_string_pattern(literal)?
                {
                    let condition = self.temp()?;
                    self.op(bc::Opcode::Is, condition.0, value.0, ty.0, node)?;
                    return self.pattern_branch(node, condition, failed);
                }
                if let Form::Literal { token } = self.l.m.hir().node(literal).form
                    && token.kind == K::Number
                {
                    let constant = self.number(self.l.m.hir().text(token.span), negative)?;
                    let constant = self.constant_id(constant)?;
                    let condition = self.temp()?;
                    self.op(bc::Opcode::EqNumber, condition.0, value.0, constant.0, node)?;
                    return self.pattern_branch(node, condition, failed);
                }
                let literal = self.value(literal, depth)?.expect("literal value");
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
                self.value_pattern(pattern, value, failed, depth, use_text_checks, projection)
            }
            Form::EnumCase { payload, .. } => {
                let shape = self.l.ty(self.l.m.facts()[node.0 as usize].ty, 0)?;
                let condition = self.temp()?;
                self.op(bc::Opcode::Is, condition.0, value.0, shape.0, node)?;
                self.pattern_branch(node, condition, failed)?;
                if let Some(payload) = payload {
                    let part = self.match_part(node, value, bc::Const::Int(1), projection)?;
                    projection.active += 1;
                    self.match_fields(payload, part, failed, depth, projection, false)?;
                    projection.active -= 1;
                }
                Ok(())
            }
            Form::Or { left, right } => {
                let alternative = self.block()?;
                let accepted = self.block()?;
                self.value_pattern(left, value, alternative, depth, use_text_checks, projection)?;
                self.terminate(Terminator::Jump(accepted), node)?;
                self.start(alternative)?;
                self.value_pattern(right, value, failed, depth, use_text_checks, projection)?;
                self.terminate(Terminator::Jump(accepted), node)?;
                self.start(accepted)
            }
            Form::Sequence
                if matches!(
                    self.l.m.hir().node(node).kind,
                    H::TuplePattern | H::ObjectPattern
                ) =>
            {
                self.l.b.work(self.l.m.hir().elements(node).len() as u64)?;
                let tuple = self.l.m.hir().node(node).kind == H::TuplePattern;
                let open = !tuple
                    && self
                        .l
                        .m
                        .hir()
                        .children(node)
                        .last()
                        .is_some_and(|child| self.l.m.hir().node(child).kind == H::RestPattern);
                let shape = if tuple {
                    self.tuple_pattern_type(self.l.m.hir().children(node).count())?
                } else if open {
                    self.l.builtin(bc::BuiltinType::Table)?
                } else {
                    self.record_pattern_type(node)?
                };
                let condition = self.temp()?;
                self.op(bc::Opcode::Is, condition.0, value.0, shape.0, node)?;
                self.pattern_branch(node, condition, failed)?;
                self.match_fields(node, value, failed, depth, projection, open)
            }
            _ => Err(self.l.b.error(
                ErrorKind::Unsupported,
                "structural pattern lowering is not connected",
            )),
        }
    }

    fn match_fields(
        &mut self,
        node: h::NodeId,
        value: bc::Reg,
        failed: BlockId,
        depth: usize,
        projection: &mut MatchProjection,
        open: bool,
    ) -> Result<(), Box<Error>> {
        self.l.b.work(self.l.m.hir().elements(node).len() as u64)?;
        for (index, child) in self.l.m.hir().children(node).enumerate() {
            if self.l.m.hir().node(child).kind == H::RestPattern {
                break;
            }
            let (pattern, field) = match self.l.m.hir().node(child).form {
                Form::PatternField { key, pattern, .. } => (pattern.unwrap_or(child), Some(key)),
                _ => (child, None),
            };
            let ignore = matches!(self.l.m.hir().node(pattern).form, Form::Wildcard);
            if ignore && !open {
                continue;
            }
            let key = match field {
                Some(field) => bc::Const::Utf8(self.l.b.string(self.l.m.hir().text(field.span))?),
                None => bc::Const::Int(index as i64),
            };
            let part = if open {
                let found = self.constant(key, child)?;
                let part = if ignore {
                    found
                } else {
                    self.match_slot(projection)?
                };
                self.op(bc::Opcode::PatternField, found.0, value.0, part.0, child)?;
                self.pattern_branch(child, found, failed)?;
                if ignore {
                    continue;
                }
                part
            } else {
                self.match_part(child, value, key, projection)?
            };
            projection.active += 1;
            self.value_pattern(pattern, part, failed, depth, false, projection)?;
            projection.active -= 1;
        }
        Ok(())
    }

    fn match_part(
        &mut self,
        node: h::NodeId,
        value: bc::Reg,
        key: bc::Const,
        projection: &mut MatchProjection,
    ) -> Result<bc::Reg, Box<Error>> {
        let part = self.match_slot(projection)?;
        let key = self.constant(key, node)?;
        self.op(bc::Opcode::IndexGet, part.0, value.0, key.0, node)?;
        self.release(key)?;
        Ok(part)
    }

    fn match_slot(&mut self, projection: &mut MatchProjection) -> Result<bc::Reg, Box<Error>> {
        if projection.active == projection.scratch.len() {
            let slot = self.temp()?;
            self.l.b.push(&mut projection.scratch, slot)?;
        }
        Ok(projection.scratch[projection.active])
    }

    fn short_string_pattern(
        &mut self,
        literal: h::NodeId,
    ) -> Result<Option<bc::TypeId>, Box<Error>> {
        let source = self.l.m.hir().node(literal);
        if source.kind != H::String || u32::from(source.span.len()) > TEXT_MATCH_MAX_SOURCE_BYTES {
            return Ok(None);
        }
        let Some(bc::Const::Utf8(text)) = self.l.literal_constant(literal)? else {
            return Ok(None);
        };
        let ty = bc::TypeId(self.l.package.types.len() as u32);
        self.l
            .b
            .push(&mut self.l.package.types, bc::TypeDef::StringLiteral(text))?;
        Ok(Some(ty))
    }
}
