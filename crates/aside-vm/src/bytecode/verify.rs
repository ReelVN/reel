use super::code_residency::{CodeStore, CodeStoreBuilder, PAGE_WORDS};
use super::*;
use std::collections::{BinaryHeap, hash_map::RandomState};
use std::fmt::Write;
use std::hash::BuildHasher;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackageStats {
    pub encoded_bytes: usize,
    /// Includes inline package data and all retained package/derived capacities.
    pub owned_capacity_bytes: usize,
    /// Retained verifier-derived metadata, already included in owned_capacity_bytes.
    pub derived_capacity_bytes: usize,
    pub instruction_bytes: usize,
    pub records: u64,
    pub declared_registers: u64,
    pub validation_work: u64,
    pub validation_scratch_peak: usize,
}
/// Admission cost for an explicit clone into the compiler's ordinal-PC model.
/// Metadata spare capacities form a conservative retained bound; PC maps are
/// temporary and reused one function at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditableCloneCost {
    pub retained_capacity_bytes: usize,
    pub scratch_peak_bytes: usize,
    pub work_units: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HandlerKind {
    Catch,
    Finally,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug)]
pub struct VerifiedProgram(std::sync::Arc<VerifiedData>, Option<TextPool>);

#[derive(Debug)]
struct VerifiedData {
    identity: [u8; 32],
    package: RuntimePackage,
    stats: PackageStats,
    limits: Limits,
    /// Sorted (signature, parameter) pairs; only Cell parameters are retained.
    native_parameter_cells: Vec<(u32, u32)>,
    module_bindings: ModuleBindings,
    opcode_uses: Vec<OpcodeUse>,
    member_symbols: Vec<SymbolId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalBinding {
    pub offset: u32,
    pub ty: TypeId,
    pub mutable: bool,
    pub imported: bool,
}
#[derive(Debug)]
struct ModuleBindings {
    offsets: Vec<u32>,
    values: Vec<GlobalBinding>,
}
#[derive(Clone, Copy, Debug)]
struct OpcodeUse {
    opcode: u16,
    function: FunctionId,
    pc: Pc,
}
struct InstructionUses {
    opcodes: Vec<OpcodeUse>,
    seen: [bool; 256],
    members: Vec<u32>,
}
struct DerivedLayouts {
    native_parameters: Vec<(u32, u32)>,
    modules: ModuleBindings,
    opcode_uses: Vec<OpcodeUse>,
    member_symbols: Vec<SymbolId>,
}
impl VerifiedProgram {
    pub(crate) fn shared_header_bytes(&self) -> usize {
        std::mem::size_of::<VerifiedData>() + 2 * std::mem::size_of::<usize>()
    }
    pub(crate) fn snapshot_identity(&self) -> [u8; 32] {
        self.0.identity
    }
    /// Imports resolve to their original cell; access costs are independent of
    /// the number of source modules or the length of a reexport chain.
    pub fn global_binding(
        &self,
        module: ModuleId,
        global: GlobalId,
    ) -> Result<GlobalBinding, Error> {
        let start = *get(&self.0.module_bindings.offsets, module.0, "global module")?;
        let end = *get(
            &self.0.module_bindings.offsets,
            module.0 + 1,
            "global module",
        )?;
        check(global.0 < end - start, ErrorKind::Index, "global binding")?;
        Ok(self.0.module_bindings.values[(start + global.0) as usize])
    }
    pub fn package(&self) -> &RuntimePackage {
        &self.0.package
    }
    /// Every statically named member, including operands in unreachable code.
    /// VM loading interns these names without decoding execution pages again.
    pub(crate) fn member_symbols(&self) -> &[SymbolId] {
        &self.0.member_symbols
    }
    /// Copy this immutable package into the compiler's editable instruction model.
    /// Execution and serialization borrow compact storage without this expansion.
    pub fn to_unpacked(&self) -> Result<BytecodePackage, Error> {
        self.package().to_unpacked()
    }
    /// Query editable-clone capacity and work before expanding execution words.
    pub fn editable_clone_cost(&self) -> Result<EditableCloneCost, Error> {
        let p = self.package();
        let overflow = || Error::new(ErrorKind::Limit, "editable clone cost");
        let shared = p
            .functions
            .first()
            .and_then(|f| f.code.runtime_store())
            .map_or(0, |store| store.cache_stats().cold_capacity_bytes);
        let mut retained = self.0.stats.owned_capacity_bytes - self.0.stats.derived_capacity_bytes;
        retained = retained.checked_sub(shared).ok_or_else(overflow)?;
        retained = retained
            .checked_sub(p.functions.capacity() * size_of::<RuntimeFunction>())
            .ok_or_else(overflow)?;
        retained = retained
            .checked_add(p.functions.len() * size_of::<FunctionDef>())
            .ok_or_else(overflow)?;
        let mut scratch = 0;
        let mut work = self
            .0
            .stats
            .records
            .checked_add(self.0.stats.encoded_bytes as u64)
            .ok_or_else(overflow)?;
        for f in &p.functions {
            retained = retained
                .checked_sub(f.code.capacity_bytes())
                .and_then(|n| n.checked_add(f.code.len().checked_mul(size_of::<Instruction>())?))
                .ok_or_else(overflow)?;
            scratch = scratch.max((f.code.len() + 1).checked_mul(4).ok_or_else(overflow)?);
            let search = 1 + u64::from(usize::BITS - f.code.len().leading_zeros());
            let queries = (f.code.len() as u64)
                .checked_mul(3)
                .and_then(|n| n.checked_add(f.source_map.len() as u64))
                .and_then(|n| n.checked_add(f.regions.len() as u64 * 6))
                .ok_or_else(overflow)?;
            work = work
                .checked_add(queries.checked_mul(search).ok_or_else(overflow)?)
                .and_then(|n| n.checked_add(f.code.len() as u64))
                .ok_or_else(overflow)?;
        }
        Ok(EditableCloneCost {
            retained_capacity_bytes: retained,
            scratch_peak_bytes: scratch,
            work_units: work,
        })
    }
    /// Inspect shared cold execution storage and the bounded lazy page cache.
    pub fn code_cache_stats(&self) -> Option<super::code_residency::CodeCacheStats> {
        self.package()
            .functions
            .first()
            .and_then(|f| f.code.store())
            .map(|store| store.cache_stats())
    }
    /// Capacity reserved by execution admission, including pages not yet loaded.
    pub fn execution_capacity_bytes(&self) -> usize {
        let actual = self.0.stats.owned_capacity_bytes
            + self.1.as_ref().map_or(0, |pool| pool.capacity_bytes());
        self.code_cache_stats().map_or(actual, |code| {
            actual + code.hot_limit_bytes + code.construction_scratch_bytes
        })
    }
    pub fn stats(&self) -> PackageStats {
        let mut stats = self.0.stats;
        if let Some(code) = self.code_cache_stats() {
            stats.owned_capacity_bytes += code.resident_capacity_bytes;
            stats.instruction_bytes += code.resident_capacity_bytes;
        }
        if let Some(pool) = &self.1 {
            stats.owned_capacity_bytes += pool.capacity_bytes();
        }
        stats
    }
    /// Validated canonical native storage, derived once from all default prefixes.
    /// Rest and parameters outside every default prefix are ordinary Value slots.
    /// These are binder slots, never a reference-call ABI for author arguments.
    pub fn native_parameter_storage(
        &self,
        native: NativeId,
        parameter: u32,
    ) -> Result<BindingStorage, Error> {
        let native = get(&self.0.package.natives, native.0, "native parameter layout")?;
        let signature = &self.0.package.signatures[native.signature.index()];
        check(
            (parameter as usize)
                < signature.parameters.len() + usize::from(signature.rest.is_some()),
            ErrorKind::Index,
            "native parameter layout index",
        )?;
        Ok(
            if self
                .0
                .native_parameter_cells
                .binary_search(&(native.signature.0, parameter))
                .is_ok()
            {
                BindingStorage::Cell(Reg(parameter))
            } else {
                BindingStorage::Value(Reg(parameter))
            },
        )
    }
    /// False selects the ordinary native binder's no-Cell path. Borrowing these
    /// immutable tables allocates nothing; execution accounts its own query work.
    pub fn native_parameters_need_cells(&self, native: NativeId) -> Result<bool, Error> {
        let native = get(&self.0.package.natives, native.0, "native parameter layout")?;
        Ok(self
            .0
            .native_parameter_cells
            .binary_search_by_key(&native.signature.0, |&(signature, _)| signature)
            .is_ok())
    }
    /// Bound for a lookup in the retained sparse native parameter layout.
    pub(crate) fn native_parameter_lookup_work(&self) -> u64 {
        4 + self
            .0
            .native_parameter_cells
            .len()
            .checked_ilog2()
            .unwrap_or(0) as u64
    }
    pub fn source_at(&self, function: FunctionId, pc: Pc) -> Option<SourceSpan> {
        let f = self.0.package.functions.get(function.index())?;
        if !(pc.index() < f.code.end_pc() && f.code.is_boundary(pc.index())
            || matches!(f.kind, FunctionKind::Native(_)) && pc.0 == 0)
        {
            return None;
        }
        source_span(&f.source_map, pc)
    }
    /// Decode and verify owned program data. Code identity reuses the validated
    /// canonical bytes; the returned program retains no borrow of the input.
    pub fn decode(input: &[u8], limits: Limits) -> Result<Self, Error> {
        verify_runtime_with_identity(decode(input, limits)?, limits, |_| {
            super::codec::decoded_identity(input, limits)
        })
    }
    pub fn decode_with_text_pool(
        input: &[u8],
        limits: Limits,
        pool: TextPool,
    ) -> Result<Self, Error> {
        Self::decode(input, limits)?.bind_text_pool(pool)
    }
    /// Binds the exact backing identified by the package. This validates every
    /// span once; normal string reads do not hash or revalidate the pool.
    pub fn bind_text_pool(mut self, pool: TextPool) -> Result<Self, Error> {
        check(
            self.package().text_pool == Some(pool.descriptor()),
            ErrorKind::Encoding,
            "text pool identity does not match bytecode",
        )?;
        let unbound_capacity =
            self.execution_capacity_bytes() - self.1.as_ref().map_or(0, TextPool::capacity_bytes);
        check(
            unbound_capacity
                .checked_add(pool.capacity_bytes())
                .is_some_and(|n| n <= self.0.limits.max_allocation_bytes),
            ErrorKind::Limit,
            "package/text pool allocation limit",
        )?;
        for constant in &self.package().constants {
            if let Const::Text { offset, len } = constant {
                pool.text(*offset, *len)?;
            }
        }
        self.1 = Some(pool);
        Ok(self)
    }
    pub fn require_text_pool(&self) -> Result<(), Error> {
        check(
            self.package().text_pool.is_none() || self.1.is_some(),
            ErrorKind::Encoding,
            "bytecode requires an external text pool",
        )
    }
    /// Borrow an inline or bound byte/string constant without copying it.
    /// Returns None for other constant kinds or an unbound external text pool.
    pub fn constant_bytes(&self, id: ConstId) -> Option<&[u8]> {
        Some(match self.package().constants.get(id.index())? {
            Const::Utf8(text) => text.as_bytes(),
            Const::Bytes(bytes) => bytes,
            Const::Text { offset, len } => {
                &self.1.as_ref()?.as_bytes()[*offset as usize..*offset as usize + *len as usize]
            }
            _ => return None,
        })
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
    /// A trusted executor queries an already determined exit: PC/cursor checks
    /// do not revalidate arbitrary source-to-target control flow; a verified
    /// instruction or valid executor state must guarantee destination legality.
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
                target.index() < f.code.end_pc() && f.code.is_boundary(target.index()),
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
    ) -> Result<&FunctionDef<impl InstructionSequence>, Error> {
        let f = get(&self.0.package.functions, function.0, "exception function")?;
        check(
            pc.index() < f.code.end_pc() && f.code.is_boundary(pc.index()),
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
        let p = &self.0.package;
        let mut b = Budget::new(self.0.limits, self.execution_capacity_bytes());
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
        for used in &self.0.opcode_uses {
            b.work(1)?;
            if !supported[used.opcode as usize] {
                let opcode = instruction_spec(used.opcode)
                    .expect("verified opcode")
                    .opcode;
                return Err(located(
                    p,
                    used.function.index(),
                    used.pc.index(),
                    Error::new(
                        ErrorKind::Capability,
                        format!("runtime does not execute {opcode:?}"),
                    ),
                ));
            }
        }
        b.release(supported);
        Ok(())
    }
}
/// A package admitted through the single shared verifier. Editable vectors are
/// rebased once; packages decoded as runtime words retain their storage directly.
pub trait VerifiablePackage {
    fn verified(self, limits: Limits) -> Result<VerifiedProgram, Error>;
}
impl VerifiablePackage for BytecodePackage {
    fn verified(self, limits: Limits) -> Result<VerifiedProgram, Error> {
        let (mut stats, mut layouts) = validate_program(&self, limits)?;
        let package = compact_package(self, &mut stats, &mut layouts.opcode_uses, limits)?;
        let identity = super::codec::identity(&package, limits)?;
        Ok(publish(package, stats, layouts, limits, identity))
    }
}
impl VerifiablePackage for RuntimePackage {
    fn verified(self, limits: Limits) -> Result<VerifiedProgram, Error> {
        verify_runtime_with_identity(self, limits, |p| super::codec::identity(p, limits))
    }
}
pub fn verify(package: impl VerifiablePackage, limits: Limits) -> Result<VerifiedProgram, Error> {
    package.verified(limits)
}
fn publish(
    package: RuntimePackage,
    stats: PackageStats,
    layouts: DerivedLayouts,
    limits: Limits,
    identity: [u8; 32],
) -> VerifiedProgram {
    VerifiedProgram(
        std::sync::Arc::new(VerifiedData {
            identity,
            package,
            stats,
            limits,
            native_parameter_cells: layouts.native_parameters,
            module_bindings: layouts.modules,
            opcode_uses: layouts.opcode_uses,
            member_symbols: layouts.member_symbols,
        }),
        None,
    )
}
fn verify_runtime_with_identity(
    package: RuntimePackage,
    limits: Limits,
    identity: impl FnOnce(&RuntimePackage) -> Result<[u8; 32], Error>,
) -> Result<VerifiedProgram, Error> {
    let (mut stats, layouts) = validate_program(&package, limits)?;
    if let Some(store) = package
        .functions
        .first()
        .and_then(|f| f.code.runtime_store())
    {
        stats.validation_scratch_peak = stats
            .validation_scratch_peak
            .max(store.cache_stats().construction_scratch_bytes + package.modules.capacity() * 4);
    }
    let identity = identity(&package)?;
    Ok(publish(package, stats, layouts, limits, identity))
}
fn compact_package(
    p: BytecodePackage,
    stats: &mut PackageStats,
    opcode_uses: &mut [OpcodeUse],
    limits: Limits,
) -> Result<RuntimePackage, Error> {
    let mut budget = Budget::new(limits, stats.owned_capacity_bytes);
    budget.work = stats.validation_work;
    let mut functions = budget.vec::<RuntimeFunction>(p.functions.len())?;
    let mut retained = stats.owned_capacity_bytes;
    let mut builder = CodeStoreBuilder::new();
    let mut ranges = budget.vec::<(u32, u32, u32)>(p.functions.len())?;
    let mut instruction_bytes = 0usize;
    let total_words = p.functions.iter().try_fold(0usize, |n, f| {
        f.code.iter().try_fold(n, |n, i| {
            n.checked_add(1 + CompactCode::shape(&i?)?.3)
                .ok_or_else(|| Error::new(ErrorKind::Limit, "code word count"))
        })
    })?;
    let packed_bound = total_words
        .checked_mul(4)
        .and_then(|n| n.checked_add(total_words / 250 + total_words.div_ceil(PAGE_WORDS) * 128))
        .ok_or_else(|| Error::new(ErrorKind::Limit, "cold code capacity"))?;
    let build_bound = CodeStore::planned_capacity_bytes(
        u32::try_from(total_words).map_err(|_| Error::new(ErrorKind::Limit, "code words"))?,
        packed_bound,
        total_words.div_ceil(PAGE_WORDS),
    )? + builder.capacity_bytes();
    check(
        retained
            .checked_add(budget.scratch)
            .and_then(|n| n.checked_add(build_bound))
            .is_some_and(|n| n <= limits.max_allocation_bytes),
        ErrorKind::Limit,
        "shared code construction capacity",
    )?;
    let mut use_cursor = 0;
    for (fi, mut f) in p.functions.into_iter().enumerate() {
        budget.work(
            (f.code.len() as u64)
                .checked_mul(6)
                .ok_or_else(|| Error::new(ErrorKind::Limit, "compact work"))?,
        )?;
        let bytes = CompactCode::planned_bytes(&f.code)?;
        let map_bytes = (f.code.len() + 1)
            .checked_mul(4)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "PC map size"))?;
        check(
            retained
                .checked_add(budget.scratch)
                .and_then(|n| n.checked_add(bytes))
                .and_then(|n| n.checked_add(map_bytes))
                .is_some_and(|n| n <= limits.max_allocation_bytes),
            ErrorKind::Limit,
            "compact allocation limit",
        )?;
        budget.peak = budget.peak.max(budget.scratch + bytes + map_bytes);
        let (code, pcs) = CompactCode::from_ordinal(&f.code)?;
        super::compact::map_metadata_pcs(&mut f.regions, &mut f.source_map, |pc| {
            pcs.get(pc.index())
                .copied()
                .map(Pc)
                .ok_or_else(|| Error::new(ErrorKind::Index, "metadata ordinal PC"))
        })?;
        while let Some(used) = opcode_uses
            .get_mut(use_cursor)
            .filter(|used| used.function.index() == fi)
        {
            used.pc = Pc(pcs[used.pc.index()]);
            use_cursor += 1;
        }
        let start = builder.words_len();
        let words = code.end_pc() as u32;
        let count = code.len() as u32;
        builder.append(code.words())?;
        ranges.push((start, words, count));
        budget.peak = budget
            .peak
            .max(budget.scratch + builder.capacity_bytes() + bytes + map_bytes);
        retained -= f.code.capacity_bytes();
        functions.push(FunctionDef {
            module: f.module,
            name: f.name,
            kind: f.kind,
            signature: f.signature,
            context: f.context,
            captures: f.captures,
            register_count: f.register_count,
            bindings: f.bindings,
            descriptors: f.descriptors,
            code: CompactCode::default(),
            regions: f.regions,
            source_map: f.source_map,
        });
    }
    let store = builder.finish()?;
    for (f, &(start, words, count)) in functions.iter_mut().zip(&ranges) {
        f.code = CompactCode::from_store(store.clone(), start, words, count)?;
    }
    budget.release(ranges);
    if !functions.is_empty() {
        instruction_bytes +=
            store.cache_stats().cold_capacity_bytes + functions.len() * size_of::<CompactCode>();
    }
    let package = BytecodePackage {
        format_version: p.format_version,
        language_version: p.language_version,
        host_abi_version: p.host_abi_version,
        content_version: p.content_version,
        text_pool: p.text_pool,
        symbols: p.symbols,
        types: p.types,
        constants: p.constants,
        signatures: p.signatures,
        natives: p.natives,
        modules: p.modules,
        functions,
        capabilities: p.capabilities,
        sources: p.sources,
    };
    stats.instruction_bytes = instruction_bytes;
    let measured = super::codec::measure(&package, limits)?;
    stats.encoded_bytes = measured.0;
    stats.owned_capacity_bytes = measured.1 + stats.derived_capacity_bytes;
    stats.validation_work = budget.work;
    stats.validation_scratch_peak = stats.validation_scratch_peak.max(budget.peak);
    Ok(package)
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
    p: &BytecodePackage<impl InstructionSequence>,
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
fn symbol(p: &BytecodePackage<impl InstructionSequence>, id: SymbolId) -> Result<&str, Error> {
    Ok(get(&p.symbols, id.0, "symbol")?.as_str())
}
fn ty(p: &BytecodePackage<impl InstructionSequence>, id: TypeId) -> Result<(), Error> {
    get(&p.types, id.0, "type")?;
    Ok(())
}
fn reg(f: &FunctionDef<impl InstructionSequence>, r: Reg) -> Result<(), Error> {
    check(r.0 < f.register_count, ErrorKind::Index, "register")
}
fn window(f: &FunctionDef<impl InstructionSequence>, w: RegWindow) -> Result<(), Error> {
    check(
        w.start
            .0
            .checked_add(w.len)
            .is_some_and(|end| end <= f.register_count),
        ErrorKind::Window,
        "register window",
    )
}
fn unique_slice<T, K: Ord>(
    items: &[T],
    key: impl Fn(&T) -> K,
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

fn validate_symbols(
    p: &BytecodePackage<impl InstructionSequence>,
    b: &mut Budget,
) -> Result<(), Error> {
    let hashes = RandomState::new();
    validate_symbols_with_hash(p, b, |text| hashes.hash_one(text))
}

fn validate_symbols_with_hash(
    p: &BytecodePackage<impl InstructionSequence>,
    b: &mut Budget,
    hash: impl Fn(&str) -> u64,
) -> Result<(), Error> {
    for s in &p.symbols {
        b.work(s.len() as u64 + 1)?;
        check(
            !s.is_empty() && !s.contains('\0'),
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
fn version(p: &BytecodePackage<impl InstructionSequence>) -> Result<(), Error> {
    check(
        (p.format_version, p.language_version, p.host_abi_version)
            == (FORMAT_VERSION, LANGUAGE_VERSION, HOST_ABI_VERSION),
        ErrorKind::Version,
        "format/language/host ABI version",
    )
}

pub(crate) fn validate(
    p: &BytecodePackage<impl InstructionSequence>,
    limits: Limits,
) -> Result<PackageStats, Error> {
    validate_program(p, limits).map(|(stats, _)| stats)
}
fn validate_program(
    p: &BytecodePackage<impl InstructionSequence>,
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
    let (encoded, owned, records) = super::codec::measure(p, limits)?;
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
    for (fi, f) in p.functions.iter().enumerate() {
        validate_function(p, fi, f, &parameter_layouts, &mut uses, &mut b).map_err(|mut e| {
            e.function = Some(FunctionId(fi as u32));
            e
        })?;
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
    let module_bindings = module_bindings(p, &mut b)?;
    parameter_layouts.release(&mut b);
    let derived_capacity_bytes = native_parameter_cells.capacity()
        * std::mem::size_of::<(u32, u32)>()
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
            validation_scratch_peak: b.peak,
        },
        DerivedLayouts {
            native_parameters: native_parameter_cells,
            modules: module_bindings,
            opcode_uses,
            member_symbols,
        },
    ))
}

/// Resolve binding aliases iteratively before execution. No runtime object is
/// copied for an import, and malformed alias cycles cannot hang module loading.
fn module_bindings(
    p: &BytecodePackage<impl InstructionSequence>,
    b: &mut Budget,
) -> Result<ModuleBindings, Error> {
    let mut offsets = b.vec::<u32>(p.modules.len() + 1)?;
    let mut count = 0u32;
    for module in &p.modules {
        offsets.push(count);
        count = count
            .checked_add(
                u32::try_from(module.globals.len() + module.imports.len())
                    .map_err(|_| Error::new(ErrorKind::Limit, "module binding count"))?,
            )
            .ok_or_else(|| Error::new(ErrorKind::Limit, "module binding count"))?;
    }
    offsets.push(count);
    let mut values = b.vec::<Option<GlobalBinding>>(count as usize)?;
    values.resize(count as usize, None);
    let mut edges = b.vec::<Option<u32>>(count as usize)?;
    edges.resize(count as usize, None);
    let mut root_offset = 0u32;
    for (index, module) in p.modules.iter().enumerate() {
        for (global, definition) in module.globals.iter().enumerate() {
            b.work(1)?;
            values[offsets[index] as usize + global] = Some(GlobalBinding {
                offset: root_offset,
                ty: definition.ty,
                mutable: definition.mutable,
                imported: false,
            });
            root_offset = root_offset
                .checked_add(1)
                .ok_or_else(|| Error::new(ErrorKind::Limit, "module global count"))?;
        }
        for (import, definition) in module.imports.iter().enumerate() {
            let target = &p.modules[definition.module.index()];
            b.work(target.exports.len() as u64 + 1)?;
            let export = target
                .exports
                .iter()
                .find(|e| e.name == definition.export)
                .expect("validated import export");
            let source = offsets[index] as usize + module.globals.len() + import;
            edges[source] = Some(offsets[definition.module.index()] + export.global.0);
        }
    }
    let mut visiting = b.vec::<bool>(count as usize)?;
    visiting.resize(count as usize, false);
    let mut path = b.vec::<u32>(count as usize)?;
    for source in 0..count {
        let mut at = source;
        while values[at as usize].is_none() {
            b.work(1)?;
            check(
                !visiting[at as usize],
                ErrorKind::Index,
                "import binding cycle has no declaration",
            )?;
            visiting[at as usize] = true;
            path.push(at);
            at = edges[at as usize].expect("unresolved binding is an import");
        }
        let target = values[at as usize].unwrap();
        while let Some(at) = path.pop() {
            b.work(1)?;
            visiting[at as usize] = false;
            values[at as usize] = Some(GlobalBinding {
                imported: true,
                ..target
            });
        }
    }
    let mut resolved = b.vec(count as usize)?;
    resolved.extend(values.iter().map(|value| value.unwrap()));
    b.release(values);
    b.release(edges);
    b.release(visiting);
    b.release(path);
    Ok(ModuleBindings {
        offsets,
        values: resolved,
    })
}

/// Derive once per native signature, retaining only canonical Cell slots.
fn native_parameter_cells(
    p: &BytecodePackage<impl InstructionSequence>,
    layouts: &ParameterLayouts,
    b: &mut Budget,
) -> Result<Vec<(u32, u32)>, Error> {
    if p.natives.is_empty() {
        return Ok(Vec::new());
    }
    // Native declarations may share signatures. Normalize once, avoiding a
    // native_count * default_count * prefix_count derivation or retained map.
    let mut signatures = sorted_keys(p.natives.iter().map(|n| n.signature.0), 1, b)?;
    b.work(signatures.len() as u64)?;
    signatures.dedup();
    let last_prefix = |signature: u32, b: &mut Budget| -> Result<_, Error> {
        let parameters = &p.signatures[signature as usize].parameters;
        b.work(parameters.len() as u64)?;
        Ok(parameters
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, parameter)| match parameter.default {
                Some(ParameterDefault::Function(id)) => Some((i, id)),
                _ => None,
            }))
    };
    let mut count = 0usize;
    b.work(signatures.len() as u64)?;
    for &signature in &signatures {
        if let Some((end, source)) = last_prefix(signature, b)? {
            b.work(end as u64)?;
            for (i, parameter) in p.signatures[signature as usize].parameters[..end]
                .iter()
                .enumerate()
            {
                let cell = layouts.cell(source.index(), i as u32, b)?;
                check(
                    !parameter.mutable || cell,
                    ErrorKind::Signature,
                    "mutable native default prefix requires a parameter cell",
                )
                .map_err(|mut e| {
                    e.function = Some(source);
                    e
                })?;
                count += usize::from(cell);
            }
        }
    }
    let mut cells = b.vec(count)?;
    b.work(signatures.len() as u64)?;
    for &signature in &signatures {
        let Some((end, source)) = last_prefix(signature, b)? else {
            continue;
        };
        let parameters = &p.signatures[signature as usize].parameters;
        b.work(parameters.len() as u64)?;
        for (i, parameter) in parameters.iter().enumerate() {
            if let Some(ParameterDefault::Function(default)) = parameter.default {
                b.work(i as u64)?;
                for index in 0..i {
                    let expected = layouts.cell(source.index(), index as u32, b)?;
                    let actual = layouts.cell(default.index(), index as u32, b)?;
                    check(
                        actual == expected,
                        ErrorKind::Signature,
                        "contradictory native default prefix storage",
                    )
                    .map_err(|mut e| {
                        e.function = Some(default);
                        e
                    })?;
                }
            }
        }
        // All relevant thunks now match. The last/longest prefix is equivalent
        // to taking each readonly slot from its first relevant thunk, and lets
        // us retain only Cells rather than a dense Value/Cell prefix table.
        b.work(end as u64)?;
        for index in 0..end {
            if layouts.cell(source.index(), index as u32, b)? {
                cells.push((signature, index as u32));
            }
        }
    }
    b.release(signatures);
    Ok(cells)
}

/// Sparse declarations only: missing Parameter records retain ordinary Value
/// slots. Shared signatures never allocate function_count * parameter_count maps.
struct ParameterLayouts {
    offsets: Vec<usize>,
    declarations: Vec<(u32, u32, bool)>,
}
impl ParameterLayouts {
    fn build(p: &BytecodePackage<impl InstructionSequence>, b: &mut Budget) -> Result<Self, Error> {
        b.work(p.functions.len() as u64)?;
        let mut count = 0usize;
        let mut bindings = 0u64;
        for (fi, f) in p.functions.iter().enumerate() {
            if matches!(f.kind, FunctionKind::Native(_)) {
                continue;
            }
            let result = (|| {
                get(&p.modules, f.module.0, "function module")?;
                symbol(p, f.name)?;
                let s = get(&p.signatures, f.signature.0, "function signature")?;
                check(
                    f.register_count <= b.limits.max_registers
                        && f.register_count as usize
                            >= s.parameters.len() + usize::from(s.rest.is_some()),
                    ErrorKind::Signature,
                    "parameter register slots",
                )?;
                b.work(f.bindings.len() as u64)?;
                bindings += f.bindings.len() as u64;
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
                                .ok_or_else(|| {
                                    Error::new(ErrorKind::Signature, "parameter binding index")
                                })?
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
                            binding.storage.reg() == Reg(i),
                            ErrorKind::Signature,
                            "parameter binding register",
                        )?;
                        count += 1;
                    }
                }
                Ok(())
            })();
            result.map_err(|mut e: Error| {
                e.function = Some(FunctionId(fi as u32));
                e
            })?;
        }
        let mut declarations = b.vec(count)?;
        b.work(bindings + p.functions.len() as u64)?;
        for (fi, f) in p.functions.iter().enumerate() {
            for binding in &f.bindings {
                if let BindingInitialization::Parameter(i) = binding.initialization {
                    declarations.push((
                        fi as u32,
                        i,
                        matches!(binding.storage, BindingStorage::Cell(_)),
                    ));
                }
            }
        }
        // Rust 1.96's Floyd rebuild has <=2*n comparisons; subsequent in-place
        // heapsort has <=2 comparisons per level. Tuple comparisons cost <=3.
        b.work(6 * declarations.len() as u64)?;
        let heap = BinaryHeap::from(declarations);
        let height = heap.len().checked_ilog2().unwrap_or(0) as u64;
        b.work(heap.len() as u64 * (2 * height + 1) * 3)?;
        let declarations = heap.into_sorted_vec();
        b.work(declarations.len() as u64)?;
        for pair in declarations.windows(2) {
            check(
                (pair[0].0, pair[0].1) != (pair[1].0, pair[1].1),
                ErrorKind::Signature,
                "duplicate parameter binding",
            )
            .map_err(|mut e| {
                e.function = Some(FunctionId(pair[0].0));
                e
            })?;
        }
        let mut offsets = b.vec(p.functions.len() + 1)?;
        b.work(declarations.len() as u64)?;
        let mut next = 0;
        for fi in 0..p.functions.len() {
            offsets.push(next);
            while next < declarations.len() && declarations[next].0 as usize == fi {
                next += 1;
            }
        }
        offsets.push(next);
        Ok(Self {
            offsets,
            declarations,
        })
    }
    fn cell(&self, function: usize, parameter: u32, b: &mut Budget) -> Result<bool, Error> {
        let declarations = &self.declarations[self.offsets[function]..self.offsets[function + 1]];
        // std's binary search halves with rounding up, then compares the final
        // element: ceil(log2(len)) + 1 comparisons, not floor(log2(len)) + 1.
        let levels = usize::BITS - declarations.len().saturating_sub(1).leading_zeros();
        b.work(levels as u64 + 1)?;
        Ok(declarations
            .binary_search_by_key(&parameter, |d| d.1)
            .ok()
            .is_some_and(|i| declarations[i].2))
    }
    fn release(self, b: &mut Budget) {
        b.release(self.declarations);
        b.release(self.offsets);
    }
}

