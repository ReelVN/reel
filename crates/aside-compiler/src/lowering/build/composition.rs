//! Lexical children collection and sequential iteration with owned cleanup.
use super::*;

impl FunctionBuilder<'_, '_> {
    pub(super) fn call_opcode(&self) -> bc::Opcode {
        if self.no_wait {
            bc::Opcode::CallNoWait
        } else {
            bc::Opcode::Call
        }
    }

    pub(super) fn collects(&self, node: h::NodeId) -> bool {
        self.l.m.hir().collects(node)
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
        let region = bc::RegionId(self.ir.regions.len() as u32);
        self.l.b.push(
            &mut self.ir.regions,
            Region {
                protected: 0..0,
                catch: None,
                finally: None,
            },
        )?;
        let head = self.block()?;
        let body_block = self.block()?;
        let exhausted = self.block()?;
        let end = self.block()?;
        self.terminate(Terminator::Jump(head), node)?;
        let begin = self.ir.layout.len() as u32;
        self.start(head)?;
        self.protected_depth += 1;
        let done = self.temp()?;
        let value = self.temp()?;
        self.op(bc::Opcode::IterAdvance, done.0, iterator.0, value.0, node)?;
        self.terminate(
            Terminator::BranchBool {
                value: done,
                yes: exhausted,
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
                protected_depth: self.protected_depth - 1,
                next_protected_depth: self.protected_depth,
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
        self.start(exhausted)?;
        self.terminate(Terminator::Leave(end), node)?;
        self.protected_depth -= 1;
        self.ir.regions[region.index()].protected = begin..self.ir.layout.len() as u32;
        let cleanup = self.block()?;
        let begin = self.ir.layout.len() as u32;
        self.start(cleanup)?;
        self.op(bc::Opcode::IterClose, iterator.0, 0, 0, node)?;
        self.terminate(Terminator::EndFinally(region), node)?;
        self.ir.regions[region.index()].finally = Some(begin..self.ir.layout.len() as u32);
        self.start(end)?;
        self.release(iterator)?;
        Ok(())
    }
}
