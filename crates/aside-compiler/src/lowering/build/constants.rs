//! Fold pure host-constant conditions after ordinary semantic analysis.
use super::*;
use crate::semantic::CompileConstant;

enum Step {
    Read(h::NodeId),
    Not,
    Binary(K),
    Right(K, h::NodeId),
}

pub(super) struct Evaluator<'m, 'b> {
    pub(super) m: &'m h::AnalyzedModule,
    pub(super) b: &'b mut Budget,
}

impl Evaluator<'_, '_> {
    pub(super) fn external_constant(
        &mut self,
        node: h::NodeId,
    ) -> Result<Option<bc::Const>, Box<Error>> {
        if !matches!(self.m.hir().node(node).kind, H::Name | H::Member) {
            return Ok(None);
        }
        self.b.work(1 + usize::BITS as u64)?;
        let Some(value) = self
            .m
            .external_binding(None, Some(node))
            .or_else(|| {
                self.m.facts()[node.0 as usize]
                    .symbol
                    .and_then(|symbol| self.m.external_binding(Some(symbol), None))
            })
            .filter(|binding| {
                binding.kind == h::ExternalBindingKind::Reference
                    || (binding.descriptor.intrinsic == Some(crate::semantic::Intrinsic::EnumCase)
                        && binding.descriptor.signature.is_none())
            })
            .and_then(|binding| binding.descriptor.constant.as_deref())
        else {
            return Ok(None);
        };
        Ok(Some(match value {
            CompileConstant::Null => bc::Const::Null,
            CompileConstant::Bool(v) => bc::Const::Bool(*v),
            CompileConstant::Int(v) => bc::Const::Int(*v),
            CompileConstant::FloatBits(v) => bc::Const::FloatBits(*v),
            CompileConstant::Utf8(v) => bc::Const::Utf8(self.b.string(v)?),
        }))
    }

    fn release_constant(&mut self, value: Option<bc::Const>) {
        if let Some(bc::Const::Utf8(text)) = value {
            self.b.release_bytes(text.capacity());
        }
    }

    /// Use an explicit stack so long boolean chains do not grow the Rust stack.
    /// Unknown operands keep their original runtime evaluation and diagnostics.
    pub(super) fn constant_condition(
        &mut self,
        root: h::NodeId,
    ) -> Result<Option<bool>, Box<Error>> {
        let mut steps = Vec::new();
        let mut values = Vec::new();
        self.b.push(&mut steps, Step::Read(root))?;
        while let Some(step) = steps.pop() {
            self.b.work(1)?;
            match step {
                Step::Read(node) => {
                    self.b.span = self.m.hir().node(node).span;
                    if let Some(value) = self.external_constant(node)? {
                        self.b.push(&mut values, Some(value))?;
                        continue;
                    }
                    match (self.m.hir().node(node).kind, self.m.hir().node(node).form) {
                        (H::Group, Form::Single { child }) => {
                            self.b.push(&mut steps, Step::Read(child))?;
                        }
                        (H::Prefix, Form::Unary { operator, value })
                            if operator.kind == K::Bang =>
                        {
                            self.b.push(&mut steps, Step::Not)?;
                            self.b.push(&mut steps, Step::Read(value))?;
                        }
                        (
                            H::Binary,
                            Form::Binary {
                                left,
                                operator,
                                right,
                            },
                        ) if matches!(operator.kind, K::EqEq | K::NotEq | K::AndAnd | K::OrOr) => {
                            self.b.push(&mut steps, Step::Right(operator.kind, right))?;
                            self.b.push(&mut steps, Step::Read(left))?;
                        }
                        _ => {
                            let value = self.literal_constant(node)?;
                            self.b.push(&mut values, value)?;
                        }
                    }
                }
                Step::Not => {
                    let value = values.pop().expect("constant operand");
                    let result = match &value {
                        Some(bc::Const::Bool(v)) => Some(bc::Const::Bool(!v)),
                        _ => None,
                    };
                    self.release_constant(value);
                    self.b.push(&mut values, result)?;
                }
                Step::Right(operator, right) => {
                    let left = values.last().expect("left constant");
                    let skip = left.is_none()
                        || matches!(
                            (operator, left),
                            (K::AndAnd, Some(bc::Const::Bool(false)))
                                | (K::OrOr, Some(bc::Const::Bool(true)))
                        );
                    if !skip {
                        self.b.push(&mut steps, Step::Binary(operator))?;
                        self.b.push(&mut steps, Step::Read(right))?;
                    }
                }
                Step::Binary(operator) => {
                    let right = values.pop().expect("right constant");
                    let left = values.pop().expect("left constant");
                    if let (Some(bc::Const::Utf8(a)), Some(bc::Const::Utf8(b))) = (&left, &right) {
                        self.b.work(a.len().min(b.len()) as u64)?;
                    }
                    let result = match (&left, &right, operator) {
                        (Some(bc::Const::Bool(a)), Some(bc::Const::Bool(b)), K::AndAnd) => {
                            Some(*a && *b)
                        }
                        (Some(bc::Const::Bool(a)), Some(bc::Const::Bool(b)), K::OrOr) => {
                            Some(*a || *b)
                        }
                        (Some(a), Some(b), K::EqEq | K::NotEq) => {
                            scalar_equal(a, b).map(|equal| equal == (operator == K::EqEq))
                        }
                        _ => None,
                    }
                    .map(bc::Const::Bool);
                    self.release_constant(left);
                    self.release_constant(right);
                    self.b.push(&mut values, result)?;
                }
            }
        }
        let value = values.pop().expect("root constant");
        let result = match value {
            Some(bc::Const::Bool(v)) => Some(v),
            _ => None,
        };
        self.release_constant(value);
        self.b.release(steps);
        self.b.release(values);
        Ok(result)
    }
}

