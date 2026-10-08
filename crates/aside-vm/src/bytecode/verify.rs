use super::*;
use std::collections::{BinaryHeap, hash_map::RandomState};
use std::fmt::Write;
use std::hash::BuildHasher;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackageStats {
    pub encoded_bytes: usize,
    /// Includes inline package data and retained Vec/String capacity, not len alone.
    pub owned_capacity_bytes: usize,
    pub instruction_bytes: usize,
    pub records: u64,
    pub declared_registers: u64,
    pub validation_work: u64,
    pub validation_scratch_peak: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlerKind {
    Catch,
    Finally,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandlerTarget {
    pub region: RegionId,
    pub kind: HandlerKind,
    pub pc: Pc,
    pub exception: Option<Reg>,
}
/// Saved normal exit destination; this is query metadata, not a wire record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupDestination {
    Return,
    Leave(Pc),
}
/// Construction is restricted to the verifier. Accessors only borrow immutable data.
#[derive(Debug)]
pub struct VerifiedProgram {
    package: BytecodePackage,
    stats: PackageStats,
    limits: Limits,
}
impl VerifiedProgram {
    pub fn package(&self) -> &BytecodePackage {
        &self.package
    }
    pub fn stats(&self) -> PackageStats {
        self.stats
    }
    pub fn source_at(&self, function: FunctionId, pc: Pc) -> Option<SourceSpan> {
        let f = self.package.functions.get(function.index())?;
        f.source_map
            .binary_search_by_key(&pc, |m| m.pc)
            .ok()
            .map(|i| f.source_map[i].span)
    }
    pub fn decode(input: &[u8], limits: Limits) -> Result<Self, Error> {
        verify(decode(input, limits)?, limits)
    }
    /// Execution loading includes the actual dispatch/native support check.
    pub fn load(input: &[u8], limits: Limits, support: &RuntimeSupport) -> Result<Self, Error> {
        let program = Self::decode(input, limits)?;
        program.require_runtime(support)?;
        Ok(program)
    }
    /// Routes from the instruction PC that starts this unwind (including the
    /// Rethrow instruction, independently of the envelope's original source). Catch faults
    /// bypass that catch; finally faults bypass the same finally. After cleanup,
    /// `skip` identifies the consumed lexical subtree in the saved unwind
    /// record. Subsequent targets are strict ancestors; inner layers never re-enter.
    pub fn exception_handler(
        &self,
        function: FunctionId,
        pc: Pc,
        skip: Option<RegionId>,
    ) -> Result<Option<HandlerTarget>, Error> {
        let f = self.unwind_function(function, pc, skip)?;
        Ok(exception_target(f, pc, skip))
    }
    /// Selects the next finally for a saved Return/Leave exit. Keep original_pc
    /// and destination fixed while finally bodies execute; after EndFinally,
    /// pass its region as consumed. A Leave remaining in its source's enclosing
    /// protected/catch block preserves that block, including an outer catch.
    /// Finally blocks never select themselves. No allocation or execution occurs.
    pub fn cleanup_handler(
        &self,
        function: FunctionId,
        original_pc: Pc,
        destination: CleanupDestination,
        consumed: Option<RegionId>,
    ) -> Result<Option<HandlerTarget>, Error> {
        let f = self.unwind_function(function, original_pc, consumed)?;
        if let CleanupDestination::Leave(target) = destination {
            check(
                target.index() < f.code.len(),
                ErrorKind::Index,
                "cleanup target PC",
            )?;
        }
        Ok(unwind_target(f, original_pc, consumed, Some(destination)))
    }
    fn unwind_function(
        &self,
        function: FunctionId,
        pc: Pc,
        skip: Option<RegionId>,
    ) -> Result<&FunctionDef, Error> {
        let f = get(&self.package.functions, function.0, "exception function")?;
        check(
            pc.index() < f.code.len(),
            ErrorKind::Index,
            "exception source PC",
        )?;
        if let Some(id) = skip {
            let consumed = get(&f.regions, id.0, "consumed exception region")?;
            check(
                ranges(consumed).any(|range| range.contains(pc)),
                ErrorKind::Region,
                "unwind cursor does not enclose original fault",
            )?;
        }
        Ok(f)
    }
    /// A dispatcher must check its actual implemented opcodes and native registry.
    /// This includes unreachable code: unsupported instructions are never no-ops.
    pub fn require_runtime(&self, support: &RuntimeSupport) -> Result<(), Error> {
        let p = &self.package;
        let mut b = Budget::new(self.limits, self.stats.owned_capacity_bytes);
        for c in &p.capabilities {
            if !contains_text(&support.capabilities, &p.symbols[c.index()], &mut b)? {
                return Err(Error::new(
                    ErrorKind::Capability,
                    format!("missing capability {}", p.symbols[c.index()]),
                ));
            }
        }
        for native in &p.natives {
            let name = &p.symbols[native.symbol.index()];
            let mut registered = None;
            for n in &support.natives {
                b.work(1)?;
                if b.text_equal(&n.symbol, name)? && n.abi_version == native.abi_version {
                    // Parameters contain fixed-size fields; the rest and tail are
                    // fixed-size optional records. Charge before derived equality.
                    let signature = &p.signatures[native.signature.index()];
                    b.work(signature.parameters.len() as u64 + 1)?;
                    if n.signature == *signature {
                        registered = Some(n);
                        break;
                    }
                }
            }
            let Some(registered) = registered else {
                return Err(Error::new(
                    ErrorKind::Capability,
                    format!(
                        "missing native {name}@{} with exact signature",
                        native.abi_version
                    ),
                ));
            };
            b.work(registered.required_capabilities.len() as u64)?;
            for cap in &registered.required_capabilities {
                check(
                    contains_text(&support.capabilities, cap, &mut b)?,
                    ErrorKind::Capability,
                    "trusted native capability is not authorized",
                )?;
                check(
                    contains_symbol_text(p, &native.capabilities, cap, &mut b)?
                        && contains_symbol_text(p, &p.capabilities, cap, &mut b)?,
                    ErrorKind::Capability,
                    "package omitted a capability required by trusted native registration",
                )?;
            }
        }
        // Typed Opcode entries share the frozen u16 domain. Normalize once;
        // dispatch admission does not rescan the host list per instruction.
        let opcode_slots = INSTRUCTION_SPECS.last().expect("opcode table").opcode as usize + 1;
        let mut supported = b.vec::<bool>(opcode_slots)?;
        supported.resize(opcode_slots, false);
        b.work(support.opcodes.len() as u64)?;
        for opcode in &support.opcodes {
            supported[*opcode as usize] = true;
        }
        for (fi, f) in p.functions.iter().enumerate() {
            for (pc, i) in f.code.iter().enumerate() {
                b.work(1)?;
                if !supported[i.opcode as usize] {
                    let opcode = instruction_spec(i.opcode).expect("verified opcode").opcode;
                    return Err(located(
                        p,
                        fi,
                        pc,
                        Error::new(
                            ErrorKind::Capability,
                            format!("runtime does not execute {opcode:?}"),
                        ),
                    ));
                }
            }
        }
        b.release(supported);
        Ok(())
    }
}
pub fn verify(package: BytecodePackage, limits: Limits) -> Result<VerifiedProgram, Error> {
    let stats = validate(&package, limits)?;
    Ok(VerifiedProgram {
        package,
        stats,
        limits,
    })
}
struct Budget {
    limits: Limits,
    work: u64,
    owned: usize,
    scratch: usize,
    peak: usize,
}
impl Budget {
    fn new(limits: Limits, owned: usize) -> Self {
        Self {
            limits,
            work: 0,
            owned,
            scratch: 0,
            peak: 0,
        }
    }
    fn work(&mut self, n: u64) -> Result<(), Error> {
        self.work = self
            .work
            .checked_add(n)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "verification work overflow"))?;
        if self.work > self.limits.max_work {
            return Err(Error::new(ErrorKind::Limit, "verification work limit"));
        }
        Ok(())
    }
    fn text_equal(&mut self, a: &str, b: &str) -> Result<bool, Error> {
        self.work(a.len().max(b.len()) as u64 + 1)?;
        Ok(a == b)
    }
    fn vec<T>(&mut self, n: usize) -> Result<Vec<T>, Error> {
        self.work(n as u64)?;
        let bytes = n
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(|| Error::new(ErrorKind::Limit, "scratch size"))?;
        if self
            .owned
            .checked_add(self.scratch)
            .and_then(|b| b.checked_add(bytes))
            .is_none_or(|b| b > self.limits.max_allocation_bytes)
        {
            return Err(Error::new(ErrorKind::Limit, "validation scratch limit"));
        }
        let mut v = Vec::new();
        v.try_reserve_exact(n)
            .map_err(|_| Error::new(ErrorKind::Allocation, "validation scratch"))?;
        let bytes = v.capacity() * std::mem::size_of::<T>();
        self.scratch = self
            .scratch
            .checked_add(bytes)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "scratch overflow"))?;
        self.peak = self.peak.max(self.scratch);
        if self.owned + self.scratch > self.limits.max_allocation_bytes {
            return Err(Error::new(ErrorKind::Limit, "validation scratch capacity"));
        }
        Ok(v)
    }
    fn release<T>(&mut self, v: Vec<T>) {
        let bytes = v.capacity() * std::mem::size_of::<T>();
        drop(v);
        self.scratch -= bytes;
    }
}
fn contains_text(items: &[String], text: &str, b: &mut Budget) -> Result<bool, Error> {
    b.work(items.len() as u64)?;
    for item in items {
        if b.text_equal(item, text)? {
            return Ok(true);
        }
    }
    Ok(false)
}
fn contains_symbol_text(
    p: &BytecodePackage,
    items: &[SymbolId],
    text: &str,
    b: &mut Budget,
) -> Result<bool, Error> {
    b.work(items.len() as u64)?;
    for item in items {
        if b.text_equal(&p.symbols[item.index()], text)? {
            return Ok(true);
        }
    }
    Ok(false)
}
fn check(ok: bool, kind: ErrorKind, detail: &str) -> Result<(), Error> {
    if ok {
        Ok(())
    } else {
        Err(Error::new(kind, detail))
    }
}
fn get<'a, T>(table: &'a [T], id: u32, label: &str) -> Result<&'a T, Error> {
    table
        .get(id as usize)
        .ok_or_else(|| Error::new(ErrorKind::Index, label))
}
fn symbol(p: &BytecodePackage, id: SymbolId) -> Result<&str, Error> {
    Ok(get(&p.symbols, id.0, "symbol")?.as_str())
}
fn ty(p: &BytecodePackage, id: TypeId) -> Result<(), Error> {
    get(&p.types, id.0, "type")?;
    Ok(())
}
fn reg(f: &FunctionDef, r: Reg) -> Result<(), Error> {
    check(r.0 < f.register_count, ErrorKind::Index, "register")
}
fn window(f: &FunctionDef, w: RegWindow) -> Result<(), Error> {
    check(
        w.start
            .0
            .checked_add(w.len)
            .is_some_and(|end| end <= f.register_count),
        ErrorKind::Window,
        "register window",
    )
}
fn unique_slice<T>(
    items: &[T],
    key: impl Fn(&T) -> u32,
    b: &mut Budget,
    label: &str,
) -> Result<(), Error> {
    if items.len() < 2 {
        return Ok(());
    }
    let sorted = sorted_keys(items.iter().map(key), 1, b)?;
    b.work(sorted.len() as u64)?;
    for pair in sorted.windows(2) {
        check(pair[0] != pair[1], ErrorKind::Signature, label)?;
    }
    b.release(sorted);
    Ok(())
}

