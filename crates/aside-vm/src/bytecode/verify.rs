use super::code_residency::{CodeStore, CodeStoreBuilder, PAGE_WORDS};
use super::*;
pub(crate) use scalar_entry::ValueEntry;
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
pub struct VerifiedProgram(
    std::sync::Arc<VerifiedData>,
    Option<TextPool>,
    Option<std::sync::Arc<text_matches::TextMatches>>,
);

#[derive(Debug)]
struct VerifiedData {
    identity: [u8; 32],
    package: RuntimePackage,
    stats: PackageStats,
    limits: Limits,
    /// Sorted (signature, parameter) pairs; only Cell parameters are retained.
    native_parameter_cells: Vec<(u32, u32)>,
    /// Sorted (function, parameter) pairs; Value slots need no retained record.
    script_parameter_cells: Vec<(u32, u32)>,
    module_bindings: ModuleBindings,
    opcode_uses: Vec<OpcodeUse>,
    member_symbols: Vec<SymbolId>,
    native_calls: native_calls::NativeCallPlans,
    compact_derived: Vec<u32>,
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    scalar_suffixes: scalar_suffixes::ScalarSuffixes,
    scalar_entries: scalar_entry::ScalarEntries,
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
    script_parameters: Vec<(u32, u32)>,
    modules: ModuleBindings,
    opcode_uses: Vec<OpcodeUse>,
    member_symbols: Vec<SymbolId>,
    native_calls: native_calls::NativeCallPlans,
    compact_derived: Vec<u32>,
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    scalar_suffixes: scalar_suffixes::ScalarSuffixes,
    scalar_entries: scalar_entry::ScalarEntries,
    match_functions: text_matches::MatchFunctions,
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
                .and_then(|n| n.checked_sub(f.source_map.capacity_bytes()))
                .and_then(|n| {
                    n.checked_add(
                        f.source_map
                            .len()
                            .checked_mul(size_of::<SourceMapEntry>())?,
                    )
                })
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
            + self.text_match_capacity_bytes()
            + self.1.as_ref().map_or(0, |pool| pool.capacity_bytes());
        self.code_cache_stats().map_or(actual, |code| {
            actual + code.hot_limit_bytes + code.construction_scratch_bytes
        })
    }
    pub fn stats(&self) -> PackageStats {
        let mut stats = self.0.stats;
        let match_bytes = self.text_match_capacity_bytes();
        stats.derived_capacity_bytes += match_bytes;
        stats.owned_capacity_bytes += match_bytes;
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
    /// The verifier fixes every parameter register and retains only Cell slots.
    pub(crate) fn script_parameter_storage(
        &self,
        function: FunctionId,
        parameter: u32,
    ) -> BindingStorage {
        if self
            .0
            .script_parameter_cells
            .binary_search(&(function.0, parameter))
            .is_ok()
        {
            BindingStorage::Cell(Reg(parameter))
        } else {
            BindingStorage::Value(Reg(parameter))
        }
    }
    /// Binary search uses ceil(log2(len)) + 1 comparisons for a nonempty table.
    pub(crate) fn script_parameter_lookup_work(&self) -> u64 {
        let length = self.0.script_parameter_cells.len();
        u64::from(usize::BITS - length.saturating_sub(1).leading_zeros()) + 1
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
        let mut unbound_capacity =
            self.execution_capacity_bytes() - self.1.as_ref().map_or(0, TextPool::capacity_bytes);
        if unbound_capacity
            .checked_add(pool.capacity_bytes())
            .is_none_or(|bytes| bytes > self.0.limits.max_allocation_bytes)
        {
            unbound_capacity -= self.text_match_capacity_bytes();
            self.disable_text_matches();
        }
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
    ) -> Result<&FunctionDef<impl InstructionSequence, impl SourceMapSequence>, Error> {
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
        self.bind_runtime(support, &support.opcodes, |_, _| Ok(()))
    }

    pub(crate) fn bind_runtime(
        &self,
        support: &RuntimeSupport,
        opcodes: &[Opcode],
        mut bind_native: impl FnMut(NativeId, &NativeSupport) -> Result<(), Error>,
    ) -> Result<(), Error> {
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
        for (index, native) in p.natives.iter().enumerate() {
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
            if let Some(leaf) = registered.read_only_leaf {
                check(
                    leaf.work != 0 && registered.signature.effects.0 == 0,
                    ErrorKind::Capability,
                    "read-only leaf requires nonzero work and pure effects",
                )?;
            }
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
            bind_native(NativeId(index as u32), registered)?;
        }
        // Typed Opcode entries share the frozen u16 domain. Normalize once;
        // dispatch admission does not rescan the host list per instruction.
        let opcode_slots = INSTRUCTION_SPECS.last().expect("opcode table").opcode as usize + 1;
        let mut supported = [false; 256];
        b.work(opcode_slots as u64 + opcodes.len() as u64)?;
        for opcode in opcodes {
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
        let package = compact_package(
            self,
            &mut stats,
            &mut layouts.opcode_uses,
            #[cfg(any(test, feature = "vm-validation-oracle"))]
            &mut layouts.scalar_suffixes,
            limits,
        )?;
        let identity = super::codec::identity(&package, limits)?;
        publish(package, stats, layouts, limits, identity)
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
    mut stats: PackageStats,
    layouts: DerivedLayouts,
    limits: Limits,
    identity: [u8; 32],
) -> Result<VerifiedProgram, Error> {
    let mut budget = Budget::new(limits, stats.owned_capacity_bytes);
    budget.peak = stats.validation_scratch_peak;
    budget.work = stats.validation_work;
    let text_matches = layouts.match_functions.build(&package, &mut budget)?;
    stats.validation_scratch_peak = budget.peak;
    stats.validation_work = budget.work;
    Ok(VerifiedProgram(
        std::sync::Arc::new(VerifiedData {
            identity,
            package,
            stats,
            limits,
            native_parameter_cells: layouts.native_parameters,
            script_parameter_cells: layouts.script_parameters,
            module_bindings: layouts.modules,
            opcode_uses: layouts.opcode_uses,
            member_symbols: layouts.member_symbols,
            native_calls: layouts.native_calls,
            compact_derived: layouts.compact_derived,
            #[cfg(any(test, feature = "vm-validation-oracle"))]
            scalar_suffixes: layouts.scalar_suffixes,
            scalar_entries: layouts.scalar_entries,
        }),
        None,
        text_matches,
    ))
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
    publish(package, stats, layouts, limits, identity)
}
fn compact_package(
    p: BytecodePackage,
    stats: &mut PackageStats,
    opcode_uses: &mut [OpcodeUse],
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    scalar_suffixes: &mut scalar_suffixes::ScalarSuffixes,
    limits: Limits,
) -> Result<RuntimePackage, Error> {
    let original_capacity = stats.owned_capacity_bytes;
    let original_peak = original_capacity + stats.validation_scratch_peak;
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
        let compact_work = 6;
        #[cfg(any(test, feature = "vm-validation-oracle"))]
        let compact_work = compact_work + 4;
        budget.work(
            (f.code.len() as u64)
                .checked_mul(compact_work)
                .ok_or_else(|| Error::new(ErrorKind::Limit, "compact work"))?,
        )?;
        let bytes = CompactCode::planned_bytes(&f.code)?;
        // Four deltas of up to five bytes are measured, encoded and scanned;
        // the remaining units cover rebasing and checkpoint publication.
        budget.work(f.source_map.len() as u64 * 64)?;
        // Each delta occupies at most five bytes before PCs are rebased.
        let source_bound = CompactSourceMap::planned_capacity(
            f.source_map.len(),
            f.source_map
                .len()
                .checked_mul(20)
                .ok_or_else(|| Error::new(ErrorKind::Limit, "source map construction size"))?,
        )?;
        let map_bytes = (f.code.len() + 1)
            .checked_mul(4)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "PC map size"))?;
        check(
            retained
                .checked_add(budget.scratch)
                .and_then(|n| n.checked_add(bytes))
                .and_then(|n| n.checked_add(map_bytes))
                .and_then(|n| n.checked_add(source_bound))
                .is_some_and(|n| n <= limits.max_allocation_bytes),
            ErrorKind::Limit,
            "compact allocation limit",
        )?;
        budget.peak = budget
            .peak
            .max(budget.scratch + bytes + map_bytes + source_bound);
        let (code, pcs) = CompactCode::from_ordinal(&f.code)?;
        #[cfg(any(test, feature = "vm-validation-oracle"))]
        scalar_suffixes.rebase(fi, &pcs);
        super::compact::map_metadata_pcs(&mut f.regions, &mut f.source_map, |pc| {
            pcs.get(pc.index())
                .copied()
                .map(Pc)
                .ok_or_else(|| Error::new(ErrorKind::Index, "metadata ordinal PC"))
        })?;
        let source_map = CompactSourceMap::from_entries(&f.source_map)?;
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
        retained = retained - f.source_map.capacity_bytes() + source_map.capacity_bytes();
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
            source_map,
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
    stats.records = measured.2;
    stats.validation_work = budget.work;
    stats.validation_scratch_peak = stats
        .validation_scratch_peak
        .max(budget.peak)
        .max(original_peak.saturating_sub(stats.owned_capacity_bytes))
        .max(if package.sources.is_empty() {
            0
        } else {
            super::codec::SOURCE_COMPRESSION_SCRATCH
        });
    Ok(package)
}
#[derive(Clone, Copy)]
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
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
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
fn symbol(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
    id: SymbolId,
) -> Result<&str, Error> {
    Ok(get(&p.symbols, id.0, "symbol")?.as_str())
}
fn ty(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
    id: TypeId,
) -> Result<(), Error> {
    get(&p.types, id.0, "type")?;
    Ok(())
}
fn reg(
    f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence>,
    r: Reg,
) -> Result<(), Error> {
    check(r.0 < f.register_count, ErrorKind::Index, "register")
}
fn window(
    f: &FunctionDef<impl InstructionSequence, impl SourceMapSequence>,
    w: RegWindow,
) -> Result<(), Error> {
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
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
    b: &mut Budget,
) -> Result<(), Error> {
    let hashes = RandomState::new();
    validate_symbols_with_hash(p, b, |text| hashes.hash_one(text))
}

