//! One null exit per continuous postfix chain; Group remains a value boundary.
use super::*;

impl FunctionBuilder<'_, '_> {
    pub(super) fn optional_chain(
        &mut self,
        root: h::NodeId,
        mut depth: usize,
    ) -> Result<Option<bc::Reg>, Error> {
        let mut segments = Vec::new();
        let mut base = root;
        loop {
            self.l.b.span = self.span(base);
            self.l.b.work(1)?;
            if depth >= self.l.b.limits.max_depth.min(128) {
                return Err(self
                    .l
                    .b
                    .error(ErrorKind::Limit, "optional chain depth exceeded"));
            }
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
            self.l.b.push(&mut segments, (base, depth))?;
            base = receiver;
            depth += 1;
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
        for &(node, depth) in segments.iter().rev() {
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
                    children: None,
                    ..
                } => self.call_with_target(
                    node,
                    arguments,
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