fn scalar_equal(left: &bc::Const, right: &bc::Const) -> Option<bool> {
    use bc::Const;
    match (left, right) {
        (Const::Null, Const::Null) => Some(true),
        (Const::Bool(a), Const::Bool(b)) => Some(a == b),
        (Const::Int(a), Const::Int(b)) => Some(a == b),
        (Const::FloatBits(a), Const::FloatBits(b)) => {
            Some(f64::from_bits(*a) == f64::from_bits(*b))
        }
        (Const::Utf8(a), Const::Utf8(b)) => Some(a == b),
        _ => None,
    }
}

impl Lowerer<'_> {
    pub(super) fn external_constant(
        &mut self,
        node: h::NodeId,
    ) -> Result<Option<bc::Const>, Box<Error>> {
        Evaluator {
            m: self.m,
            b: &mut self.b,
        }
        .external_constant(node)
    }
    pub(super) fn constant_condition(
        &mut self,
        node: h::NodeId,
    ) -> Result<Option<bool>, Box<Error>> {
        Evaluator {
            m: self.m,
            b: &mut self.b,
        }
        .constant_condition(node)
    }
}

pub(in crate::lowering) fn selection(
    m: &h::AnalyzedModule,
    b: &mut Budget,
) -> Result<Vec<u8>, Box<Error>> {
    let mut selected = b.filled(m.hir().nodes.len(), 0)?;
    let mut pending = Vec::new();
    b.push(&mut pending, h::NodeId(0))?;
    while let Some(node) = pending.pop() {
        b.work(1)?;
        selected[node.0 as usize] = 1;
        if let (
            H::If,
            Form::Condition {
                pattern: None,
                condition,
                body,
                otherwise,
            },
        ) = (m.hir().node(node).kind, m.hir().node(node).form)
            && let Some(value) = (Evaluator { m, b }).constant_condition(condition)?
        {
            if let Some(branch) = if value { Some(body) } else { otherwise } {
                b.push(&mut pending, branch)?;
            }
            continue;
        }
        for child in m.hir().children(node) {
            b.push(&mut pending, child)?;
        }
    }
    b.release(pending);
    Ok(selected)
}

pub(in crate::lowering) fn condition(
    module: &h::AnalyzedModule,
    node: h::NodeId,
    budget: &mut Budget,
) -> Result<Option<bool>, Box<Error>> {
    Evaluator {
        m: module,
        b: budget,
    }
    .constant_condition(node)
}