/// Rust 1.96 BinaryHeap keeps the reserved Vec allocation: push sifts upward
/// through at most floor(log2(len)) parents, and into_sorted_vec compares at
/// most twice per downward level. Charge those bounds before each operation.
/// Keys are fixed-size scalars or borrowed strings with a byte-comparison bound.
fn sorted_keys<K: Ord>(
    keys: impl ExactSizeIterator<Item = K>,
    comparison_work: u64,
    b: &mut Budget,
) -> Result<Vec<K>, Error> {
    let mut heap = BinaryHeap::from(b.vec(keys.len())?);
    for key in keys {
        let height = (heap.len() + 1).ilog2() as u64;
        b.work((height + 1) * comparison_work)?;
        heap.push(key);
    }
    let height = heap.len().checked_ilog2().unwrap_or(0) as u64;
    let work = (heap.len() as u64)
        .checked_mul(2 * height + 1)
        .and_then(|work| work.checked_mul(comparison_work))
        .ok_or_else(|| Error::new(ErrorKind::Limit, "sort work overflow"))?;
    b.work(work)?;
    Ok(heap.into_sorted_vec())
}

fn validate_symbols(p: &BytecodePackage, b: &mut Budget) -> Result<(), Error> {
    let hashes = RandomState::new();
    validate_symbols_with_hash(p, b, |text| hashes.hash_one(text))
}

