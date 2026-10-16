//! Lexical children collection and ordinary sequence loops.
use super::*;

impl FunctionBuilder<'_, '_> {
    pub(super) fn call_opcode(&self) -> bc::Opcode {
        if self.no_wait {
            bc::Opcode::CallNoWait
        } else {
            bc::Opcode::Call
        }
    }

    pub(super) fn collects(&self, mut node: h::NodeId) -> bool {
        while let (H::Group, Form::Single { child }) = (
            self.l.m.hir().node(node).kind,
            self.l.m.hir().node(node).form,
        ) {
            node = child;
        }
        !matches!(
            self.l.m.hir().node(node).kind,
            H::Assignment
                | H::Block
                | H::If
                | H::Match
                | H::For
                | H::While
                | H::Loop
                | H::Try
                | H::Return
                | H::Throw
                | H::Break
                | H::Continue
                | H::Label
        )
    }

    pub(super) fn children_value(
        &mut self,
        node: h::NodeId,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let children = self.temp()?;
        let descriptor = self.descriptor(bc::Descriptor::Sequence(bc::RegWindow {
            start: children,
            len: 0,
        }))?;
        self.op(bc::Opcode::MakeArray, children.0, descriptor.0, 0, node)?;
        let previous = self.collector.replace(children);
        let no_wait = std::mem::replace(&mut self.no_wait, true);
        for body in self.l.m.hir().children(node) {
            if let Some(value) = self.expr(body, depth)? {
                self.release(value)?;
            }
            if self.current.is_none() {
                break;
            }
        }
        self.collector = previous;
        self.no_wait = no_wait;
        if self.current.is_some() {
            Ok(Some(children))
        } else {
            self.release(children)?;
            Ok(None)
        }
    }

    pub(super) fn for_node(
        &mut self,
        node: h::NodeId,
        pattern: h::NodeId,
        source: h::NodeId,
        body: h::NodeId,
        depth: usize,
    ) -> Result<(), Box<Error>> {
        let Some(source) = self.value(source, depth)? else {
            return Ok(());
        };
        let iterator = self.temp()?;
        self.op(bc::Opcode::IterInit, iterator.0, source.0, 0, node)?;
        self.release(source)?;
        let head = self.block()?;
        let body_block = self.block()?;
        let end = self.block()?;
        self.terminate(Terminator::Jump(head), node)?;
        self.start(head)?;
        let done = self.temp()?;
        let value = self.temp()?;
        self.op(bc::Opcode::IterAdvance, done.0, iterator.0, value.0, node)?;
        self.terminate(
            Terminator::BranchBool {
                value: done,
                yes: end,
                no: body_block,
            },
            node,
        )?;
        self.release(done)?;
        self.start(body_block)?;
        self.bind_value(pattern, value, depth, true)?;
        self.release(value)?;
        self.l.b.push(
            &mut self.loops,
            LoopTarget {
                node,
                next: head,
                end,
                protected_depth: self.protected_depth,
                has_break: false,
            },
        )?;
        if let Some(value) = self.expr(body, depth)? {
            self.release(value)?;
        }
        if self.current.is_some() {
            self.terminate(Terminator::Jump(head), node)?;
        }
        self.loops.pop();
        self.start(end)?;
        self.op(bc::Opcode::IterClose, iterator.0, 0, 0, node)?;
        self.release(iterator)?;
        Ok(())
    }
}
