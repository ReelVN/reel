use super::*;

impl FunctionBuilder<'_, '_> {
    pub(super) fn value(
        &mut self,
        n: h::NodeId,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let collector = self.collector.take();
        let value = self.expr(n, depth)?;
        self.collector = collector;
        if value.is_none() && self.current.is_some() {
            return Err(self.l.b.error(
                ErrorKind::Unsupported,
                "expression does not produce a value",
            ));
        }
        Ok(value)
    }
    pub(super) fn expr(
        &mut self,
        n: h::NodeId,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        self.expr_use(n, depth, Demand::Value)
    }
    pub(super) fn expr_use(
        &mut self,
        n: h::NodeId,
        depth: usize,
        demand: Demand,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let value = self.expression(n, depth, demand)?;
        if demand == Demand::Discard {
            if let Some(value) = value {
                self.release(value)?;
            }
            Ok(None)
        } else {
            Ok(value)
        }
    }
    pub(super) fn discard(&mut self, n: h::NodeId, depth: usize) -> Result<(), Box<Error>> {
        self.expr_use(n, depth, Demand::Discard).map(|_| ())
    }
    pub(super) fn expression_depth(
        &mut self,
        n: h::NodeId,
        depth: usize,
    ) -> Result<usize, Box<Error>> {
        self.l.b.span = self.span(n);
        self.l.b.work(1)?;
        if depth >= self.l.b.limits.max_depth {
            return Err(self
                .l
                .b
                .error(ErrorKind::Limit, "source lowering depth exceeded"));
        }
        Ok(depth + 1)
    }
    fn expression(
        &mut self,
        n: h::NodeId,
        depth: usize,
        demand: Demand,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let depth = self.expression_depth(n, depth)?;
        let kind = self.l.m.hir().node(n).kind;
        let form = self.l.m.hir().node(n).form;
        if kind == H::Call && matches!(
            self.l.m.facts()[n.0 as usize].intrinsic,
            Some(crate::semantic::Intrinsic::ResumeFlow
                | crate::semantic::Intrinsic::ResumePoint
                | crate::semantic::Intrinsic::ResumeReturn)
        ) {
            if demand != Demand::Discard {
                return Err(self.l.b.error(ErrorKind::Unsupported,
                    "resume declarations must be standalone statements"));
            }
            return Ok(None);
        }
        if matches!(kind, H::Name | H::Member)
            && let Some(value) = self.l.external_constant(n)?
        {
            return self.constant(value, n).map(Some);
        }
        if matches!(kind, H::Name | H::Member)
            && let Some(native) = self.l.native(n)?
        {
            return self.native_reference(native, n).map(Some);
        }
        if matches!(kind, H::Name | H::Member)
            && let Some(global) = self.l.global(n)?
        {
            return self.global_reference(global, n).map(Some);
        }
        match (kind, form) {
            (
                H::Member | H::Index | H::Call,
                Form::Member { chain: true, .. }
                | Form::Index { chain: true, .. }
                | Form::Call { chain: true, .. },
            ) => self.optional_chain(n, depth - 1),
            (H::Module | H::Block, _) => self.block_value(n, kind, form, depth, demand),
            (
                H::ExpressionStatement,
                Form::Statement {
                    expression,
                    terminated,
                },
            ) => self.statement_value(expression, terminated, depth, demand),
            (H::Group, Form::Single { child })
            | (
                H::ModifiedDeclaration,
                Form::Modified {
                    declaration: child, ..
                },
            ) => self.expr_use(child, depth, demand),
            (
                H::Let,
                Form::Binding {
                    pattern,
                    initializer,
                    ..
                },
            ) => self.binding_value(n, pattern, initializer, depth),
            (H::Function, _) | (H::Import, _) => Ok(None),
            (H::Class, Form::Class { base, .. }) => self.class_value(n, base, depth),
            (H::Name, _) => {
                let symbol = self.l.m.facts()[n.0 as usize].symbol.ok_or_else(|| {
                    self.l
                        .b
                        .error(ErrorKind::MissingLink, "name lacks analyzed binding")
                })?;
                Ok(Some(self.read(symbol, n)?))
            }
            (H::Closure, _) => {
                let function = self.l.m.facts()[n.0 as usize].function.ok_or_else(|| {
                    self.l
                        .b
                        .error(ErrorKind::InvalidIr, "closure lacks analyzed identity")
                })?;
                Ok(Some(self.closure(function, n)?))
            }
            (H::Literal, Form::Literal { token }) => self.literal_value(n, token),
            (H::String, _) => self.string_value(n, depth),
            (H::Tuple | H::Array, Form::Sequence) => self.sequence(n, depth),
            (H::Object, Form::Sequence) => self.table(n, depth),
            (H::Index | H::Member, _) => self.flat_chain(n, depth),
            (H::Prefix, Form::Unary { operator, value }) => {
                self.prefix_value(n, operator, value, depth)
            }
            (
                H::Binary,
                Form::Binary {
                    left,
                    operator,
                    right,
                },
            ) if demand == Demand::Discard
                && matches!(operator.kind, K::AndAnd | K::OrOr | K::QuestionQuestion) =>
            {
                if operator.kind == K::QuestionQuestion {
                    self.discard_coalesce(n, left, right, depth)?;
                } else {
                    self.discard_logical(n, depth)?;
                }
                Ok(None)
            }
            (H::Binary, _) => self.flat_chain(n, depth),
            (
                H::Range,
                Form::Binary {
                    left,
                    operator,
                    right,
                },
            ) => {
                let Some(left) = self.value(left, depth)? else {
                    return Ok(None);
                };
                let Some(right) = self.value(right, depth)? else {
                    self.release(left)?;
                    return Ok(None);
                };
                let out = self.temp()?;
                self.op(
                    if operator.kind == K::RangeInclusive {
                        bc::Opcode::MakeRangeInclusive
                    } else {
                        bc::Opcode::MakeRange
                    },
                    out.0,
                    left.0,
                    right.0,
                    n,
                )?;
                self.release(left)?;
                self.release(right)?;
                Ok(Some(out))
            }
            (H::Assignment, _) => {
                self.assignment(n, depth)?;
                Ok(None)
            }
            (
                H::If,
                Form::Condition {
                    pattern: None,
                    condition,
                    body,
                    otherwise,
                },
            ) => self.condition(n, condition, body, otherwise, depth, demand),
            (H::Match, Form::Match { value }) => self.match_value(n, value, depth),
            (H::Call, Form::Call { chain: false, .. }) => self.flat_chain(n, depth),
            (H::Return, Form::Transfer { value, .. }) => self.return_value(n, value, depth),
            (H::Yield, Form::Transfer { value, .. }) => {
                let input = if let Some(value) = value {
                    let Some(value) = self.value(value, depth)? else {
                        return Ok(None);
                    };
                    value
                } else {
                    self.constant(bc::Const::Null, n)?
                };
                let result = self.temp()?;
                self.op(bc::Opcode::Yield, result.0, input.0, 0, n)?;
                self.release(input)?;
                Ok(Some(result))
            }
            (H::Throw, Form::Transfer { value, .. }) => self.throw_value(n, value, depth),
            (
                H::Try,
                Form::Try {
                    body,
                    catch,
                    finally,
                },
            ) => {
                self.try_node(n, body, catch, finally, depth)?;
                Ok(None)
            }
            (
                H::While,
                Form::Condition {
                    pattern: None,
                    condition,
                    body,
                    otherwise: None,
                },
            ) => {
                self.loop_node(n, Some(condition), body, depth)?;
                Ok(None)
            }
            (H::Loop, Form::Single { child }) => {
                self.loop_node(n, None, child, depth)?;
                Ok(None)
            }
            (
                H::For,
                Form::For {
                    pattern,
                    source,
                    body,
                    asynchronous: false,
                },
            ) => {
                self.for_node(n, pattern, source, body, depth)?;
                Ok(None)
            }
            (H::Label, Form::Label { body, .. }) => self.expr_use(body, depth, demand),
            (H::Break | H::Continue, _) => self.loop_transfer(n, kind),
            _ => Err(self.l.b.error(
                ErrorKind::Unsupported,
                format!("{kind:?} is outside L04 lowering support"),
            )),
        }
    }
    fn block_value(
        &mut self,
        n: h::NodeId,
        kind: H,
        form: Form,
        depth: usize,
        demand: Demand,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        self.enter_scope(n)?;
        let tail = match form {
            Form::Block { tail } => tail,
            _ if kind == H::Module
                && self.l.m.entry_context() != crate::semantic::AnalysisContext::Module =>
            {
                self.l.b.work(self.l.m.hir().elements(n).len() as u64)?;
                self.l.m.hir().children(n).last().filter(|&last| {
                    matches!(
                        self.l.m.hir().node(last).form,
                        Form::Statement {
                            terminated: false,
                            ..
                        }
                    ) || matches!(self.l.m.hir().node(last).kind, H::Block | H::If | H::Match)
                })
            }
            _ => None,
        };
        let mut result = None;
        self.l.b.work(self.l.m.hir().elements(n).len() as u64)?;
        for child in self.l.m.hir().children(n) {
            if self.current.is_none() {
                break;
            }
            if let Some(old) = result.take() {
                self.release(old)?;
            }
            let value = self.expr_use(
                child,
                depth,
                if tail == Some(child) {
                    demand
                } else {
                    Demand::Discard
                },
            )?;
            if tail == Some(child) {
                result = value;
            } else if let Some(value) = value {
                self.release(value)?;
            }
        }
        if demand == Demand::Value && self.current.is_some() && result.is_none() && kind == H::Block
        {
            result = Some(self.constant(bc::Const::Null, n)?);
        }
        Ok(result)
    }
    fn statement_value(
        &mut self,
        expression: h::NodeId,
        terminated: bool,
        depth: usize,
        demand: Demand,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let collects = self.collector.is_some() && self.collects(expression);
        let value = self.expr_use(
            expression,
            depth,
            if collects || (!terminated && demand == Demand::Value) {
                Demand::Value
            } else {
                Demand::Discard
            },
        )?;
        if let (Some(collector), Some(value)) = (self.collector, value)
            && collects
        {
            self.op(
                bc::Opcode::ChildrenPush,
                collector.0,
                value.0,
                0,
                expression,
            )?;
        }
        if terminated || demand == Demand::Discard {
            if let Some(value) = value {
                self.release(value)?;
            }
            Ok(None)
        } else {
            Ok(value)
        }
    }
    fn binding_value(
        &mut self,
        _n: h::NodeId,
        pattern: h::NodeId,
        initializer: Option<h::NodeId>,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        if let Some(initializer) = initializer {
            let value = self.value(initializer, depth)?;
            let Some(value) = value else {
                return Ok(None);
            };
            self.bind_value(pattern, value, depth, false)?;
            self.release(value)?;
        }
        Ok(None)
    }
    fn literal_value(
        &mut self,
        n: h::NodeId,
        token: h::HirToken,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let constant = match token.kind {
            K::Null => bc::Const::Null,
            K::True => bc::Const::Bool(true),
            K::False => bc::Const::Bool(false),
            K::Number => self.number(self.l.m.hir().text(token.span), false)?,
            _ => {
                return Err(self
                    .l
                    .b
                    .error(ErrorKind::Unsupported, "unsupported scalar literal"));
            }
        };
        Ok(Some(self.constant(constant, n)?))
    }
    fn prefix_value(
        &mut self,
        n: h::NodeId,
        operator: h::HirToken,
        value: h::NodeId,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        // Treat the negative int64 minimum as a single source constant.
        if operator.kind == K::Minus
            && let Form::Literal { token } = self.l.m.hir().node(value).form
            && token.kind == K::Number
        {
            let constant = self.number(self.l.m.hir().text(token.span), true)?;
            return Ok(Some(self.constant(constant, n)?));
        }
        let opcode = match operator.kind {
            K::Minus => bc::Opcode::Neg,
            K::Plus => bc::Opcode::Pos,
            K::Bang => bc::Opcode::Not,
            K::Tilde => bc::Opcode::BitNot,
            K::Await => bc::Opcode::Await,
            _ => {
                return Err(self
                    .l
                    .b
                    .error(ErrorKind::Unsupported, "yield requires generator lowering"));
            }
        };
        let Some(input) = self.value(value, depth)? else {
            return Ok(None);
        };
        let out = self.temp()?;
        self.op(opcode, out.0, input.0, 0, n)?;
        self.release(input)?;
        Ok(Some(out))
    }
    pub(super) fn binary_value(
        &mut self,
        n: h::NodeId,
        left: bc::Reg,
        operator: h::HirToken,
        right: h::NodeId,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        if operator.kind == K::Is {
            let ty = self.l.ty(self.l.m.facts()[right.0 as usize].ty, 0)?;
            let out = self.temp()?;
            self.op(bc::Opcode::Is, out.0, left.0, ty.0, n)?;
            self.release(left)?;
            return Ok(Some(out));
        }
        if matches!(operator.kind, K::AndAnd | K::OrOr | K::QuestionQuestion) {
            return self.short_circuit(n, left, operator.kind, right, depth);
        }
        let opcode = binary(operator.kind).ok_or_else(|| {
            self.l.b.error(
                ErrorKind::Unsupported,
                "binary operator awaits later lowering",
            )
        })?;
        let Some(right) = self.value(right, depth)? else {
            self.release(left)?;
            return Ok(None);
        };
        let out = self.temp()?;
        self.op(opcode, out.0, left.0, right.0, n)?;
        self.release(left)?;
        self.release(right)?;
        Ok(Some(out))
    }
    fn return_value(
        &mut self,
        n: h::NodeId,
        value: Option<h::NodeId>,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        if matches!(self.ir.origin, Origin::Default { .. }) {
            return Err(self.l.b.error(
                ErrorKind::InvalidIr,
                "explicit return crossed the analyzed parameter-default boundary",
            ));
        }
        let value = match value {
            Some(value) => {
                let Some(value) = self.value(value, depth)? else {
                    return Ok(None);
                };
                value
            }
            None => self.constant(bc::Const::Null, n)?,
        };
        self.terminate(Terminator::Return(value), n)?;
        self.release(value)?;
        Ok(None)
    }
    fn throw_value(
        &mut self,
        n: h::NodeId,
        value: Option<h::NodeId>,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        if let Some(value) = value {
            let Some(value) = self.value(value, depth)? else {
                return Ok(None);
            };
            self.terminate(Terminator::Throw(value), n)?;
            self.release(value)?;
        } else {
            self.terminate(Terminator::Rethrow, n)?;
        }
        Ok(None)
    }
    fn loop_transfer(&mut self, n: h::NodeId, kind: H) -> Result<Option<bc::Reg>, Box<Error>> {
        let target = self.l.m.facts()[n.0 as usize]
            .control
            .as_ref()
            .and_then(|c| c.target)
            .ok_or_else(|| {
                self.l
                    .b
                    .error(ErrorKind::InvalidIr, "loop transfer lacks analyzed target")
            })?;
        self.l.b.work(self.loops.len() as u64)?;
        let target = self
            .loops
            .iter_mut()
            .rev()
            .find(|entry| entry.node == target)
            .ok_or_else(|| {
                self.l
                    .b
                    .error(ErrorKind::InvalidIr, "analyzed loop target not active")
            })?;
        let loop_depth = if kind == H::Break {
            target.protected_depth
        } else {
            target.next_protected_depth
        };
        let target = if kind == H::Break {
            target.has_break = true;
            target.end
        } else {
            target.next
        };
        self.terminate(
            if self.protected_depth > loop_depth {
                Terminator::Leave(target)
            } else {
                Terminator::Jump(target)
            },
            n,
        )?;
        Ok(None)
    }
    fn short_circuit(
        &mut self,
        n: h::NodeId,
        left: bc::Reg,
        operator: K,
        right: h::NodeId,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let out = self.temp()?;
        self.op(bc::Opcode::Move, out.0, left.0, 0, n)?;
        let rhs = self.block()?;
        let join = self.block()?;
        let term = match operator {
            K::AndAnd => Terminator::BranchBool {
                value: left,
                yes: rhs,
                no: join,
            },
            K::OrOr => Terminator::BranchBool {
                value: left,
                yes: join,
                no: rhs,
            },
            _ => Terminator::BranchNull {
                value: left,
                null: rhs,
                value_block: join,
            },
        };
        self.terminate(term, n)?;
        self.release(left)?;
        self.start(rhs)?;
        let Some(right) = self.value(right, depth)? else {
            // The short-circuit edge still reaches join with the saved left value.
            self.start(join)?;
            return Ok(Some(out));
        };
        self.op(bc::Opcode::Move, out.0, right.0, 0, n)?;
        self.terminate(
            if operator == K::QuestionQuestion {
                Terminator::Jump(join)
            } else {
                Terminator::BranchBool {
                    value: right,
                    yes: join,
                    no: join,
                }
            },
            n,
        )?;
        self.release(right)?;
        self.start(join)?;
        Ok(Some(out))
    }
    fn condition(
        &mut self,
        n: h::NodeId,
        condition: h::NodeId,
        yes: h::NodeId,
        no: Option<h::NodeId>,
        depth: usize,
        demand: Demand,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        if let Some(selected) = self.l.constant_condition(condition)? {
            return match if selected { Some(yes) } else { no } {
                Some(branch) => self.expr_use(branch, depth, demand),
                None if demand == Demand::Value => self.constant(bc::Const::Null, n).map(Some),
                None => Ok(None),
            };
        }
        let yes_block = self.block()?;
        let no_block = self.block()?;
        let join = self.block()?;
        if !self.branch_to(condition, n, yes_block, no_block, depth)? {
            return Ok(None);
        }
        let out = if demand == Demand::Value {
            Some(self.temp()?)
        } else {
            None
        };
        let mut falls = false;
        for (block, branch) in [(yes_block, Some(yes)), (no_block, no)] {
            self.start(block)?;
            let value = match branch {
                Some(branch) => self.expr_use(branch, depth, demand)?,
                None => None,
            };
            if self.current.is_some() {
                if let Some(out) = out {
                    let value = match value {
                        Some(value) => value,
                        None => self.constant(bc::Const::Null, n)?,
                    };
                    self.op(bc::Opcode::Move, out.0, value.0, 0, n)?;
                    self.release(value)?;
                }
                self.terminate(Terminator::Jump(join), n)?;
                falls = true;
            }
        }
        if falls {
            self.start(join)?;
            Ok(out)
        } else {
            if let Some(out) = out {
                self.release(out)?;
            }
            Ok(None)
        }
    }
    fn loop_node(
        &mut self,
        n: h::NodeId,
        condition: Option<h::NodeId>,
        body: h::NodeId,
        depth: usize,
    ) -> Result<(), Box<Error>> {
        let head = self.block()?;
        let body_block = self.block()?;
        let end = self.block()?;
        self.terminate(Terminator::Jump(head), n)?;
        self.start(head)?;
        self.l.b.push(
            &mut self.loops,
            LoopTarget {
                node: n,
                next: head,
                end,
                protected_depth: self.protected_depth,
                next_protected_depth: self.protected_depth,
                has_break: false,
            },
        )?;
        if let Some(condition) = condition {
            if !self.branch_to(condition, n, body_block, end, depth)? {
                let target = self.loops.pop().expect("installed loop target");
                if target.has_break {
                    self.start(end)?;
                }
                return Ok(());
            }
        } else {
            self.terminate(Terminator::Jump(body_block), n)?;
        }
        self.start(body_block)?;
        self.discard(body, depth)?;
        if self.current.is_some() {
            self.terminate(Terminator::Jump(head), n)?;
        }
        let target = self.loops.pop().expect("installed loop target");
        if condition.is_some() || target.has_break {
            self.start(end)?;
        }
        Ok(())
    }
    fn try_node(
        &mut self,
        n: h::NodeId,
        body: h::NodeId,
        catch: Option<h::NodeId>,
        finally: Option<h::NodeId>,
        depth: usize,
    ) -> Result<(), Box<Error>> {
        let region = bc::RegionId(self.ir.regions.len() as u32);
        self.l.b.push(
            &mut self.ir.regions,
            Region {
                protected: 0..0,
                catch: None,
                finally: None,
            },
        )?;
        let body_block = self.block()?;
        let after = self.block()?;
        self.terminate(Terminator::Jump(body_block), n)?;
        let begin = self.ir.layout.len() as u32;
        self.start(body_block)?;
        self.protected_depth += 1;
        let mut falls = false;
        self.discard(body, depth)?;
        if self.current.is_some() {
            self.terminate(Terminator::Leave(after), n)?;
            falls = true;
        }
        self.protected_depth -= 1;
        self.ir.regions[region.index()].protected = begin..self.ir.layout.len() as u32;
        if let Some(catch) = catch {
            let Form::Catch { body, .. } = self.l.m.hir().node(catch).form else {
                return Err(self.l.b.error(ErrorKind::InvalidIr, "catch form"));
            };
            let sym = self.l.m.facts()[catch.0 as usize].symbol.ok_or_else(|| {
                self.l
                    .b
                    .error(ErrorKind::InvalidIr, "catch binding identity")
            })?;
            let exception = self.temp()?;
            let block = self.block()?;
            let begin = self.ir.layout.len() as u32;
            self.start(block)?;
            if let Some(binding) = self.local(sym)? {
                if matches!(
                    self.def.bindings[binding.index()].storage,
                    bc::BindingStorage::Cell(_)
                ) {
                    self.op(bc::Opcode::CellNew, binding.0, 0, 0, catch)?;
                } else {
                    self.op(
                        bc::Opcode::Clear,
                        self.def.bindings[binding.index()].storage.reg().0,
                        0,
                        0,
                        catch,
                    )?;
                }
            }
            self.write(sym, exception, true, catch)?;
            self.release(exception)?;
            self.protected_depth += 1;
            self.discard(body, depth)?;
            if self.current.is_some() {
                self.terminate(Terminator::Leave(after), catch)?;
                falls = true;
            }
            self.protected_depth -= 1;
            self.ir.regions[region.index()].catch =
                Some((begin..self.ir.layout.len() as u32, exception));
        }
        if let Some(finally) = finally {
            let Form::Single { child } = self.l.m.hir().node(finally).form else {
                return Err(self.l.b.error(ErrorKind::InvalidIr, "finally form"));
            };
            let block = self.block()?;
            let begin = self.ir.layout.len() as u32;
            self.start(block)?;
            self.discard(child, depth)?;
            if self.current.is_none() {
                let end = self.block()?;
                self.start(end)?;
            }
            self.terminate(Terminator::EndFinally(region), finally)?;
            self.ir.regions[region.index()].finally = Some(begin..self.ir.layout.len() as u32);
        }
        if falls {
            self.start(after)?;
        }
        Ok(())
    }
    pub(super) fn number(&mut self, text: &str, negative: bool) -> Result<bc::Const, Box<Error>> {
        self.l.number(text, negative)
    }
    fn string_value(&mut self, n: h::NodeId, depth: usize) -> Result<Option<bc::Reg>, Box<Error>> {
        use crate::strings::{StringLiteral, StringPart};
        let literal = StringLiteral::new(self.l.m.hir(), n).expect("analyzed string");
        let interpolated = self.l.m.hir().children(n).next().is_some();
        if !interpolated {
            let value = self.l.literal_constant(n)?.expect("static string");
            return Ok(Some(self.constant(value, n)?));
        }
        let out = self.temp()?;
        self.op(bc::Opcode::TextNew, out.0, 0, 0, n)?;
        for part in literal.parts() {
            let value = match part {
                StringPart::Text(text) => {
                    let text = self.string_piece(text)?;
                    Some(self.constant(bc::Const::Utf8(text), n)?)
                }
                StringPart::Expression { node, .. } => self.value(node, depth)?,
            };
            let Some(value) = value else {
                self.release(out)?;
                return Ok(None);
            };
            self.op(bc::Opcode::TextPush, out.0, value.0, 0, n)?;
            self.release(value)?;
        }
        self.op(bc::Opcode::TextFinish, out.0, out.0, 0, n)?;
        Ok(Some(out))
    }
    fn string_piece(&mut self, text: crate::strings::StringText<'_>) -> Result<String, Box<Error>> {
        self.l.string_piece(text)
    }
}
