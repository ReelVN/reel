//! One null exit per continuous postfix chain; Group remains a value boundary.
use super::*;

impl FunctionBuilder<'_, '_> {
    /// Traverse the completed left side without growing the Rust stack. New
    /// operands and argument bodies keep their ordinary lexical depth.
    pub(super) fn flat_chain(
        &mut self,
        root: h::NodeId,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let mut segments = Vec::new();
        let mut base = root;
        let value = loop {
            self.l.b.span = self.span(base);
            self.l.b.work(1)?;
            let receiver = match self.l.m.hir().node(base).form {
                Form::Binary { left, .. } if self.l.m.hir().node(base).kind == H::Binary => left,
                Form::Member {
                    object,
                    chain: false,
                    ..
                }
                | Form::Index {
                    object,
                    chain: false,
                    ..
                } => object,
                Form::Call {
                    callee,
                    arguments,
                    children,
                    chain: false,
                    ..
                } => {
                    if let Some(native) = self.l.native(callee)? {
                        break self.call_with_target(
                            base,
                            arguments,
                            children,
                            bc::CallTarget::Native(native),
                            depth,
                        )?;
                    }
                    callee
                }
                _ => break self.value(base, depth)?,
            };
            self.l.b.push(&mut segments, base)?;
            base = receiver;
        };
        let Some(mut value) = value else {
            self.l.b.release(segments);
            return Ok(None);
        };
        for &node in segments.iter().rev() {
            self.l.b.span = self.span(node);
            let next = match self.l.m.hir().node(node).form {
                Form::Binary {
                    operator, right, ..
                } => self.binary_value(node, value, operator, right, depth)?,
                Form::Member { key, .. } => {
                    let name = self
                        .l
                        .name(self.l.m.hir().name(key.name.expect("member name")))?;
                    let out = self.temp()?;
                    self.op(bc::Opcode::MemberGet, out.0, value.0, name.0, node)?;
                    self.release(value)?;
                    Some(out)
                }
                Form::Index { index, .. } => {
                    if let Some(key) = self.value(index, depth)? {
                        let out = self.temp()?;
                        self.op(bc::Opcode::IndexGet, out.0, value.0, key.0, node)?;
                        self.release(key)?;
                        self.release(value)?;
                        Some(out)
                    } else {
                        self.release(value)?;
                        None
                    }
                }
                Form::Call {
                    arguments,
                    children,
                    ..
                } => {
                    let out = self.call_with_target(
                        node,
                        arguments,
                        children,
                        bc::CallTarget::Register(value),
                        depth,
                    )?;
                    self.release(value)?;
                    out
                }
                _ => unreachable!(),
            };
            let Some(next) = next else {
                self.l.b.release(segments);
                return Ok(None);
            };
            value = next;
        }
        self.l.b.release(segments);
        Ok(Some(value))
    }

    pub(super) fn optional_chain(
        &mut self,
        root: h::NodeId,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let mut segments = Vec::new();
        let mut base = root;
        loop {
            self.l.b.span = self.span(base);
            self.l.b.work(1)?;
            let receiver = match self.l.m.hir().node(base).form {
                Form::Member {
                    object,
                    chain: true,
                    ..
                }
                | Form::Index {
                    object,
                    chain: true,
                    ..
                } => object,
                Form::Call {
                    callee,
                    chain: true,
                    ..
                } => callee,
                _ => break,
            };
            self.l.b.push(&mut segments, base)?;
            base = receiver;
        }
        let Some(receiver) = self.value(base, depth)? else {
            self.l.b.release(segments);
            return Ok(None);
        };
        // A later guard may see null after earlier successful, non-null segments.
        // Keep the null exit's result separate from the evolving receiver.
        let out = self.constant(bc::Const::Null, root)?;
        let join = self.block()?;
        self.l.b.work(segments.len() as u64)?;
        for &node in segments.iter().rev() {
            self.l.b.span = self.span(node);
            let form = self.l.m.hir().node(node).form;
            let optional = match form {
                Form::Member { optional, .. }
                | Form::Index { optional, .. }
                | Form::Call { optional, .. } => optional,
                _ => {
                    return Err(self
                        .l
                        .b
                        .error(ErrorKind::InvalidIr, "optional chain segment"));
                }
            };
            if optional {
                let next = self.block()?;
                self.terminate(
                    Terminator::BranchNull {
                        value: receiver,
                        null: join,
                        value_block: next,
                    },
                    node,
                )?;
                self.start(next)?;
            }
            let next = match form {
                Form::Member { key, .. } => {
                    let name = key.name.ok_or_else(|| {
                        self.l.b.error(ErrorKind::InvalidIr, "member name identity")
                    })?;
                    let symbol = self.l.name(self.l.m.hir().name(name))?;
                    let next = self.temp()?;
                    self.op(bc::Opcode::MemberGet, next.0, receiver.0, symbol.0, node)?;
                    Some(next)
                }
                Form::Index { index, .. } => {
                    if let Some(key) = self.value(index, depth + 1)? {
                        let next = self.temp()?;
                        self.op(bc::Opcode::IndexGet, next.0, receiver.0, key.0, node)?;
                        self.release(key)?;
                        Some(next)
                    } else {
                        None
                    }
                }
                Form::Call {
                    arguments,
                    children,
                    ..
                } => self.call_with_target(
                    node,
                    arguments,
                    children,
                    bc::CallTarget::Register(receiver),
                    depth + 1,
                )?,
                _ => {
                    return Err(self.l.b.error(
                        ErrorKind::Unsupported,
                        "optional children calls await children lowering",
                    ));
                }
            };
            let Some(next) = next else {
                // The non-null path has terminated; earlier null edges still join.
                break;
            };
            self.op(bc::Opcode::Move, receiver.0, next.0, 0, node)?;
            self.release(next)?;
        }
        self.l.b.release(segments);
        if self.current.is_some() {
            self.op(bc::Opcode::Move, out.0, receiver.0, 0, root)?;
            self.terminate(Terminator::Jump(join), root)?;
        }
        self.start(join)?;
        self.release(receiver)?;
        Ok(Some(out))
    }
}