fn validate_symbols_with_hash(
    p: &BytecodePackage,
    b: &mut Budget,
    hash: impl Fn(&str) -> u64,
) -> Result<(), Error> {
    for s in &p.symbols {
        b.work(s.len() as u64 + 1)?;
        check(
            s.len() <= b.limits.max_string_bytes as usize && !s.is_empty() && !s.contains('\0'),
            ErrorKind::Encoding,
            "symbol text",
        )?;
    }
    // Hash bytes are charged once, before the iterator hashes any string.
    // Hashes only partition the sort: equal hashes always undergo exact byte
    // comparison, including a sorted fallback for an adversarial collision group.
    for s in &p.symbols {
        b.work(s.len() as u64 + 1)?;
    }
    let sorted = sorted_keys(
        p.symbols
            .iter()
            .enumerate()
            .map(|(i, s)| (hash(s), i as u32)),
        2,
        b,
    )?;
    b.work(2 * sorted.len() as u64)?;
    let mut start = 0;
    while start < sorted.len() {
        let mut end = start + 1;
        while end < sorted.len() && sorted[end].0 == sorted[start].0 {
            end += 1;
        }
        let group = &sorted[start..end];
        if group.len() > 1 {
            b.work(group.len() as u64)?;
            let comparison_work = group
                .iter()
                .map(|(_, i)| p.symbols[*i as usize].len() as u64 + 1)
                .max()
                .expect("nonempty collision group");
            let texts = sorted_keys(
                group.iter().map(|(_, i)| p.symbols[*i as usize].as_str()),
                comparison_work,
                b,
            )?;
            for pair in texts.windows(2) {
                check(
                    !b.text_equal(pair[0], pair[1])?,
                    ErrorKind::Encoding,
                    "duplicate interned symbol",
                )?;
            }
            b.release(texts);
        }
        start = end;
    }
    b.release(sorted);
    Ok(())
}
fn version(p: &BytecodePackage) -> Result<(), Error> {
    check(
        (p.format_version, p.language_version, p.host_abi_version)
            == (FORMAT_VERSION, LANGUAGE_VERSION, HOST_ABI_VERSION),
        ErrorKind::Version,
        "format/language/host ABI version",
    )
}

