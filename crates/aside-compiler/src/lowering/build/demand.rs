//! Branch and discard uses retain evaluation and checks without materializing
//! a control-flow result that no consumer reads.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Demand {
    Value,
    Discard,
}

#[derive(Clone, Copy)]
struct Reach {
    yes: bool,
    no: bool,
}
impl Reach {
    const NONE: Self = Self {
        yes: false,
        no: false,
    };
    const BOTH: Self = Self {
        yes: true,
        no: true,
    };
    fn any(self) -> bool {
        self.yes || self.no
    }
}

struct LogicalRight {
    node: h::NodeId,
    operator: K,
    right: h::NodeId,
    block: BlockId,
    yes: BlockId,
    no: BlockId,
}

impl FunctionBuilder<'_, '_> {
    pub(super) fn branch_to(
        &mut self,
        node: h::NodeId,
        checked_at: h::NodeId,
        yes: BlockId,
        no: BlockId,
        depth: usize,
    ) -> Result<bool, Box<Error>> {
        Ok(self.branch_reach(node, checked_at, yes, no, depth)?.any())
    }

    fn branch_reach(
        &mut self,
        node: h::NodeId,
        checked_at: h::NodeId,
        yes: BlockId,
        no: BlockId,
        depth: usize,
    ) -> Result<Reach, Box<Error>> {
        match (
            self.l.m.hir().node(node).kind,
            self.l.m.hir().node(node).form,
        ) {
            (H::Group, Form::Single { child }) => {
                let depth = self.expression_depth(node, depth)?;
                self.branch_reach(child, checked_at, yes, no, depth)
            }
            (H::Binary, Form::Binary { operator, .. })
                if matches!(operator.kind, K::AndAnd | K::OrOr) =>
            {
                let depth = self.expression_depth(node, depth)?;
                self.logical_branch(node, yes, no, depth)
            }
            _ => {
                let Some(value) = self.value(node, depth)? else {
                    return Ok(Reach::NONE);
                };
                // Retain strict bool validation at the original logical or
                // condition node, even when both outcomes discard the result.
                self.terminate(Terminator::BranchBool { value, yes, no }, checked_at)?;
                self.release(value)?;
                Ok(Reach::BOTH)
            }
        }
    }

    fn logical_branch(
        &mut self,
        root: h::NodeId,
        yes: BlockId,
        no: BlockId,
        depth: usize,
    ) -> Result<Reach, Box<Error>> {
        let mut pending = Vec::new();
        let (mut node, mut checked_at) = (root, root);
        let (mut left_yes, mut left_no) = (yes, no);
        // A flat left-associated chain retains the existing iterative lowering;
        // parentheses and right-hand nesting still use the depth budget.
        while let (
            H::Binary,
            Form::Binary {
                left,
                operator,
                right,
            },
        ) = (
            self.l.m.hir().node(node).kind,
            self.l.m.hir().node(node).form,
        ) {
            if !matches!(operator.kind, K::AndAnd | K::OrOr) {
                break;
            }
            self.l.b.span = self.span(node);
            self.l.b.work(1)?;
            let block = self.block()?;
            self.l.b.push(
                &mut pending,
                LogicalRight {
                    node,
                    operator: operator.kind,
                    right,
                    block,
                    yes: left_yes,
                    no: left_no,
                },
            )?;
            if operator.kind == K::AndAnd {
                left_yes = block;
            } else {
                left_no = block;
            }
            checked_at = node;
            node = left;
        }
        let mut reach = self.branch_reach(node, checked_at, left_yes, left_no, depth)?;
        while let Some(right) = pending.pop() {
            let evaluate = if right.operator == K::AndAnd {
                reach.yes
            } else {
                reach.no
            };
            let rhs = if evaluate {
                self.start(right.block)?;
                self.branch_reach(right.right, right.node, right.yes, right.no, depth)?
            } else {
                Reach::NONE
            };
            reach = if right.operator == K::AndAnd {
                Reach {
                    yes: reach.yes && rhs.yes,
                    no: reach.no || (reach.yes && rhs.no),
                }
            } else {
                Reach {
                    yes: reach.yes || (reach.no && rhs.yes),
                    no: reach.no && rhs.no,
                }
            };
        }
        self.l.b.release(pending);
        Ok(reach)
    }

    pub(super) fn discard_logical(
        &mut self,
        node: h::NodeId,
        depth: usize,
    ) -> Result<(), Box<Error>> {
        let join = self.block()?;
        if self.logical_branch(node, join, join, depth)?.any() {
            self.start(join)?;
        }
        Ok(())
    }

    pub(super) fn discard_coalesce(
        &mut self,
        node: h::NodeId,
        left: h::NodeId,
        right: h::NodeId,
        depth: usize,
    ) -> Result<(), Box<Error>> {
        let Some(value) = self.value(left, depth)? else {
            return Ok(());
        };
        let rhs = self.block()?;
        let join = self.block()?;
        self.terminate(
            Terminator::BranchNull {
                value,
                null: rhs,
                value_block: join,
            },
            node,
        )?;
        self.release(value)?;
        self.start(rhs)?;
        self.discard(right, depth)?;
        if self.current.is_some() {
            self.terminate(Terminator::Jump(join), node)?;
        }
        self.start(join)
    }
}
