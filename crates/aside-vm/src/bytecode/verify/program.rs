//! Package metadata and capability validation.
use super::*;

pub(crate) fn validate(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
    limits: Limits,
) -> Result<PackageStats, Error> {
    validate_program(p, limits).map(|(stats, _)| stats)
}
pub(super) fn validate_program(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
    limits: Limits,
) -> Result<(PackageStats, DerivedLayouts), Error> {
    version(p)?;
    if let Some(store) = p.functions.first().and_then(|f| f.code.runtime_store()) {
        let mut start = 0u32;
        for f in &p.functions {
            check(
                f.code
                    .runtime_store()
                    .is_some_and(|other| std::sync::Arc::ptr_eq(store, other))
                    && f.code.runtime_start() == start,
                ErrorKind::Encoding,
                "shared function code layout",
            )?;
            start = start
                .checked_add(
                    u32::try_from(f.code.end_pc())
                        .map_err(|_| Error::new(ErrorKind::Limit, "code extent"))?,
                )
                .ok_or_else(|| Error::new(ErrorKind::Limit, "code extent"))?;
        }
        check(
            start == store.words_len(),
            ErrorKind::Encoding,
            "shared function code extent",
        )?;
    }
    let (encoded, owned, records) = super::super::codec::measure(p, limits)?;
    check(
        records <= limits.max_records as u64 && owned <= limits.max_allocation_bytes,
        ErrorKind::Limit,
        "retained capacity/records",
    )?;
    let hot = p
        .functions
        .first()
        .and_then(|f| f.code.runtime_store())
        .map_or(0, |store| {
            let s = store.cache_stats();
            s.hot_limit_bytes + s.construction_scratch_bytes
        });
    let mut b = Budget::new(
        limits,
        owned
            .checked_add(hot)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "execution capacity"))?,
    );
    check(
        b.owned <= limits.max_allocation_bytes,
        ErrorKind::Limit,
        "execution cache capacity",
    )?;
    b.work(encoded as u64)?;
    validate_symbols(p, &mut b)?;
    for (i, c) in p.constants.iter().enumerate() {
        if let Const::Text { offset, len } = c {
            let pool = p.text_pool.ok_or_else(|| {
                Error::new(
                    ErrorKind::Encoding,
                    "external text constant has no pool descriptor",
                )
            })?;
            check(
                offset
                    .checked_add(u64::from(*len))
                    .is_some_and(|end| end <= pool.byte_len),
                ErrorKind::Index,
                "text constant outside its pool",
            )?;
        }
        if let Const::Tuple(ids) = c {
            for id in ids {
                b.work(1)?;
                check(
                    id.index() < i,
                    ErrorKind::Index,
                    "constant tuple must refer backwards",
                )?;
            }
        }
    }
    for (i, t) in p.types.iter().enumerate() {
        match t {
            TypeDef::Builtin(_) | TypeDef::StringLiteral(_) => {}
            TypeDef::Array(item) => {
                b.work(1)?;
                check(item.index() < i, ErrorKind::Index, "array element type must refer backwards")?;
            }
            TypeDef::Record(fields) => {
                unique_slice(
                    fields,
                    |field| field.name.0,
                    &mut b,
                    "duplicate record field",
                )?;
                for field in fields {
                    b.work(1)?;
                    symbol(p, field.name)?;
                    check(
                        field.ty.index() < i,
                        ErrorKind::Index,
                        "record field type must refer backwards",
                    )?;
                }
            }
            TypeDef::Tuple(ids) | TypeDef::Union(ids) => {
                if matches!(t, TypeDef::Union(_)) {
                    check(
                        ids.len() >= 2,
                        ErrorKind::Signature,
                        "union needs two types",
                    )?;
                    unique_slice(ids, |x| x.0, &mut b, "duplicate union member")?;
                }
                for id in ids {
                    b.work(1)?;
                    check(
                        id.index() < i,
                        ErrorKind::Index,
                        "structural type must refer backwards",
                    )?;
                }
            }
            TypeDef::Function {
                parameters,
                return_type,
            } => {
                for id in parameters.iter().chain(std::iter::once(return_type)) {
                    b.work(1)?;
                    check(
                        id.index() < i,
                        ErrorKind::Index,
                        "function type must refer backwards",
                    )?;
                }
            }
            TypeDef::Named {
                module,
                name,
                version,
            } => {
                get(&p.modules, module.0, "nominal module")?;
                symbol(p, *name)?;
                check(
                    *version == p.content_version,
                    ErrorKind::Version,
                    "nominal type version",
                )?;
            }
        }
    }
    for (si, s) in p.signatures.iter().enumerate() {
        check(
            s.effects.0 & !Effects::ALL.0 == 0,
            ErrorKind::Signature,
            "unknown effects",
        )?;
        ty(p, s.return_type)?;
        if let Some(tail) = &s.property_bag_tail {
            check(
                (tail.target_parameter as usize) < s.parameters.len(),
                ErrorKind::Signature,
                "property bag target parameter",
            )?;
            if let Some(cap) = tail.capability {
                symbol(p, cap)?;
                b.work(p.capabilities.len() as u64)?;
                check(
                    p.capabilities.contains(&cap),
                    ErrorKind::Capability,
                    "property bag capability",
                )?;
            }
        }
        unique_slice(&s.parameters, |v| v.name.0, &mut b, "duplicate parameter")?;
        for (i, param) in s.parameters.iter().enumerate() {
            symbol(p, param.name)?;
            ty(p, param.ty)?;
            if let Some(ParameterDefault::Constant(id)) = param.default {
                get(&p.constants, id.0, "parameter default constant")?;
            }
            if let Some(ParameterDefault::Function(default)) = param.default {
                let f = get(&p.functions, default.0, "default expression function")?;
                let ds = get(&p.signatures, f.signature.0, "default signature")?;
                check(
                    f.kind == FunctionKind::Plain && f.context == Context::NoWait,
                    ErrorKind::Signature,
                    "default must be Plain/NoWait",
                )?;
                check(
                    ds.parameters.len() == i
                        && ds.rest.is_none()
                        && ds.property_bag_tail.is_none()
                        && ds.return_type == param.ty
                        && ds.effects.0 & Effects::WAIT == 0,
                    ErrorKind::Signature,
                    "default inputs/return/effects",
                )?;
                for (prev, input) in s.parameters[..i].iter().zip(&ds.parameters) {
                    b.work(1)?;
                    check(
                        prev.name == input.name
                            && prev.ty == input.ty
                            && prev.mutable == input.mutable
                            && input.default.is_none(),
                        ErrorKind::Signature,
                        "default may only receive preceding parameters",
                    )?;
                }
                check(
                    f.signature.index() != si,
                    ErrorKind::Signature,
                    "recursive default signature",
                )?;
            }
        }
        if let Some(rest) = &s.rest {
            symbol(p, rest.name)?;
            ty(p, rest.ty)?;
            b.work(s.parameters.len() as u64)?;
            check(
                rest.default.is_none() && !s.parameters.iter().any(|p| p.name == rest.name),
                ErrorKind::Signature,
                "rest name/default conflict",
            )?;
        }
    }
    unique_slice(
        &p.natives,
        |n| n.symbol.0,
        &mut b,
        "duplicate native symbol",
    )?;
    unique_slice(&p.capabilities, |c| c.0, &mut b, "duplicate capability")?;
    for cap in &p.capabilities {
        symbol(p, *cap)?;
    }
    for n in &p.natives {
        symbol(p, n.symbol)?;
        get(&p.signatures, n.signature.0, "native signature")?;
        check(
            n.abi_version != 0,
            ErrorKind::Version,
            "native ABI version zero",
        )?;
        let parameters = &p.signatures[n.signature.index()].parameters;
        b.work(parameters.len() as u64)?;
        for param in parameters {
            if let Some(ParameterDefault::Function(default)) = param.default {
                check(
                    p.functions[default.index()].captures.is_empty(),
                    ErrorKind::Signature,
                    "native default has no script closure environment",
                )?;
            }
        }
        unique_slice(
            &n.capabilities,
            |c| c.0,
            &mut b,
            "duplicate native capability",
        )?;
        for cap in &n.capabilities {
            b.work(p.capabilities.len() as u64)?;
            symbol(p, *cap)?;
            check(
                p.capabilities.contains(cap),
                ErrorKind::Capability,
                "native capability missing from package requirements",
            )?;
        }
    }
    unique_slice(&p.modules, |m| m.path.0, &mut b, "duplicate module path")?;
    for (mi, m) in p.modules.iter().enumerate() {
        let path = symbol(p, m.path)?;
        b.work(path.len() as u64)?;
        check(
            path.split("::")
                .all(|s| !s.is_empty() && s.chars().all(|c| c == '_' || c.is_alphanumeric())),
            ErrorKind::Encoding,
            "normalized module path",
        )?;
        unique_slice(&m.globals, |g| g.name.0, &mut b, "duplicate global name")?;
        unique_slice(&m.imports, |i| i.name.0, &mut b, "duplicate import binding")?;
        unique_slice(&m.exports, |e| e.name.0, &mut b, "duplicate export name")?;
        for global in &m.globals {
            symbol(p, global.name)?;
            ty(p, global.ty)?;
            if let Some(id) = global.linked_function {
                let f = get(&p.functions, id.0, "linked global function")?;
                check(
                    f.module.index() == mi && f.captures.is_empty() && !global.mutable,
                    ErrorKind::Signature,
                    "link-time function binding",
                )?;
            }
        }
        for export in &m.exports {
            symbol(p, export.name)?;
            check(
                export.global.index() < m.globals.len() + m.imports.len(),
                ErrorKind::Index,
                "export binding",
            )?;
        }
        for import in &m.imports {
            symbol(p, import.name)?;
            symbol(p, import.export)?;
            let from = get(&p.modules, import.module.0, "import module")?;
            b.work((from.exports.len() + m.globals.len()) as u64)?;
            check(
                import.content_version == p.content_version,
                ErrorKind::Version,
                "fixed import content version",
            )?;
            check(
                from.exports.iter().any(|e| e.name == import.export),
                ErrorKind::Index,
                "import missing export",
            )?;
            check(
                !m.globals.iter().any(|g| g.name == import.name),
                ErrorKind::Signature,
                "import/global collision",
            )?;
        }
        if let Some(id) = m.initializer {
            let f = get(&p.functions, id.0, "module initializer")?;
            let s = get(&p.signatures, f.signature.0, "initializer signature")?;
            check(
                f.module.index() == mi
                    && f.kind == FunctionKind::Plain
                    && f.context == Context::Init
                    && f.captures.is_empty()
                    && s.parameters.is_empty()
                    && s.rest.is_none()
                    && s.effects.0 & Effects::WAIT == 0,
                ErrorKind::Signature,
                "module initialization contract",
            )?;
        }
    }
    for file in &p.sources {
        symbol(p, file.path)?;
    }
    let parameter_layouts = ParameterLayouts::build(p, &mut b)?;
    let mut instruction_bytes = p
        .functions
        .first()
        .and_then(|f| f.code.runtime_store())
        .map_or(0, |store| store.cache_stats().cold_capacity_bytes);
    if p.functions
        .first()
        .is_some_and(|f| f.code.runtime_store().is_some())
    {
        instruction_bytes += p.functions.len() * size_of::<CompactCode>();
    }
    let mut registers = 0u64;
    let code_count = p.functions.iter().try_fold(0usize, |sum, f| {
        sum.checked_add(f.code.len())
            .ok_or_else(|| Error::new(ErrorKind::Limit, "instruction count"))
    })?;
    let mut uses = InstructionUses {
        opcodes: b.vec::<OpcodeUse>(code_count.min(INSTRUCTION_SPECS.len()))?,
        seen: [false; 256],
        members: b.vec::<u32>(p.symbols.len().div_ceil(32))?,
    };
    uses.members.resize(p.symbols.len().div_ceil(32), 0);
    let mut match_functions = text_matches::MatchFunctions::new();
    for (fi, f) in p.functions.iter().enumerate() {
        let function = FunctionId(fi as u32);
        let literal_checks = validate_function(p, fi, f, &parameter_layouts, &mut uses, &mut b)
            .map_err(|mut e| {
                e.function = Some(function);
                e
            })?;
        match_functions.note(function, literal_checks);
        instruction_bytes = instruction_bytes
            .checked_add(f.code.capacity_bytes())
            .ok_or_else(|| Error::new(ErrorKind::Limit, "instruction byte count"))?;
        registers += f.register_count as u64;
    }
    let InstructionUses {
        opcodes: opcode_uses,
        members: member_bits,
        ..
    } = uses;
    b.work(member_bits.len() as u64 * 2)?;
    let count = member_bits
        .iter()
        .map(|bits| bits.count_ones() as usize)
        .sum();
    let mut member_symbols = b.vec::<SymbolId>(count)?;
    for (word, &bits) in member_bits.iter().enumerate() {
        let mut bits = bits;
        while bits != 0 {
            let bit = bits.trailing_zeros() as usize;
            member_symbols.push(SymbolId((word * 32 + bit) as u32));
            bits &= bits - 1;
        }
    }
    b.release(member_bits);
    let native_parameter_cells = native_parameter_cells(p, &parameter_layouts, &mut b)?;
    let script_parameter_cells = parameter_layouts.script_cells(&mut b)?;
    let module_bindings = module_bindings(p, &mut b)?;
    parameter_layouts.release(&mut b);
    let native_calls = native_calls::build(p, &native_parameter_cells, &mut b)?;
    let compact_derived = compact_derived::build(p, &mut b)?;
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    let scalar_suffixes = scalar_suffixes::build(p, &mut b)?;
    let scalar_entries = scalar_entry::build(p, &mut b)?;
    #[cfg(not(any(test, feature = "vm-validation-oracle")))]
    let scalar_suffix_bytes = 0;
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    let scalar_suffix_bytes = scalar_suffixes.capacity_bytes();
    let derived_capacity_bytes = compact_derived.capacity() * size_of::<u32>()
        + scalar_suffix_bytes
        + scalar_entries.capacity_bytes()
        + native_calls.capacity_bytes()
        + native_parameter_cells.capacity() * std::mem::size_of::<(u32, u32)>()
        + script_parameter_cells.capacity() * std::mem::size_of::<(u32, u32)>()
        + module_bindings.offsets.capacity() * std::mem::size_of::<u32>()
        + module_bindings.values.capacity() * std::mem::size_of::<GlobalBinding>()
        + opcode_uses.capacity() * size_of::<OpcodeUse>()
        + member_symbols.capacity() * size_of::<SymbolId>();
    Ok((
        PackageStats {
            encoded_bytes: encoded,
            owned_capacity_bytes: owned + derived_capacity_bytes,
            derived_capacity_bytes,
            instruction_bytes,
            records,
            declared_registers: registers,
            validation_work: b.work,
            validation_scratch_peak: b.peak.max(if p.sources.is_empty() {
                0
            } else {
                super::super::codec::SOURCE_COMPRESSION_SCRATCH
            }),
        },
        DerivedLayouts {
            native_parameters: native_parameter_cells,
            script_parameters: script_parameter_cells,
            modules: module_bindings,
            opcode_uses,
            member_symbols,
            native_calls,
            compact_derived,
            #[cfg(any(test, feature = "vm-validation-oracle"))]
            scalar_suffixes,
            scalar_entries,
            match_functions,
        },
    ))
}