pub(crate) fn validate(p: &BytecodePackage, limits: Limits) -> Result<PackageStats, Error> {
    version(p)?;
    let (encoded, owned, records) = super::codec::measure(p, limits)?;
    check(
        records <= limits.max_records as u64 && owned <= limits.max_allocation_bytes,
        ErrorKind::Limit,
        "retained capacity/records",
    )?;
    let mut b = Budget::new(limits, owned);
    b.work(encoded as u64)?;
    validate_symbols(p, &mut b)?;
    for (i, c) in p.constants.iter().enumerate() {
        match c {
            Const::Tuple(ids) => {
                for id in ids {
                    b.work(1)?;
                    check(
                        id.index() < i,
                        ErrorKind::Index,
                        "constant tuple must refer backwards",
                    )?;
                }
            }
            Const::Utf8(s) => check(
                s.len() <= limits.max_string_bytes as usize,
                ErrorKind::Limit,
                "constant UTF-8 bytes",
            )?,
            _ => {}
        }
    }
    for (i, t) in p.types.iter().enumerate() {
        match t {
            TypeDef::Builtin(_) => {}
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
            TypeDef::Function(s) => {
                get(&p.signatures, s.0, "function type signature")?;
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
            if let Some(default) = param.default {
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
                        prev.name == input.name && prev.ty == input.ty && input.default.is_none(),
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
    check_type_cycles(p, &mut b)?;
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
            if let Some(default) = param.default {
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
            get(&m.globals, export.global.0, "export global")?;
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
        check(
            file.text.len() <= limits.max_string_bytes as usize,
            ErrorKind::Limit,
            "source text bytes",
        )?;
    }
    let mut instruction_bytes = 0usize;
    let mut registers = 0u64;
    for (fi, f) in p.functions.iter().enumerate() {
        validate_function(p, fi, f, &mut b).map_err(|mut e| {
            e.function = Some(FunctionId(fi as u32));
            e
        })?;
        instruction_bytes = instruction_bytes
            .checked_add(f.code.len() * 16)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "instruction byte count"))?;
        registers += f.register_count as u64;
    }
    Ok(PackageStats {
        encoded_bytes: encoded,
        owned_capacity_bytes: owned,
        instruction_bytes,
        records,
        declared_registers: registers,
        validation_work: b.work,
        validation_scratch_peak: b.peak,
    })
}

fn check_type_cycles(p: &BytecodePackage, b: &mut Budget) -> Result<(), Error> {
    // Edges between structural type records and signature records. Nominal types
    // are leaves, so recursive classes remain possible without recursive layouts.
    let count = p.types.len() + p.signatures.len();
    let mut states = b.vec::<u8>(count)?;
    states.resize(count, 0);
    let mut stack = b.vec::<(usize, usize)>(count)?;
    for root in 0..count {
        if states[root] != 0 {
            continue;
        }
        states[root] = 1;
        stack.push((root, 0));
        while let Some((node, edge)) = stack.last_mut() {
            b.work(1)?;
            let next = if *node < p.types.len() {
                match &p.types[*node] {
                    TypeDef::Tuple(ids) | TypeDef::Union(ids) => ids.get(*edge).map(|i| i.index()),
                    TypeDef::Function(sig) if *edge == 0 => Some(p.types.len() + sig.index()),
                    _ => None,
                }
            } else {
                let s = &p.signatures[*node - p.types.len()];
                if *edge < s.parameters.len() {
                    Some(s.parameters[*edge].ty.index())
                } else if *edge == s.parameters.len() {
                    Some(s.return_type.index())
                } else if *edge == s.parameters.len() + 1 {
                    s.rest.as_ref().map(|r| r.ty.index())
                } else {
                    None
                }
            };
            *edge += 1;
            if let Some(next) = next {
                check(
                    states[next] != 1,
                    ErrorKind::Signature,
                    "recursive structural type/signature",
                )?;
                if states[next] == 0 {
                    states[next] = 1;
                    stack.push((next, 0));
                }
            } else {
                states[*node] = 2;
                stack.pop();
            }
        }
    }
    b.release(stack);
    b.release(states);
    Ok(())
}
fn validate_function(
    p: &BytecodePackage,
    fi: usize,
    f: &FunctionDef,
    b: &mut Budget,
) -> Result<(), Error> {
    get(&p.modules, f.module.0, "function module")?;
    symbol(p, f.name)?;
    let s = get(&p.signatures, f.signature.0, "function signature")?;
    b.work(s.parameters.len() as u64)?;
    for param in &s.parameters {
        if let Some(default) = param.default {
            let default = &p.functions[default.index()];
            b.work(default.captures.len() as u64)?;
            check(
                default.captures == f.captures && default.module == f.module,
                ErrorKind::Capture,
                "default shares the callee lexical capture layout/module",
            )?;
        }
    }
    check(
        f.register_count <= b.limits.max_registers
            && f.register_count as usize >= s.parameters.len() + usize::from(s.rest.is_some()),
        ErrorKind::Signature,
        "parameter register slots",
    )?;
    check(!f.code.is_empty(), ErrorKind::ControlFlow, "empty function")?;
    for binding in &f.bindings {
        symbol(p, binding.name)?;
        ty(p, binding.ty)?;
        reg(f, binding.storage.reg())?;
        if let BindingInitialization::Parameter(i) = binding.initialization {
            let param = if (i as usize) < s.parameters.len() {
                &s.parameters[i as usize]
            } else {
                s.rest
                    .as_ref()
                    .filter(|_| i as usize == s.parameters.len())
                    .ok_or_else(|| Error::new(ErrorKind::Signature, "parameter binding index"))?
            };
            check(
                binding.name == param.name,
                ErrorKind::Signature,
                "parameter binding name",
            )?;
            check(
                binding.mutable == param.mutable && binding.ty == param.ty,
                ErrorKind::Signature,
                "parameter binding mutability/type",
            )?;
            check(
                binding.storage == BindingStorage::Value(Reg(i)),
                ErrorKind::Signature,
                "parameter binding register",
            )?;
        }
    }
    for descriptor in &f.descriptors {
        validate_descriptor(p, f, descriptor, b)?;
    }
    let mut prev = None;
    for m in &f.source_map {
        check(
            m.pc.index() < f.code.len() && prev.is_none_or(|p| p < m.pc),
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
    validate_regions(f, b)?;
    for (pc, i) in f.code.iter().enumerate() {
        validate_instruction(p, f, pc, i, b).map_err(|e| located(p, fi, pc, e))?;
    }
    Ok(())
}
fn validate_call(
    p: &BytecodePackage,
    f: &FunctionDef,
    c: &CallDesc,
    b: &mut Budget,
) -> Result<(), Error> {
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
fn validate_descriptor(
    p: &BytecodePackage,
    f: &FunctionDef,
    d: &Descriptor,
    b: &mut Budget,
) -> Result<(), Error> {
    match d {
        Descriptor::Global { module, global } => {
            let m = get(&p.modules, module.0, "global module")?;
            get(&m.globals, global.0, "global binding")?;
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
        Descriptor::Class(c) => {
            if let Some(base) = c.base {
                reg(f, base)?;
            }
            unique_slice(&c.fields, |field| field.name.0, b, "duplicate class field")?;
            unique_slice(
                &c.methods,
                |method| method.name.0,
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
                    b.work(5)?; // Equality with the four-byte receiver name.
                    check(
                        !ms.parameters.is_empty() && symbol(p, ms.parameters[0].name)? == "self",
                        ErrorKind::Signature,
                        "instance method explicit self",
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
fn validate_captures(
    p: &BytecodePackage,
    f: &FunctionDef,
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
fn validate_pattern(
    p: &BytecodePackage,
    f: &FunctionDef,
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
fn pattern_bindings(
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

fn ranges(r: &ExceptionRegion) -> impl Iterator<Item = PcRange> + '_ {
    std::iter::once(r.protected)
        .chain(r.catch.as_ref().map(|c| c.range))
        .chain(r.finally)
}
fn validate_regions(f: &FunctionDef, b: &mut Budget) -> Result<(), Error> {
    for (index, r) in f.regions.iter().enumerate() {
        check(
            r.catch.is_some() || r.finally.is_some(),
            ErrorKind::Region,
            "empty exception region",
        )?;
        for range in ranges(r) {
            check(
                range.start < range.end && range.end.index() <= f.code.len(),
                ErrorKind::Region,
                "exception range bounds",
            )?;
        }
        if let Some(c) = &r.catch {
            reg(f, c.exception)?;
        }
        check(
            !r.catch.as_ref().is_some_and(|c| c.range.contains(Pc(0)))
                && !r.finally.is_some_and(|range| range.contains(Pc(0))),
            ErrorKind::Region,
            "function entry inside handler",
        )?;
        let own: [Option<PcRange>; 3] = [
            Some(r.protected),
            r.catch.as_ref().map(|c| c.range),
            r.finally,
        ];
        for i in 0..3 {
            for j in i + 1..3 {
                if let (Some(a), Some(c)) = (own[i], own[j]) {
                    check(
                        a.end <= c.start || c.end <= a.start,
                        ErrorKind::Region,
                        "protected/catch/finally overlap",
                    )?;
                }
            }
        }
        if let Some(cleanup) = r.finally {
            let end = f.code[cleanup.end.index() - 1];
            check(
                end.opcode == Opcode::EndFinally as u16 && end.operands[0] as usize == index,
                ErrorKind::Region,
                "finally must end with matching EndFinally",
            )?;
        }
        for other in &f.regions[..index] {
            for a in ranges(r) {
                for c in ranges(other) {
                    b.work(1)?;
                    check(
                        a != c,
                        ErrorKind::Region,
                        "ambiguous shared exception block",
                    )?;
                    check(
                        a.end <= c.start
                            || c.end <= a.start
                            || (a.start <= c.start && c.end <= a.end)
                            || (c.start <= a.start && a.end <= c.end),
                        ErrorKind::Region,
                        "partially overlapping exception blocks",
                    )?;
                }
            }
            for (inner, outer) in [(r, other), (other, r)] {
                for block in ranges(outer) {
                    b.work(3)?;
                    if ranges(inner).any(|part| block.start <= part.start && part.end <= block.end)
                    {
                        for part in ranges(inner) {
                            b.work(1)?;
                            check(
                                block.start <= part.start && part.end <= block.end,
                                ErrorKind::Region,
                                "region protected/catch/finally must share one lexical parent block",
                            )?;
                        }
                    }
                }
            }
            check(
                r.protected != other.protected,
                ErrorKind::Region,
                "duplicate protected range",
            )?;
        }
    }
    Ok(())
}
fn validate_instruction(
    p: &BytecodePackage,
    f: &FunctionDef,
    pc: usize,
    i: &Instruction,
    b: &mut Budget,
) -> Result<(), Error> {
    b.work(1)?;
    let spec = instruction_spec(i.opcode)
        .ok_or_else(|| Error::new(ErrorKind::Opcode, "unknown opcode"))?;
    check(
        i.flags & !spec.allowed_flags == 0,
        ErrorKind::Flags,
        "undeclared instruction flags",
    )?;
    for (kind, value) in spec.operands.iter().zip(i.operands) {
        use OperandKind::*;
        match kind {
            Unused => check(
                value == 0,
                ErrorKind::Encoding,
                "unused operand must be zero",
            )?,
            ReadReg | WriteReg | ReadWriteReg => reg(f, Reg(value))?,
            Const => {
                get(&p.constants, value, "instruction constant")?;
            }
            Symbol => {
                symbol(p, SymbolId(value))?;
            }
            Type => ty(p, TypeId(value))?,
            Binding | BindingRead | BindingWrite | BindingInit => {
                get(&f.bindings, value, "instruction binding")?;
            }
            Capture => {
                get(&f.captures, value, "instruction capture")?;
            }
            Pc => check((value as usize) < f.code.len(), ErrorKind::Index, "jump PC")?,
            Region => {
                get(&f.regions, value, "instruction region")?;
            }
            Descriptor(kind) => check(
                get(&f.descriptors, value, "instruction descriptor")?.kind() == *kind,
                ErrorKind::Index,
                "descriptor kind mismatch",
            )?,
        }
    }
    spec.visit_registers_budgeted(f, i, |units| b.work(units), |_, _| {})?;
    if spec.may_throw {
        b.work(f.regions.len() as u64)?;
        if let Some(target) = exception_target(f, Pc(pc as u32), None) {
            let r = &f.regions[target.region.index()];
            let range = match target.kind {
                HandlerKind::Catch => r.catch.as_ref().expect("catch target").range,
                HandlerKind::Finally => r.finally.expect("finally target"),
            };
            check(
                !range.contains(Pc(pc as u32)) && range.start == target.pc,
                ErrorKind::ControlFlow,
                "exception edge re-enters its own handler",
            )?;
        }
    }
    if spec.opcode == Opcode::CellNew {
        let binding = &f.bindings[i.operands[0] as usize];
        check(
            matches!(binding.storage, BindingStorage::Cell(_))
                && binding.initialization == BindingInitialization::Explicit,
            ErrorKind::Capture,
            "CellNew requires an explicit cell binding contract",
        )?;
    }
    b.work(f.regions.len() as u64)?;
    let in_finally = f
        .regions
        .iter()
        .any(|r| r.finally.is_some_and(|range| range.contains(Pc(pc as u32))));
    let restricted =
        f.context != Context::Ordinary || in_finally || f.kind == FunctionKind::Generator;
    if spec.opcode == Opcode::Await {
        check(
            !restricted && p.signatures[f.signature.index()].effects.0 & Effects::WAIT != 0,
            ErrorKind::ControlFlow,
            "Await in restricted context/effect",
        )?;
    }
    if spec.opcode == Opcode::Yield {
        check(
            f.kind == FunctionKind::Generator && f.context == Context::Ordinary && !in_finally,
            ErrorKind::ControlFlow,
            "Yield outside generator or in restricted context",
        )?;
    }
    if spec.opcode == Opcode::ThreadSuspend {
        check(
            f.kind == FunctionKind::Plain && !restricted,
            ErrorKind::ControlFlow,
            "ThreadSuspend in restricted context",
        )?;
    }
    if spec.opcode == Opcode::Return {
        check(!in_finally, ErrorKind::ControlFlow, "Return from finally")?;
    }
    if spec.opcode == Opcode::Rethrow {
        b.work(f.regions.len() as u64)?;
        check(
            f.regions.iter().any(|r| {
                r.catch
                    .as_ref()
                    .is_some_and(|c| c.range.contains(Pc(pc as u32)))
            }),
            ErrorKind::ControlFlow,
            "Rethrow outside catch",
        )?;
    }
    if spec.opcode == Opcode::EndFinally {
        let r = &f.regions[i.operands[0] as usize];
        check(
            r.finally.is_some_and(|range| range.end.index() == pc + 1),
            ErrorKind::Region,
            "EndFinally outside matching cleanup terminator",
        )?;
    }
    match spec.flow {
        Flow::Next => {
            check(
                pc + 1 < f.code.len(),
                ErrorKind::ControlFlow,
                "fallthrough past code",
            )?;
            edge(f, pc, pc + 1, false, b)?;
        }
        Flow::Jump => edge(f, pc, i.operands[0] as usize, false, b)?,
        Flow::Leave => {
            edge(f, pc, i.operands[0] as usize, true, b)?;
        }
        Flow::Branch => {
            edge(f, pc, i.operands[1] as usize, false, b)?;
            edge(f, pc, i.operands[2] as usize, false, b)?;
        }
        Flow::Return | Flow::Throw | Flow::EndFinally => {}
    }
    Ok(())
}
fn edge(f: &FunctionDef, from: usize, to: usize, leave: bool, b: &mut Budget) -> Result<(), Error> {
    let from = Pc(from as u32);
    let to = Pc(to as u32);
    let mut exits = false;
    for r in &f.regions {
        b.work(1)?;
        for handler in r
            .catch
            .as_ref()
            .map(|c| c.range)
            .into_iter()
            .chain(r.finally)
        {
            if handler.contains(to) {
                check(
                    handler.contains(from),
                    ErrorKind::ControlFlow,
                    "jump into exception handler",
                )?;
            }
            if handler.contains(from) && !handler.contains(to) {
                check(
                    !r.finally.is_some_and(|range| range == handler) && leave,
                    ErrorKind::ControlFlow,
                    "handler escape requires Leave",
                )?;
                exits = true;
            }
        }
        if r.protected.contains(from) && !r.protected.contains(to) {
            check(
                leave,
                ErrorKind::ControlFlow,
                "protected exit requires Leave",
            )?;
            exits = true;
        }
        if !r.protected.contains(from) && r.protected.contains(to) {
            check(
                to == r.protected.start,
                ErrorKind::ControlFlow,
                "jump into protected interior",
            )?;
        }
    }
    if leave {
        check(
            exits,
            ErrorKind::ControlFlow,
            "Leave must exit a protected/catch region",
        )?;
    }
    Ok(())
}
fn exception_target(f: &FunctionDef, pc: Pc, skip: Option<RegionId>) -> Option<HandlerTarget> {
    unwind_target(f, pc, skip, None)
}
fn strict_region_ancestor(f: &FunctionDef, parent: RegionId, child: RegionId) -> bool {
    parent != child
        && ranges(&f.regions[parent.index()]).any(|block| {
            ranges(&f.regions[child.index()])
                .all(|part| block.start <= part.start && part.end <= block.end)
        })
}
fn protected_or_catch_block(r: &ExceptionRegion, pc: Pc) -> Option<PcRange> {
    if r.protected.contains(pc) {
        Some(r.protected)
    } else {
        r.catch
            .as_ref()
            .filter(|c| c.range.contains(pc))
            .map(|c| c.range)
    }
}
fn unwind_target(
    f: &FunctionDef,
    pc: Pc,
    skip: Option<RegionId>,
    destination: Option<CleanupDestination>,
) -> Option<HandlerTarget> {
    let mut best: Option<(u32, HandlerTarget)> = None;
    for (id, r) in f.regions.iter().enumerate() {
        let region = RegionId(id as u32);
        if let Some(consumed) = skip
            && !strict_region_ancestor(f, region, consumed)
        {
            continue;
        }
        // A finally range is intentionally not a candidate for its own region.
        let Some(range) = protected_or_catch_block(r, pc) else {
            continue;
        };
        if let Some(CleanupDestination::Leave(target)) = destination
            && range.contains(target)
        {
            continue;
        }
        let catch = (destination.is_none() && range == r.protected)
            .then_some(r.catch.as_ref())
            .flatten();
        let target = if let Some(c) = catch {
            Some(HandlerTarget {
                region,
                kind: HandlerKind::Catch,
                pc: c.range.start,
                exception: Some(c.exception),
            })
        } else {
            r.finally.map(|range| HandlerTarget {
                region,
                kind: HandlerKind::Finally,
                pc: range.start,
                exception: None,
            })
        };
        if let Some(target) = target {
            let width = range.end.0 - range.start.0;
            if best.is_none_or(|(old, _)| width < old) {
                best = Some((width, target));
            }
        }
    }
    best.map(|(_, target)| target)
}
fn located(p: &BytecodePackage, fi: usize, pc: usize, mut e: Error) -> Error {
    e.function = Some(FunctionId(fi as u32));
    e.pc = Some(Pc(pc as u32));
    // All callers reach this after the source map's strict PC order is checked.
    let map = &p.functions[fi].source_map;
    e.source = map
        .binary_search_by_key(&Pc(pc as u32), |m| m.pc)
        .ok()
        .map(|i| map[i].span);
    e
}

/// Streaming disassembly to a caller-owned sink; text includes descriptor fields,
/// source byte spans and the exact shared flow/suspension contract.
pub fn disassemble(program: &VerifiedProgram, out: &mut impl Write) -> std::fmt::Result {
    let p = program.package();
    writeln!(
        out,
        "ASIDBC01 format={} language={} abi={} content={} bytes={} capacity={}",
        p.format_version,
        p.language_version,
        p.host_abi_version,
        p.content_version,
        program.stats.encoded_bytes,
        program.stats.owned_capacity_bytes
    )?;
    for (id, module) in p.modules.iter().enumerate() {
        writeln!(
            out,
            "module m{id} {} {module:?}",
            p.symbols[module.path.index()]
        )?;
    }
    for (id, s) in p.signatures.iter().enumerate() {
        writeln!(out, "signature s{id} {s:?}")?;
    }
    for (id, c) in p.constants.iter().enumerate() {
        writeln!(out, "const k{id} {c:?}")?;
    }
    for (id, n) in p.natives.iter().enumerate() {
        writeln!(
            out,
            "native n{id} {}@{} {n:?}",
            p.symbols[n.symbol.index()],
            n.abi_version
        )?;
    }
    for (id, f) in p.functions.iter().enumerate() {
        writeln!(
            out,
            "function f{id} {} {:?} {:?} s{} regs={} captures={:?}",
            p.symbols[f.name.index()],
            f.kind,
            f.context,
            f.signature.0,
            f.register_count,
            f.captures
        )?;
        for (d, desc) in f.descriptors.iter().enumerate() {
            writeln!(out, "  descriptor d{d} {desc:?}")?;
        }
        for (r, region) in f.regions.iter().enumerate() {
            writeln!(out, "  region e{r} {region:?}")?;
        }
        for (pc, i) in f.code.iter().enumerate() {
            let spec = instruction_spec(i.opcode).expect("verified opcode");
            write!(out, "  {pc:04} {:04x} {}", i.opcode, spec.mnemonic)?;
            for (kind, operand) in spec.operands.iter().zip(i.operands) {
                if *kind != OperandKind::Unused {
                    write!(out, " {kind:?}={operand}")?;
                }
            }
            write!(
                out,
                " ; {:?} throw={} suspend={:?}",
                spec.flow, spec.may_throw, spec.suspension
            )?;
            if let Some(span) = program.source_at(FunctionId(id as u32), Pc(pc as u32)) {
                write!(
                    out,
                    " {}:{}..{}",
                    p.symbols[p.sources[span.file.index()].path.index()],
                    span.start,
                    span.end
                )?;
            }
            writeln!(out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod bounded_index_tests {
    use super::*;

    #[test]
    fn symbol_hash_collisions_keep_exact_unique_checks_and_bounded_scratch() {
        let mut p = BytecodePackage {
            symbols: (0..10_000).map(|i| format!("s{i:04x}")).collect(),
            ..BytecodePackage::default()
        };
        let limits = Limits::default();
        let (_, owned, _) = super::super::codec::measure(&p, limits).unwrap();
        let mut b = Budget::new(limits, owned);
        let allocation = allocation_counter::measure(|| {
            // Force every distinct symbol into one bucket. Equality stays exact,
            // the fallback remains sorted, and no probabilistic assumption is needed.
            validate_symbols_with_hash(&p, &mut b, |_| 0).unwrap();
        });
        assert_eq!(allocation.bytes_current, 0);
        assert_eq!(allocation.bytes_max as usize, b.peak);
        assert_eq!(b.scratch, 0);
        assert_eq!(
            b.peak,
            p.symbols.len() * (std::mem::size_of::<(u64, u32)>() + std::mem::size_of::<&str>())
        );
        assert!(b.work < limits.max_work);

        p.symbols.push(p.symbols[0].clone());
        let mut b = Budget::new(limits, owned);
        assert_eq!(
            validate_symbols_with_hash(&p, &mut b, |_| 0)
                .unwrap_err()
                .kind,
            ErrorKind::Encoding
        );
        let mut b = Budget::new(
            Limits {
                max_work: 100,
                ..limits
            },
            owned,
        );
        assert_eq!(
            validate_symbols_with_hash(&p, &mut b, |_| 0)
                .unwrap_err()
                .kind,
            ErrorKind::Limit
        );
        assert_eq!(
            b.peak, 0,
            "work limit is checked before reserving the index"
        );
    }

    #[test]
    fn collision_fallback_capacity_is_checked_before_its_reservation() {
        let p = BytecodePackage {
            symbols: (0..1000).map(|i| format!("s{i:04x}")).collect(),
            ..BytecodePackage::default()
        };
        let limits = Limits::default();
        let (_, owned, _) = super::super::codec::measure(&p, limits).unwrap();
        let index_bytes = p.symbols.len() * std::mem::size_of::<(u64, u32)>();
        let group_bytes = p.symbols.len() * std::mem::size_of::<&str>();
        let mut b = Budget::new(
            Limits {
                max_allocation_bytes: owned + index_bytes + group_bytes - 1,
                ..limits
            },
            owned,
        );
        assert_eq!(
            validate_symbols_with_hash(&p, &mut b, |_| 0)
                .unwrap_err()
                .kind,
            ErrorKind::Limit
        );
        assert_eq!(b.peak, index_bytes);
    }
}
