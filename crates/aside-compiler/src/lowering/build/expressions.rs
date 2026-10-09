use super::*;

impl FunctionBuilder<'_, '_> {
    pub(super) fn value(&mut self, n: h::NodeId, depth: usize) -> Result<Option<bc::Reg>, Error> {
        let value = self.expr(n, depth)?;
        if value.is_none() && self.current.is_some() {
            return Err(self.l.b.error(
                ErrorKind::Unsupported,
                "expression does not produce a value",
            ));
        }
        Ok(value)
    }
    pub(super) fn expr(&mut self, n: h::NodeId, depth: usize) -> Result<Option<bc::Reg>, Error> {
        self.l.b.span = self.span(n);
        self.l.b.work(1)?;
        if depth >= self.l.b.limits.max_depth.min(128) {
            return Err(self
                .l
                .b
                .error(ErrorKind::Limit, "source lowering depth exceeded"));
        }
        let kind = self.l.m.hir().node(n).kind;
        let form = self.l.m.hir().node(n).form;
        let depth = depth + 1;
        match (kind, form) {
            (
                H::Member | H::Index | H::Call,
                Form::Member { chain: true, .. }
                | Form::Index { chain: true, .. }
                | Form::Call { chain: true, .. },
            ) => self.optional_chain(n, depth - 1),
            (H::Module | H::Block, _) => {
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
                            ) || matches!(self.l.m.hir().node(last).kind, H::Block | H::If)
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
                    let value = self.expr(child, depth)?;
                    if tail == Some(child) {
                        result = value;
                    } else if let Some(value) = value {
                        self.release(value)?;
                    }
                }
                if self.current.is_some() && result.is_none() && kind == H::Block {
                    result = Some(self.constant(bc::Const::Null, n)?);
                }
                Ok(result)
            }
            (
                H::ExpressionStatement,
                Form::Statement {
                    expression,
                    terminated,
                },
            ) => {
                let value = self.expr(expression, depth)?;
                if terminated {
                    if let Some(value) = value {
                        self.release(value)?;
                    }
                    Ok(None)
                } else {
                    Ok(value)
                }
            }
            (H::Group, Form::Single { child })
            | (
                H::ModifiedDeclaration,
                Form::Modified {
                    declaration: child, ..
                },
            ) => self.expr(child, depth),
            (
                H::Let,
                Form::Binding {
                    pattern,
                    initializer,
                    ..
                },
            ) => {
                let symbol = self.pattern_symbol(pattern)?;
                if let Some(initializer) = initializer {
                    let Some(value) = self.value(initializer, depth)? else {
                        return Ok(None);
                    };
                    if let Some(symbol) = symbol {
                        self.write(symbol, value, true, n)?;
                    }
                    self.release(value)?;
                }
                Ok(None)
            }
            (H::Function, _) | (H::Import, _) => Ok(None),
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
            (H::Literal, Form::Literal { token }) => {
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
            (H::String, _) => {
                let text = self.string(n)?;
                Ok(Some(self.constant(bc::Const::Utf8(text), n)?))
            }
            (H::Tuple | H::Array, Form::Sequence) => self.sequence(n, depth),
            (H::Object, Form::Sequence) => self.table(n, depth),
            (H::Index | H::Member, _) => self.position_value(n, depth - 1),
            (H::Prefix, Form::Unary { operator, value }) => {
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
                    _ => {
                        return Err(self.l.b.error(
                            ErrorKind::Unsupported,
                            "await/yield require later task lowering",
                        ));
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
            (
                H::Binary,
                Form::Binary {
                    left,
                    operator,
                    right,
                },
            ) => {
                if operator.kind == K::Is {
                    let Some(left) = self.value(left, depth)? else {
                        return Ok(None);
                    };
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
                let Some(left) = self.value(left, depth)? else {
                    return Ok(None);
                };
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
            ) => self.condition(n, condition, body, otherwise, depth),
            (
                H::Call,
                Form::Call {
                    callee,
                    arguments,
                    children: None,
                    optional: false,
                    chain: false,
                },
            ) => {
                let native = self.l.native(callee)?;
                let callee_value = if native.is_none() {
                    let Some(callee) = self.value(callee, depth)? else {
                        return Ok(None);
                    };
                    Some(callee)
                } else {
                    None
                };
                let target = match (native, callee_value) {
                    (Some(native), _) => bc::CallTarget::Native(native),
                    (_, Some(callee)) => bc::CallTarget::Register(callee),
                    _ => unreachable!(),
                };
                let out = self.call_with_target(n, arguments, target, depth)?;
                if let Some(callee) = callee_value {
                    self.release(callee)?;
                }
                Ok(out)
            }
            (H::Return, Form::Transfer { value, .. }) => {
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
            (H::Throw, Form::Transfer { value, .. }) => {
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
            (H::Label, Form::Label { body, .. }) => self.expr(body, depth),
            (H::Break | H::Continue, _) => {
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
                let loop_depth = target.protected_depth;
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
            _ => Err(self.l.b.error(
                ErrorKind::Unsupported,
                format!("{kind:?} is outside L04 lowering support"),
            )),
        }
    }
    fn pattern_symbol(&mut self, n: h::NodeId) -> Result<Option<h::SymbolId>, Error> {
        self.l.b.span = self.span(n);
        self.l.b.work(1)?;
        match self.l.m.hir().node(n).form {
            Form::Name { .. } => Ok(self.l.m.facts()[n.0 as usize].symbol),
            Form::Wildcard => Ok(None),
            Form::Single { child } if self.l.m.hir().node(n).kind == H::GroupPattern => {
                self.pattern_symbol(child)
            }
            _ => Err(self.l.b.error(
                ErrorKind::Unsupported,
                "destructuring/pattern lowering belongs to later language work",
            )),
        }
    }
    fn short_circuit(
        &mut self,
        n: h::NodeId,
        left: h::NodeId,
        operator: K,
        right: h::NodeId,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Error> {
        let Some(left) = self.value(left, depth)? else {
            return Ok(None);
        };
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
    ) -> Result<Option<bc::Reg>, Error> {
        let Some(condition) = self.value(condition, depth)? else {
            return Ok(None);
        };
        let out = self.temp()?;
        let yes_block = self.block()?;
        let no_block = self.block()?;
        let join = self.block()?;
        self.terminate(
            Terminator::BranchBool {
                value: condition,
                yes: yes_block,
                no: no_block,
            },
            n,
        )?;
        self.release(condition)?;
        let mut falls = false;
        for (block, branch) in [(yes_block, Some(yes)), (no_block, no)] {
            self.start(block)?;
            let value = match branch {
                Some(branch) => self.expr(branch, depth)?,
                None => None,
            };
            if self.current.is_some() {
                let value = match value {
                    Some(value) => value,
                    None => self.constant(bc::Const::Null, n)?,
                };
                self.op(bc::Opcode::Move, out.0, value.0, 0, n)?;
                self.release(value)?;
                self.terminate(Terminator::Jump(join), n)?;
                falls = true;
            }
        }
        if falls {
            self.start(join)?;
            Ok(Some(out))
        } else {
            self.release(out)?;
            Ok(None)
        }
    }
    fn loop_node(
        &mut self,
        n: h::NodeId,
        condition: Option<h::NodeId>,
        body: h::NodeId,
        depth: usize,
    ) -> Result<(), Error> {
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
                has_break: false,
            },
        )?;
        if let Some(condition) = condition {
            let Some(value) = self.value(condition, depth)? else {
                let target = self.loops.pop().expect("installed loop target");
                if target.has_break {
                    self.start(end)?;
                }
                return Ok(());
            };
            self.terminate(
                Terminator::BranchBool {
                    value,
                    yes: body_block,
                    no: end,
                },
                n,
            )?;
            self.release(value)?;
        } else {
            self.terminate(Terminator::Jump(body_block), n)?;
        }
        self.start(body_block)?;
        if let Some(value) = self.expr(body, depth)? {
            self.release(value)?;
        }
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
    ) -> Result<(), Error> {
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
        if let Some(value) = self.expr(body, depth)? {
            self.release(value)?;
        }
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
            if let Some(value) = self.expr(body, depth)? {
                self.release(value)?;
            }
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
            if let Some(value) = self.expr(child, depth)? {
                self.release(value)?;
            }
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
    fn number(&mut self, text: &str, negative: bool) -> Result<bc::Const, Error> {
        let mut bytes = Vec::new();
        self.l.b.work(text.len() as u64)?;
        self.l.b.reserve(&mut bytes, text.len())?;
        bytes.extend(text.bytes().filter(|&c| c != b'_'));
        let text = std::str::from_utf8(&bytes).expect("numeric source UTF-8");
        let prefix = text.get(..2).unwrap_or("");
        let (radix, digits) = match prefix {
            "0x" | "0X" => (16, &text[2..]),
            "0o" | "0O" => (8, &text[2..]),
            "0b" | "0B" => (2, &text[2..]),
            _ => (10, text),
        };
        let value = if radix == 10 && text.bytes().any(|c| matches!(c, b'.' | b'e' | b'E')) {
            let float: f64 = text.parse().map_err(|_| {
                self.l
                    .b
                    .error(ErrorKind::InvalidIr, "analyzed float literal")
            })?;
            bc::Const::FloatBits(if negative { -float } else { float }.to_bits())
        } else {
            let value = i128::from_str_radix(digits, radix).map_err(|_| {
                self.l
                    .b
                    .error(ErrorKind::InvalidIr, "analyzed integer literal")
            })?;
            let value = if negative { -value } else { value };
            bc::Const::Int(i64::try_from(value).map_err(|_| {
                self.l
                    .b
                    .error(ErrorKind::InvalidIr, "integer literal exceeds int64")
            })?)
        };
        self.l.b.release(bytes);
        Ok(value)
    }
    fn string(&mut self, n: h::NodeId) -> Result<String, Error> {
        if self.l.m.hir().children(n).next().is_some() {
            return Err(self.l.b.error(
                ErrorKind::Unsupported,
                "interpolation requires standard string conversion linkage",
            ));
        }
        let mut bytes = Vec::new();
        self.l
            .b
            .reserve(&mut bytes, usize::from(self.span(n).len()))?;
        let raw = self
            .l
            .m
            .hir()
            .tokens(n)
            .next()
            .is_some_and(|t| self.l.m.hir().text(t.span).starts_with('r'));
        let format = self
            .l
            .m
            .hir()
            .tokens(n)
            .next()
            .is_some_and(|t| self.l.m.hir().text(t.span).starts_with('f'));
        for token in self.l.m.hir().tokens(n).filter(|t| t.kind == K::StringText) {
            let text = self.l.m.hir().text(token.span);
            self.l.b.work(text.len() as u64)?;
            if raw {
                bytes.extend_from_slice(text.as_bytes());
                continue;
            }
            let mut chars = text.chars();
            while let Some(ch) = chars.next() {
                let ch = if ch == '\\' {
                    match chars.next() {
                        Some('n') => '\n',
                        Some('r') => '\r',
                        Some('t') => '\t',
                        Some('0') => '\0',
                        Some('"') => '"',
                        Some('\\') => '\\',
                        Some('u') => {
                            if chars.next() != Some('{') {
                                return Err(self
                                    .l
                                    .b
                                    .error(ErrorKind::InvalidIr, "analyzed unicode escape"));
                            }
                            let mut value = 0u32;
                            loop {
                                match chars.next() {
                                    Some('}') => break,
                                    Some(c) => {
                                        value = value
                                            .checked_mul(16)
                                            .and_then(|v| {
                                                c.to_digit(16).and_then(|c| v.checked_add(c))
                                            })
                                            .ok_or_else(|| {
                                                self.l.b.error(
                                                    ErrorKind::InvalidIr,
                                                    "unicode escape overflow",
                                                )
                                            })?;
                                    }
                                    None => {
                                        return Err(self.l.b.error(
                                            ErrorKind::InvalidIr,
                                            "unterminated analyzed escape",
                                        ));
                                    }
                                }
                            }
                            char::from_u32(value).ok_or_else(|| {
                                self.l
                                    .b
                                    .error(ErrorKind::InvalidIr, "analyzed unicode scalar")
                            })?
                        }
                        _ => {
                            return Err(self
                                .l
                                .b
                                .error(ErrorKind::InvalidIr, "analyzed string escape"));
                        }
                    }
                } else if format && matches!(ch, '{' | '}') {
                    chars.next();
                    ch
                } else {
                    ch
                };
                bytes.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
            }
        }
        Ok(String::from_utf8(bytes).expect("decoded unicode"))
    }
}
