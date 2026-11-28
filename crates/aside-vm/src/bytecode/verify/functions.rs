//! Function, call, descriptor and pattern verification.
use super::*;

pub(super) fn validate_function(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    fi: usize,
    f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    parameter_layouts: &ParameterLayouts,
    uses: &mut InstructionUses,
    b: &mut Budget,
) -> Result<usize, Error> {
    get(&p.modules, f.module.0, "function module")?;
    symbol(p, f.name)?;
    let s = get(&p.signatures, f.signature.0, "function signature")?;
    if let FunctionKind::Native(id) = f.kind {
        let native = get(&p.natives, id.0, "callable native registration")?;
        check(
            f.signature == native.signature
                && f.context == Context::Ordinary
                && f.captures.is_empty()
                && f.bindings.is_empty()
                && f.descriptors.is_empty()
                && f.code.is_empty()
                && f.regions.is_empty()
                && f.register_count == 0,
            ErrorKind::Signature,
            "native callable uses the registration's preparation contract",
        )?;
        return validate_source_map(p, f, true).map(|()| 0);
    }
    b.work(s.parameters.len() as u64)?;
    for (i, param) in s.parameters.iter().enumerate() {
        if let Some(ParameterDefault::Function(default_id)) = param.default {
            let default = &p.functions[default_id.index()];
            b.work(default.captures.len() as u64)?;
            check(
                default.captures == f.captures && default.module == f.module,
                ErrorKind::Capture,
                "default shares the callee lexical capture layout/module",
            )?;
            b.work(i as u64)?;
            for (index, prefix) in s.parameters[..i].iter().enumerate() {
                let owner_cell = parameter_layouts.cell(fi, index as u32, b)?;
                let default_cell = parameter_layouts.cell(default_id.index(), index as u32, b)?;
                check(
                    owner_cell == default_cell,
                    ErrorKind::Signature,
                    "default prefix parameter storage layout",
                )?;
                check(
                    !prefix.mutable || owner_cell,
                    ErrorKind::Signature,
                    "mutable default prefix requires a shared parameter cell",
                )?;
            }
        }
    }
    check(
        f.register_count <= b.limits.max_registers
            && f.register_count as usize >= s.parameters.len() + usize::from(s.rest.is_some()),
        ErrorKind::Signature,
        "parameter register slots",
    )?;
    check(!f.code.is_empty(), ErrorKind::ControlFlow, "empty function")?;
    for descriptor in f.descriptors.iter() {
        validate_descriptor(p, f, descriptor, b)?;
    }
    validate_source_map(p, f, false)?;
    validate_regions(f, b)?;
    let mut literal_checks = 0;
    for item in f.code.ranged_iter() {
        let (range, i) = item?;
        let pc = range.start;
        validate_instruction(p, f, range, &i, b).map_err(|e| located(p, fi, pc, e))?;
        if f.regions.is_empty() && text_matches::is_case(&p.types, i) {
            literal_checks += 1;
        }
        if matches!(i.opcode, op if op == Opcode::MemberGet as u16 || op == Opcode::MemberSet as u16)
        {
            let symbol = i.operands[2] as usize;
            uses.members[symbol / 32] |= 1 << (symbol % 32);
        }
        if !uses.seen[i.opcode as usize] {
            uses.seen[i.opcode as usize] = true;
            uses.opcodes.push(OpcodeUse {
                opcode: i.opcode,
                function: FunctionId(fi as u32),
                pc: Pc(pc as u32),
            });
        }
    }
    Ok(literal_checks)
}
pub(super) fn validate_source_map(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    native: bool,
) -> Result<(), Error> {
    let mut prev = None;
    for m in f.source_map.entries() {
        check(
            (if native {
                m.pc.0 == 0
            } else {
                m.pc.index() < f.code.end_pc() && f.code.is_boundary(m.pc.index())
            }) && prev.is_none_or(|p| p < m.pc),
            ErrorKind::Source,
            "source PC order/range",
        )?;
        let file = get(&p.sources, m.span.file.0, "source file")?;
        check(
            m.span.start <= m.span.end
                && m.span.end as usize <= file.text.len()
                && file.text.is_char_boundary(m.span.start as usize)
                && file.text.is_char_boundary(m.span.end as usize),
            ErrorKind::Source,
            "UTF-8 source byte span",
        )?;
        prev = Some(m.pc);
    }
    Ok(())
}
pub(super) fn validate_call(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    c: &CallDesc,
    b: &mut Budget,
) -> Result<(), Error> {
    if let Some(contract) = c.contract {
        check(
            matches!(
                get(&p.types, contract.0, "call contract")?,
                TypeDef::Function { .. }
            ),
            ErrorKind::Signature,
            "call contract must be a function type",
        )?;
    }
    let sig = match c.target {
        CallTarget::Register(r) => {
            reg(f, r)?;
            None
        }
        CallTarget::Function(id) => {
            let target = get(&p.functions, id.0, "call function")?;
            check(
                target.captures.is_empty(),
                ErrorKind::Capture,
                "direct function call requires empty captures",
            )?;
            Some(get(&p.signatures, target.signature.0, "callee signature")?)
        }
        CallTarget::Native(id) => {
            let n = get(&p.natives, id.0, "call native")?;
            Some(get(&p.signatures, n.signature.0, "native signature")?)
        }
    };
    match c.arguments {
        Arguments::Packed(r) => {
            reg(f, r)?;
            check(
                c.names.is_empty(),
                ErrorKind::Signature,
                "packed builder owns argument names",
            )?;
        }
        Arguments::Window(w) => {
            window(f, w)?;
            check(
                c.names.len() <= w.len as usize,
                ErrorKind::Signature,
                "named argument count",
            )?;
            let positional = w.len as usize - c.names.len();
            unique_slice(&c.names, |n| n.name.0, b, "duplicate named argument")?;
            for (i, n) in c.names.iter().enumerate() {
                symbol(p, n.name)?;
                check(
                    n.offset as usize == positional + i,
                    ErrorKind::Signature,
                    "named arguments occupy ordered window tail",
                )?;
            }
            if let Some(s) = sig {
                check(
                    positional <= s.parameters.len() || s.rest.is_some(),
                    ErrorKind::Signature,
                    "extra positional arguments",
                )?;
                for name in &c.names {
                    b.work(1)?;
                    check(
                        s.rest.as_ref().is_none_or(|rest| rest.name != name.name),
                        ErrorKind::Signature,
                        "named argument cannot supply rest",
                    )?;
                    b.work(s.parameters.len() as u64)?;
                    if let Some(index) = s.parameters.iter().position(|p| p.name == name.name) {
                        check(
                            index >= positional,
                            ErrorKind::Signature,
                            "positional/named duplicate",
                        )?;
                    } else {
                        check(
                            s.property_bag_tail.is_some(),
                            ErrorKind::Signature,
                            "unknown named argument",
                        )?;
                    }
                }
                b.work(s.parameters.len() as u64)?;
                for (i, param) in s.parameters.iter().enumerate() {
                    b.work(c.names.len() as u64)?;
                    check(
                        i < positional
                            || param.default.is_some()
                            || c.names.iter().any(|n| n.name == param.name),
                        ErrorKind::Signature,
                        "missing required argument",
                    )?;
                }
            }
        }
    }
    Ok(())
}
pub(super) fn validate_descriptor(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    d: &Descriptor,
    b: &mut Budget,
) -> Result<(), Error> {
    match d {
        Descriptor::Global { module, global } => {
            let m = get(&p.modules, module.0, "global module")?;
            check(
                global.index() < m.globals.len() + m.imports.len(),
                ErrorKind::Index,
                "global binding",
            )?;
        }
        Descriptor::Closure { function, captures } => {
            validate_captures(p, f, *function, captures, b)?
        }
        Descriptor::Call(c) => validate_call(p, f, c, b)?,
        Descriptor::Sequence(w) => window(f, *w)?,
        Descriptor::Table(pairs) => {
            for pair in pairs {
                b.work(1)?;
                reg(f, pair[0])?;
                reg(f, pair[1])?;
            }
        }
        Descriptor::TableSources(sources) => {
            for source in sources {
                b.work(1)?;
                match source {
                    TableSource::Pair(pair) => {
                        reg(f, pair[0])?;
                        reg(f, pair[1])?;
                    }
                    TableSource::Spread(source) => reg(f, *source)?,
                }
            }
        }
        Descriptor::Class(c) => {
            if let Some(id) = c.nominal {
                ty(p, id)?;
                check(
                    matches!(p.types[id.index()], TypeDef::Named { module, .. } if module == f.module),
                    ErrorKind::Signature,
                    "class nominal type module",
                )?;
            }
            if let Some(base) = c.base {
                reg(f, base)?;
            }
            unique_slice(
                &c.fields,
                |field| (field.name.0, field.is_static),
                b,
                "duplicate class field",
            )?;
            unique_slice(
                &c.methods,
                |method| (method.name.0, method.is_static),
                b,
                "duplicate class method",
            )?;
            for field in &c.fields {
                symbol(p, field.name)?;
                ty(p, field.ty)?;
                if let Some(id) = field.initializer {
                    let init = get(&p.functions, id.0, "field initializer")?;
                    check(
                        init.kind == FunctionKind::Plain && init.context == Context::NoWait,
                        ErrorKind::Signature,
                        "field initializer context",
                    )?;
                    validate_captures(p, f, id, &field.captures, b)?;
                } else {
                    check(
                        field.captures.is_empty(),
                        ErrorKind::Capture,
                        "captures without field initializer",
                    )?;
                }
            }
            for method in &c.methods {
                symbol(p, method.name)?;
                let mf = get(&p.functions, method.function.0, "class method")?;
                let ms = get(&p.signatures, mf.signature.0, "method signature")?;
                validate_captures(p, f, method.function, &method.captures, b)?;
                if !method.is_static {
                    b.work(10)?; // Four-byte receiver and initializer names.
                    check(
                        !ms.parameters.is_empty() && symbol(p, ms.parameters[0].name)? == "self",
                        ErrorKind::Signature,
                        "instance method explicit self",
                    )?;
                    check(
                        symbol(p, method.name)? != "init" || mf.kind != FunctionKind::Generator,
                        ErrorKind::Signature,
                        "generator initializer",
                    )?;
                }
                b.work(c.fields.len() as u64)?;
                check(
                    !c.fields.iter().any(|field| {
                        field.name == method.name && field.is_static == method.is_static
                    }),
                    ErrorKind::Signature,
                    "field/method conflict",
                )?;
            }
        }
        Descriptor::Super { base, member } => {
            reg(f, *base)?;
            symbol(p, *member)?;
        }
        Descriptor::Property { key, .. } => reg(f, *key)?,
        Descriptor::Pattern(pattern) => validate_pattern(p, f, pattern, b)?,
        Descriptor::Spawn { call, owner } => {
            reg(f, *owner)?;
            validate_call(p, f, call, b)?;
        }
    }
    Ok(())
}
pub(super) fn validate_captures(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    function: FunctionId,
    captures: &[CaptureSource],
    b: &mut Budget,
) -> Result<(), Error> {
    let target = get(&p.functions, function.0, "closure function")?;
    check(
        target.captures.len() == captures.len(),
        ErrorKind::Capture,
        "capture layout count",
    )?;
    for (source, expected) in captures.iter().zip(&target.captures) {
        b.work(1)?;
        let actual = match source {
            CaptureSource::Binding(id) => {
                let binding = get(&f.bindings, id.0, "capture source binding")?;
                match binding.storage {
                    BindingStorage::Value(_) => {
                        check(
                            !binding.mutable,
                            ErrorKind::Capture,
                            "mutable capture needs a shared cell",
                        )?;
                        CaptureKind::Value
                    }
                    BindingStorage::Cell(_) => CaptureKind::Cell,
                }
            }
            CaptureSource::Capture(id) => *get(&f.captures, id.0, "forwarded capture")?,
        };
        check(
            actual == *expected,
            ErrorKind::Capture,
            "capture kind mismatch",
        )?;
    }
    Ok(())
}
pub(super) fn validate_pattern(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    pattern: &PatternDesc,
    b: &mut Budget,
) -> Result<(), Error> {
    check(
        (pattern.root as usize) < pattern.nodes.len(),
        ErrorKind::Index,
        "pattern root",
    )?;
    // Bindings per subtree are compared without cloning complete subtree sets.
    for (i, node) in pattern.nodes.iter().enumerate() {
        let child = |x: u32| {
            check(
                (x as usize) < i,
                ErrorKind::Index,
                "pattern child must refer backwards",
            )
        };
        match node {
            PatternNode::Wildcard => {}
            PatternNode::Bind { name, output } => {
                symbol(p, *name)?;
                reg(f, *output)?;
            }
            PatternNode::Literal(c) => {
                get(&p.constants, c.0, "pattern literal")?;
            }
            PatternNode::Tuple { items, rest } | PatternNode::Array { items, rest } => {
                for x in items {
                    b.work(1)?;
                    child(*x)?;
                }
                if let Some(r) = rest {
                    child(*r)?;
                }
            }
            PatternNode::Object { fields, rest } => {
                unique_slice(fields, |field| field.name.0, b, "duplicate pattern field")?;
                for field in fields {
                    symbol(p, field.name)?;
                    child(field.node)?;
                    if let Some(id) = field.default {
                        let default = get(&p.functions, id.0, "pattern default")?;
                        check(
                            default.kind == FunctionKind::Plain
                                && default.context == Context::NoWait,
                            ErrorKind::Signature,
                            "pattern default context",
                        )?;
                        validate_captures(p, f, id, &field.captures, b)?;
                    } else {
                        check(
                            field.captures.is_empty(),
                            ErrorKind::Capture,
                            "pattern captures without default",
                        )?;
                    }
                }
                if let Some(r) = rest {
                    child(*r)?;
                }
            }
            PatternNode::Is { node, ty: kind } => {
                child(*node)?;
                ty(p, *kind)?;
            }
            PatternNode::Or { left, right } => {
                child(*left)?;
                child(*right)?;
            }
        }
    }
    for (i, node) in pattern.nodes.iter().enumerate() {
        let left = pattern_bindings(pattern, i as u32, b)?;
        // OR collection uses the left branch after validating both branches.
        unique_slice(&left, |x| x.0.0, b, "duplicate pattern binding")?;
        unique_slice(&left, |x| x.1.0, b, "duplicate pattern output register")?;
        if let PatternNode::Or {
            left: lhs,
            right: rhs,
        } = node
        {
            let l = pattern_bindings(pattern, *lhs, b)?;
            let r = pattern_bindings(pattern, *rhs, b)?;
            check(
                l.len() == r.len(),
                ErrorKind::Signature,
                "OR binding set count",
            )?;
            for pair in &l {
                b.work(r.len() as u64)?;
                check(
                    r.contains(pair),
                    ErrorKind::Signature,
                    "OR names/output registers must agree",
                )?;
            }
            b.release(l);
            b.release(r);
        }
        b.release(left);
    }
    Ok(())
}
pub(super) fn pattern_bindings(
    pattern: &PatternDesc,
    root: u32,
    b: &mut Budget,
) -> Result<Vec<(SymbolId, Reg)>, Error> {
    // A DAG can expand exponentially. Bound both the explicit walk and its output.
    let limit = pattern.nodes.len();
    let mut stack = b.vec::<u32>(limit)?;
    stack.push(root);
    let mut out = b.vec::<(SymbolId, Reg)>(limit)?;
    while let Some(id) = stack.pop() {
        b.work(1)?;
        match &pattern.nodes[id as usize] {
            PatternNode::Bind { name, output } => {
                check(
                    out.len() < limit,
                    ErrorKind::Limit,
                    "pattern binding expansion",
                )?;
                out.push((*name, *output));
            }
            PatternNode::Tuple { items, rest } | PatternNode::Array { items, rest } => {
                b.work(items.len() as u64 + u64::from(rest.is_some()))?;
                check(
                    stack.len() + items.len() + usize::from(rest.is_some()) <= limit,
                    ErrorKind::Limit,
                    "pattern work stack",
                )?;
                stack.extend(items);
                if let Some(r) = rest {
                    stack.push(*r);
                }
            }
            PatternNode::Object { fields, rest } => {
                b.work(fields.len() as u64 + u64::from(rest.is_some()))?;
                check(
                    stack.len() + fields.len() + usize::from(rest.is_some()) <= limit,
                    ErrorKind::Limit,
                    "pattern work stack",
                )?;
                stack.extend(fields.iter().map(|f| f.node));
                if let Some(r) = rest {
                    stack.push(*r);
                }
            }
            PatternNode::Is { node, .. } => {
                check(stack.len() < limit, ErrorKind::Limit, "pattern work stack")?;
                stack.push(*node);
            }
            PatternNode::Or { left, .. } => {
                check(stack.len() < limit, ErrorKind::Limit, "pattern work stack")?;
                stack.push(*left);
            }
            _ => {}
        }
    }
    b.release(stack);
    Ok(out)
}