fn validate_function(
    p: &BytecodePackage<impl InstructionSequence>,
    fi: usize,
    f: &FunctionDef<impl InstructionSequence>,
    parameter_layouts: &ParameterLayouts,
    uses: &mut InstructionUses,
    b: &mut Budget,
) -> Result<(), Error> {
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
        return validate_source_map(p, f, true);
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
    for descriptor in &f.descriptors {
        validate_descriptor(p, f, descriptor, b)?;
    }
    validate_source_map(p, f, false)?;
    validate_regions(f, b)?;
    for item in f.code.indexed_iter() {
        let (pc, i) = item?;
        validate_instruction(p, f, pc, &i, b).map_err(|e| located(p, fi, pc, e))?;
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
    Ok(())
}
fn validate_source_map(
    p: &BytecodePackage<impl InstructionSequence>,
    f: &FunctionDef<impl InstructionSequence>,
    native: bool,
) -> Result<(), Error> {
    let mut prev = None;
    for m in &f.source_map {
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
fn validate_call(
    p: &BytecodePackage<impl InstructionSequence>,
    f: &FunctionDef<impl InstructionSequence>,
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
fn validate_descriptor(
    p: &BytecodePackage<impl InstructionSequence>,
    f: &FunctionDef<impl InstructionSequence>,
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
fn validate_captures(
    p: &BytecodePackage<impl InstructionSequence>,
    f: &FunctionDef<impl InstructionSequence>,
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
    p: &BytecodePackage<impl InstructionSequence>,
    f: &FunctionDef<impl InstructionSequence>,
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
fn validate_regions(
    f: &FunctionDef<impl InstructionSequence>,
    b: &mut Budget,
) -> Result<(), Error> {
    for (index, r) in f.regions.iter().enumerate() {
        check(
            r.catch.is_some() || r.finally.is_some(),
            ErrorKind::Region,
            "empty exception region",
        )?;
        for range in ranges(r) {
            check(
                range.start < range.end
                    && range.end.index() <= f.code.end_pc()
                    && f.code.is_boundary(range.start.index())
                    && f.code.is_boundary(range.end.index()),
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
            let end = f
                .code
                .instruction(
                    f.code
                        .previous_pc(cleanup.end.index())
                        .expect("cleanup boundary"),
                )?
                .expect("verified PC");
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
    p: &BytecodePackage<impl InstructionSequence>,
    f: &FunctionDef<impl InstructionSequence>,
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
            Pc => check(
                (value as usize) < f.code.end_pc() && f.code.is_boundary(value as usize),
                ErrorKind::Index,
                "jump PC",
            )?,
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
            f.kind == FunctionKind::Generator
                && matches!(f.context, Context::Ordinary | Context::NoWait)
                && !in_finally,
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
        let next = f.code.next_pc(pc)?;
        check(
            r.finally
                .is_some_and(|range| Some(range.end.index()) == next),
            ErrorKind::Region,
            "EndFinally outside matching cleanup terminator",
        )?;
    }
    match spec.flow {
        Flow::Next => {
            check(
                f.code
                    .next_pc(pc)?
                    .is_some_and(|next| next < f.code.end_pc()),
                ErrorKind::ControlFlow,
                "fallthrough past code",
            )?;
            edge(
                f,
                pc,
                f.code.next_pc(pc)?.expect("checked next PC"),
                false,
                b,
            )?;
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
fn edge(
    f: &FunctionDef<impl InstructionSequence>,
    from: usize,
    to: usize,
    leave: bool,
    b: &mut Budget,
) -> Result<(), Error> {
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
fn exception_target(
    f: &FunctionDef<impl InstructionSequence>,
    pc: Pc,
    skip: Option<RegionId>,
) -> Option<HandlerTarget> {
    unwind_target(f, pc, skip, None)
}
fn strict_region_ancestor(
    f: &FunctionDef<impl InstructionSequence>,
    parent: RegionId,
    child: RegionId,
) -> bool {
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
    f: &FunctionDef<impl InstructionSequence>,
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
fn located(
    p: &BytecodePackage<impl InstructionSequence>,
    fi: usize,
    pc: usize,
    mut e: Error,
) -> Error {
    e.function = Some(FunctionId(fi as u32));
    e.pc = Some(Pc(pc as u32));
    // All callers reach this after the source map's strict PC order is checked.
    let map = &p.functions[fi].source_map;
    e.source = source_span(map, Pc(pc as u32));
    e
}

fn source_span(map: &[SourceMapEntry], pc: Pc) -> Option<SourceSpan> {
    let end = map.partition_point(|entry| entry.pc <= pc);
    end.checked_sub(1).map(|index| map[index].span)
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
        program.stats().encoded_bytes,
        program.stats().owned_capacity_bytes
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
        for (binding, definition) in f.bindings.iter().enumerate() {
            writeln!(
                out,
                "  binding b{binding} {} {definition:?}",
                p.symbols[definition.name.index()]
            )?;
        }
        for (r, region) in f.regions.iter().enumerate() {
            writeln!(out, "  region e{r} {region:?}")?;
        }
        for item in f.code.indexed_iter() {
            let (pc, i) = item.map_err(|_| std::fmt::Error)?;
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
        let mut p: BytecodePackage = BytecodePackage {
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
        let p: BytecodePackage = BytecodePackage {
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

    #[test]
    fn parameter_index_capacity_matches_allocator_and_fails_before_reservation() {
        let p = fixtures::parameter_cell_default();
        let limits = Limits::default();
        let (_, owned, _) = super::super::codec::measure(&p, limits).unwrap();
        let mut b = Budget::new(limits, owned);
        let allocation = allocation_counter::measure(|| {
            let layouts = ParameterLayouts::build(&p, &mut b).unwrap();
            assert!(layouts.cell(1, 0, &mut b).unwrap());
            assert!(layouts.cell(2, 0, &mut b).unwrap());
            assert!(!layouts.cell(1, 1, &mut b).unwrap());
            layouts.release(&mut b);
        });
        let declarations = 3 * std::mem::size_of::<(u32, u32, bool)>();
        let expected = declarations + (p.functions.len() + 1) * std::mem::size_of::<usize>();
        assert_eq!(b.peak, expected);
        assert_eq!(allocation.bytes_max as usize, expected);
        assert_eq!(allocation.bytes_current, 0);
        assert_eq!(b.scratch, 0);
        let mut b = Budget::new(
            Limits {
                max_allocation_bytes: owned + expected - 1,
                ..limits
            },
            owned,
        );
        assert_eq!(
            ParameterLayouts::build(&p, &mut b).err().unwrap().kind,
            ErrorKind::Limit
        );
        assert_eq!(b.peak, declarations);
        let mut b = Budget::new(
            Limits {
                max_work: 6,
                ..limits
            },
            owned,
        );
        assert_eq!(
            ParameterLayouts::build(&p, &mut b).err().unwrap().kind,
            ErrorKind::Limit
        );
        assert_eq!(b.peak, 0);
    }

    #[test]
    fn non_power_of_two_parameter_search_prepays_the_last_comparison() {
        let mut p = fixtures::parameter_cell_default();
        p.signatures[1].parameters.push(Parameter {
            name: SymbolId(12),
            ty: TypeId(0),
            mutable: false,
            default: None,
        });
        p.functions[1].bindings.push(BindingDef {
            name: SymbolId(12),
            ty: TypeId(0),
            mutable: false,
            storage: BindingStorage::Value(Reg(2)),
            initialization: BindingInitialization::Parameter(2),
        });
        let limits = Limits::default();
        let (_, owned, _) = super::super::codec::measure(&p, limits).unwrap();
        let mut b = Budget::new(limits, owned);
        let layouts = ParameterLayouts::build(&p, &mut b).unwrap();
        let mut small = Budget::new(
            Limits {
                max_work: 2,
                ..limits
            },
            owned,
        );
        assert_eq!(
            layouts.cell(1, 2, &mut small).unwrap_err().kind,
            ErrorKind::Limit
        );
        assert_eq!(small.work, 3);
        layouts.release(&mut b);
    }

    #[test]
    fn native_layout_capacity_is_retained_and_prechecked_with_live_parameter_scratch() {
        let p = fixtures::native_parameter_defaults();
        let limits = Limits::default();
        let (_, owned, _) = super::super::codec::measure(&p, limits).unwrap();
        let mut b = Budget::new(limits, owned);
        let layouts = ParameterLayouts::build(&p, &mut b).unwrap();
        let layout_bytes = b.scratch;
        let allocation = allocation_counter::measure(|| {
            let cells = native_parameter_cells(&p, &layouts, &mut b).unwrap();
            assert_eq!(cells, [(1, 0), (1, 1)]);
            assert_eq!(cells.capacity() * std::mem::size_of::<(u32, u32)>(), 16);
            assert_eq!(b.scratch, layout_bytes + 16);
            b.release(cells);
        });
        assert_eq!(allocation.bytes_max, 20); // signature u32 index + two Cell pairs
        assert_eq!(allocation.bytes_current, 0);
        assert_eq!(b.peak, layout_bytes + 20);
        let mut small = Budget::new(
            Limits {
                max_allocation_bytes: owned + layout_bytes + 19,
                ..limits
            },
            owned,
        );
        let small_layouts = ParameterLayouts::build(&p, &mut small).unwrap();
        assert_eq!(
            native_parameter_cells(&p, &small_layouts, &mut small)
                .unwrap_err()
                .kind,
            ErrorKind::Limit
        );
        assert_eq!(small.peak, layout_bytes + 4); // failed before reserving Cell rows
        layouts.release(&mut b);
        assert_eq!(b.scratch, 0);
    }

    #[test]
    fn native_layout_work_limit_precedes_prefix_lookup_and_allocation() {
        let p = fixtures::native_parameter_defaults();
        let limits = Limits::default();
        let (_, owned, _) = super::super::codec::measure(&p, limits).unwrap();
        let mut b = Budget::new(limits, owned);
        let layouts = ParameterLayouts::build(&p, &mut b).unwrap();
        let mut small = Budget::new(
            Limits {
                max_work: 9,
                ..limits
            },
            owned,
        );
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                native_parameter_cells(&p, &layouts, &mut small)
                    .unwrap_err()
                    .kind,
                ErrorKind::Limit
            );
        });
        assert_eq!(small.peak, 4); // only bounded signature normalization
        assert_eq!(
            allocation.bytes_max,
            4 + "verification work limit".len() as u64
        );
        layouts.release(&mut b);
    }
}
