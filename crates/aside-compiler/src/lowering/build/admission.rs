//! Source compatibility and lowering admission.
use super::*;

pub(super) fn source_hint_matches(
    a: &crate::semantic::TypeHint,
    b: &crate::semantic::TypeHint,
    budget: &mut Budget,
    depth: usize,
) -> Result<bool, Box<Error>> {
    use crate::semantic::TypeHint as T;
    budget.work(1)?;
    if depth >= budget.limits.max_depth {
        return Err(budget.error(
            ErrorKind::Limit,
            "trusted source type comparison depth exceeded",
        ));
    }
    match (a, b) {
        (T::Named(a), T::Named(b)) => Ok(budget.compare(a, b)?.is_eq()),
        (T::Tuple(a), T::Tuple(b)) | (T::Union(a), T::Union(b)) => {
            if a.len() != b.len() {
                return Ok(false);
            }
            for (a, b) in a.iter().zip(b) {
                if !source_hint_matches(a, b, budget, depth + 1)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (T::Callable(a, result_a), T::Callable(b, result_b)) => {
            if a.len() != b.len() {
                return Ok(false);
            }
            for (a, b) in a.iter().zip(b) {
                if !source_hint_matches(a, b, budget, depth + 1)? {
                    return Ok(false);
                }
            }
            source_hint_matches(result_a, result_b, budget, depth + 1)
        }
        (T::Tuple(_) | T::Union(_) | T::Callable(..) | T::Named(_), _)
        | (_, T::Tuple(_) | T::Union(_) | T::Callable(..) | T::Named(_)) => Ok(false),
        _ => Ok(a == b),
    }
}
pub(super) fn source_signature_matches(
    a: &crate::semantic::ExternalSignature,
    b: &crate::semantic::ExternalSignature,
    budget: &mut Budget,
) -> Result<bool, Box<Error>> {
    budget.work(1)?;
    if a.parameters.len() != b.parameters.len()
        || a.result_cases.len() != b.result_cases.len()
        || a.effect != b.effect
        || a.init_safe != b.init_safe
        || a.derived_safe != b.derived_safe
    {
        return Ok(false);
    }
    if !match (&a.result_parameter, &b.result_parameter) {
        (
            Some(crate::semantic::ExternalResultParameter::Value(a)),
            Some(crate::semantic::ExternalResultParameter::Value(b)),
        )
        | (
            Some(crate::semantic::ExternalResultParameter::Callback(a)),
            Some(crate::semantic::ExternalResultParameter::Callback(b)),
        ) => budget.compare(a, b)?.is_eq(),
        (None, None) => true,
        _ => false,
    } {
        return Ok(false);
    }
    for (a, b) in a.parameters.iter().zip(&b.parameters) {
        if !budget.compare(&a.name, &b.name)?.is_eq()
            || a.mutable != b.mutable
            || a.default != b.default
            || a.rest != b.rest
            || !source_hint_matches(&a.ty, &b.ty, budget, 0)?
        {
            return Ok(false);
        }
    }
    for (a, b) in a.result_cases.iter().zip(&b.result_cases) {
        if !budget.compare(&a.parameter, &b.parameter)?.is_eq()
            || a.when_omitted != b.when_omitted
            || !source_constant_matches(&a.equals, &b.equals, budget)?
            || !source_hint_matches(&a.result, &b.result, budget, 0)?
        {
            return Ok(false);
        }
    }
    source_hint_matches(&a.result, &b.result, budget, 0)
}

pub(super) fn source_constant_matches(
    a: &bc::Const,
    b: &bc::Const,
    budget: &mut Budget,
) -> Result<bool, Box<Error>> {
    budget.work(1)?;
    match (a, b) {
        (bc::Const::Utf8(a), bc::Const::Utf8(b)) => Ok(budget.compare(a, b)?.is_eq()),
        (bc::Const::Bytes(a), bc::Const::Bytes(b)) => {
            budget.work(a.len().min(b.len()) as u64)?;
            Ok(a == b)
        }
        (bc::Const::Tuple(a), bc::Const::Tuple(b)) => {
            budget.work(a.len().min(b.len()) as u64)?;
            Ok(a == b)
        }
        _ => Ok(a == b),
    }
}

pub(super) fn hint_matches(
    hint: &crate::semantic::TypeHint,
    id: bc::TypeId,
    p: &bc::BytecodePackage,
    b: &mut Budget,
    depth: usize,
) -> Result<bool, Box<Error>> {
    b.work(1)?;
    if depth >= b.limits.max_depth {
        return Err(b.error(
            ErrorKind::Limit,
            "trusted native type comparison depth exceeded",
        ));
    }
    use crate::semantic::TypeHint as T;
    use bc::BuiltinType as B;
    let builtin = match hint {
        T::Any => Some(B::Any),
        T::Closure => Some(B::Closure),
        T::Null => Some(B::Null),
        T::Bool => Some(B::Bool),
        T::Int => Some(B::Int),
        T::Float => Some(B::Float),
        T::String => Some(B::String),
        T::Array => Some(B::Array),
        T::Table => Some(B::Table),
        T::Future => Some(B::Future),
        T::Generator => Some(B::Generator),
        T::Named(name) if name == "Bytes" => Some(B::Bytes),
        T::Named(name) if name == "Binding" => Some(B::PropertyRef),
        T::Named(name) if name == "tuple" => Some(B::Tuple),
        _ => None,
    };
    let actual = &p.types[id.index()];
    if let Some(builtin) = builtin {
        return Ok(*actual == bc::TypeDef::Builtin(builtin));
    }
    match (hint, actual) {
        (T::Tuple(expected), bc::TypeDef::Tuple(actual))
        | (T::Union(expected), bc::TypeDef::Union(actual)) => {
            if expected.len() != actual.len() {
                return Ok(false);
            }
            b.work(expected.len() as u64)?;
            for (expected, &actual) in expected.iter().zip(actual) {
                if !hint_matches(expected, actual, p, b, depth + 1)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (T::Named(expected), bc::TypeDef::Named { module, name, .. }) => {
            let Some((path, expected_name)) = expected.rsplit_once("::") else {
                return Ok(false);
            };
            Ok(
                b.compare(path, &p.symbols[p.modules[module.index()].path.index()])?
                    .is_eq()
                    && b.compare(expected_name, &p.symbols[name.index()])?.is_eq(),
            )
        }
        _ => Ok(false),
    }
}

pub(super) fn preflight(
    m: &h::AnalyzedModule,
    links: &Linkage<'_>,
    project: Option<&ProjectLinks>,
    b: &mut Budget,
) -> Result<(), Box<Error>> {
    expansions::validate(m, links.expansions, b)?;
    let root_scope = m.facts()[0].scope.ok_or_else(|| {
        b.error(
            ErrorKind::InvalidIr,
            "analyzed entry lacks its lexical scope",
        )
    })?;
    b.work(m.symbols().len() as u64)?;
    for symbol in m.symbols() {
        b.span = symbol.declaration;
        if symbol.owner != symbol.origin.function() {
            return Err(b.error(
                ErrorKind::InvalidIr,
                "binding origin/function owner mismatch",
            ));
        }
        if symbol.initialization == h::Initialization::External
            && !matches!(symbol.kind, h::SymbolKind::Host | h::SymbolKind::Import)
        {
            return Err(b.error(
                ErrorKind::MissingLink,
                format!("enclosing lexical binding {} requires a real external Value/Cell link; native metadata does not initialize it", m.hir().name(symbol.name)),
            ));
        }
        if symbol.origin == h::BindingOrigin::ModuleGlobal
            && (m.entry_context() != crate::semantic::AnalysisContext::Module
                || symbol.scope != root_scope
                || symbol.per_iteration)
        {
            return Err(b.error(
                ErrorKind::InvalidIr,
                "global is not a root module declaration",
            ));
        }
        if symbol.origin == h::BindingOrigin::EntryLocal
            && m.entry_context() != crate::semantic::AnalysisContext::Module
        {
            return Err(b.error(
                ErrorKind::Unsupported,
                "entry-local bindings require a module initializer or a real author function; ownerless NoWait fragments are unsupported",
            ));
        }
    }
    b.work(m.exports().len() as u64)?;
    for &sym in m.exports() {
        let symbol = &m.symbols()[sym.0 as usize];
        if m.entry_context() != crate::semantic::AnalysisContext::Module
            || symbol.scope != root_scope
        {
            b.span = symbol.declaration;
            return Err(b.error(
                ErrorKind::Unsupported,
                "only root module declarations may be exported",
            ));
        }
    }
    b.work(m.hir().nodes.len() as u64)?;
    for (i, node) in m.hir().nodes.iter().enumerate() {
        b.span = node.span;
        if matches!(node.kind, H::ArrayPattern | H::RestPattern) {
            return Err(b.error(
                ErrorKind::Unsupported,
                format!("{:?} is outside L04 lowering support", node.kind),
            ));
        }
        if node.kind == H::Spread
            && node.parent.is_none_or(|parent| {
                !matches!(
                    m.hir().node(parent).kind,
                    H::Arguments | H::Array | H::Object | H::ExpressionStatement
                )
            })
        {
            return Err(b.error(
                ErrorKind::Unsupported,
                "spread awaits ordered builder lowering",
            ));
        }
        if let Form::Binary { left, .. } = node.form
            && node.kind == H::Assignment
        {
            let mut target = left;
            let mut depth = 0;
            while let (H::Group, Form::Single { child }) =
                (m.hir().node(target).kind, m.hir().node(target).form)
            {
                b.work(1)?;
                depth += 1;
                if depth >= b.limits.max_depth {
                    return Err(b.error(ErrorKind::Limit, "assignment target depth exceeded"));
                }
                target = child;
            }
            if matches!(
                m.hir().node(target).form,
                Form::Member { chain: true, .. } | Form::Index { chain: true, .. }
            ) {
                b.span = m.hir().node(target).span;
                return Err(b.error(
                    ErrorKind::Unsupported,
                    "optional chain assignment target is unsupported",
                ));
            }
        }
        if let Form::Condition {
            pattern: Some(_), ..
        } = node.form
        {
            return Err(b.error(
                ErrorKind::Unsupported,
                "conditional patterns await later lowering",
            ));
        }
        if node.kind == H::Member {
            let count = m.external_bindings().len();
            if count != 0 {
                b.work(1 + (usize::BITS - (count - 1).leading_zeros()) as u64)?;
            }
            if m.external_binding(None, Some(h::NodeId(i as u32)))
                .is_some_and(|binding| {
                    binding.kind == h::ExternalBindingKind::Reference
                        && binding.descriptor.reference.is_some()
                })
            {
                b.work(links.natives.len() as u64)?;
                if m.external_binding(None, Some(h::NodeId(i as u32)))
                    .is_none_or(|binding| binding.descriptor.constant.is_none())
                    && project.is_none_or(|p| p.node(h::NodeId(i as u32)).is_none())
                    && !links
                        .natives
                        .iter()
                        .any(|link| link.target == ExternalTarget::Node(h::NodeId(i as u32)))
                {
                    return Err(b.error(
                        ErrorKind::MissingLink,
                        "qualified value has no source or native linkage",
                    ));
                }
                let mut target = h::NodeId(i as u32);
                let mut depth = 1;
                while let Some(parent) = m.hir().node(target).parent {
                    b.work(1)?;
                    match m.hir().node(parent).form {
                        Form::Single { child }
                            if m.hir().node(parent).kind == H::Group && child == target =>
                        {
                            depth += 1;
                            if depth >= b.limits.max_depth {
                                return Err(
                                    b.error(ErrorKind::Limit, "qualified callee depth exceeded")
                                );
                            }
                            target = parent;
                        }
                        Form::Binary { left, .. }
                            if m.hir().node(parent).kind == H::Assignment && left == target =>
                        {
                            return Err(
                                b.error(ErrorKind::Unsupported, "imported bindings are read-only")
                            );
                        }
                        _ => break,
                    }
                }
            }
        }
    }
    Ok(())
}
