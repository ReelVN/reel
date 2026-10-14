//! Calls expand sequences positionally and own table entries by name.
use super::*;

impl Binder<'_> {
    fn spread_type_possible(&mut self, node: NodeId, ty: TypeId) -> bool {
        if !self.budget(node) {
            return true;
        }
        match &self.out.types[ty.0 as usize] {
            Type::Any | Type::Named(_) | Type::Array | Type::Tuple(_) | Type::Table => true,
            Type::Union(types) => {
                let count = types.len();
                if !self.charge_work(node, count) {
                    return true;
                }
                for i in 0..count {
                    let Type::Union(types) = &self.out.types[ty.0 as usize] else {
                        unreachable!()
                    };
                    let child = types[i];
                    if self.spread_type_possible(node, child) {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    pub(super) fn check_argument_spread(&mut self, node: NodeId) {
        // Terminal transfers have no value to expand; their inferred null is
        // only a placeholder, not a source value supplied to ArgsSpread.
        if let Form::Spread { value } = self.out.hir.node(node).form
            && !self.falls_through(value)
        {
            return;
        }
        let ty = self.ty(node);
        if !self.spread_type_possible(node, ty) {
            self.error(
                D::TypeMismatch,
                self.span(node),
                "call spread requires array, tuple or table",
            );
        } else if !matches!(self.out.types[ty.0 as usize], Type::Array | Type::Tuple(_)) {
            self.check(node, RuntimeChecks::TYPE);
        }
    }
}