fn validate_symbols_with_hash(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
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
fn version(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
) -> Result<(), Error> {
    check(
        (p.format_version, p.language_version, p.host_abi_version)
            == (FORMAT_VERSION, LANGUAGE_VERSION, HOST_ABI_VERSION),
        ErrorKind::Version,
        "format/language/host ABI version",
    )
}

mod program;
pub(crate) use program::validate;
use program::validate_program;

/// Resolve binding aliases iteratively before execution. No runtime object is
/// copied for an import, and malformed alias cycles cannot hang module loading.
fn module_bindings(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
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
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
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
    fn build(
        p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
        b: &mut Budget,
    ) -> Result<Self, Error> {
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
    fn script_cells(&self, b: &mut Budget) -> Result<Vec<(u32, u32)>, Error> {
        b.work(self.declarations.len() as u64 * 2)?;
        let count = self.declarations.iter().filter(|d| d.2).count();
        let mut cells = b.vec(count)?;
        cells.extend(
            self.declarations
                .iter()
                .filter(|d| d.2)
                .map(|&(function, parameter, _)| (function, parameter)),
        );
        Ok(cells)
    }
}

mod compact_derived;
mod functions;
mod native_calls;
mod scalar_entry;
#[cfg(any(test, feature = "vm-validation-oracle"))]
mod scalar_suffixes;
mod text_matches;
use functions::validate_function;

mod control_flow;
use control_flow::{
    exception_target, ranges, unwind_target, validate_instruction, validate_regions,
};
fn located(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
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

fn source_span(map: &impl SourceMapSequence, pc: Pc) -> Option<SourceSpan> {
    map.source_at(pc)
}

mod disassembly;
pub use disassembly::disassemble;
