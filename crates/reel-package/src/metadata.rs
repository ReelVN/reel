use super::{Budget, Error, ErrorKind, Limits, digest, require};
use aside_vm::bytecode as bc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub schema_version: u32,
    pub bytecode_blake3: String,
    pub language_version: u32,
    pub bytecode_version: u32,
    pub host_abi_version: u32,
    pub content_version: u64,
    pub module_id: u32,
    pub canonical_path: String,
    pub source_version: String,
    pub source_blake3: String,
    pub source_id: u32,
    pub source_span: Span,
    pub symbol_count: u32,
    pub initializer: Initializer,
    #[serde(deserialize_with = "required_option")]
    pub game_entry: Option<GameEntry>,
    pub functions: Vec<FunctionMetadata>,
    pub bindings: Vec<BindingIdentity>,
    pub globals: Vec<GlobalIdentity>,
    pub declarations: Vec<Declaration>,
}
fn required_option<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Initializer {
    pub module_id: u32,
    pub function_id: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameEntry {
    pub module_id: u32,
    pub declaration_index: u32,
    pub function_id: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredContext {
    pub no_wait: bool,
    pub init: bool,
    pub derived: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionMetadata {
    // Array index is the source FunctionId. Initializer and thunks are excluded.
    pub output: u32,
    pub declared_context: DeclaredContext,
    pub known_wait: bool,
    pub dynamic_calls: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingIdentity {
    pub function: u32,
    pub binding: u32,
    pub source: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalIdentity {
    pub global: u32,
    pub source: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub name: String,
    pub source_symbol_id: u32,
    pub source_function_id: u32,
    pub global_id: u32,
    pub function_id: u32,
    pub declaration_span: Span,
}

impl Metadata {
    pub(crate) fn validate(
        &self,
        program: &bc::VerifiedProgram,
        bytes: &[u8],
        limits: Limits,
        budget: &mut Budget,
    ) -> Result<(), Error> {
        use ErrorKind::{Limit, Metadata as Invalid};
        let p = program.package();
        require(
            self.schema_version == 1,
            ErrorKind::Version,
            "unsupported metadata schema",
        )?;
        let records = [
            self.functions.len(),
            self.bindings.len(),
            self.globals.len(),
            self.declarations.len(),
        ]
        .into_iter()
        .try_fold(0usize, |sum, len| sum.checked_add(len))
        .ok_or_else(|| Error::new(Limit, "metadata record count overflow"))?;
        require(
            records <= limits.max_metadata_records
                && self.symbol_count <= limits.max_source_symbols,
            Limit,
            "metadata record/symbol budget exceeded",
        )?;
        budget.charge(bytes.len())?;
        // Prepay tree comparisons and descriptor scans, including adversarial
        // long common prefixes in declaration names. No all-pairs lookup.
        let log = (usize::BITS - records.max(1).leading_zeros()) as usize + 1;
        let max_name = self
            .declarations
            .iter()
            .map(|d| d.name.len())
            .max()
            .unwrap_or(0);
        let work = records
            .checked_mul(log)
            .and_then(|n| n.checked_mul(16))
            .and_then(|n| {
                self.declarations
                    .len()
                    .checked_mul(max_name + 1)
                    .and_then(|v| v.checked_mul(log))
                    .and_then(|v| n.checked_add(v))
            })
            .and_then(|n| n.checked_add(p.functions.len()))
            .ok_or_else(|| Error::new(Limit, "metadata work overflow"))?;
        budget.charge(work)?;
        require(
            self.bytecode_blake3 == digest(bytes),
            ErrorKind::Integrity,
            "metadata refers to different bytecode",
        )?;
        require(
            self.language_version == p.language_version
                && self.bytecode_version == p.format_version
                && self.host_abi_version == p.host_abi_version
                && self.content_version == p.content_version,
            Invalid,
            "metadata/B01 version mismatch",
        )?;
        require(
            p.modules.len() == 1
                && self.module_id == 0
                && p.sources.len() == 1
                && self.source_id == 0,
            Invalid,
            "ASB standalone profile requires one module and one source",
        )?;
        require(
            p.natives.is_empty() && p.modules[0].imports.is_empty() && p.capabilities.is_empty(),
            Invalid,
            "imports, natives and host capabilities are unsupported",
        )?;
        let module = &p.modules[0];
        let source = &p.sources[0];
        budget.charge(source.text.len())?;
        require(
            p.symbols[module.path.index()] == self.canonical_path
                && p.symbols[source.path.index()] == self.canonical_path,
            Invalid,
            "module/source path mismatch",
        )?;
        require(
            self.source_blake3 == digest(source.text.as_bytes()) && !self.source_version.is_empty(),
            Invalid,
            "source content/version mismatch",
        )?;
        require(
            self.source_span.start == 0 && self.source_span.end as usize == source.text.len(),
            Invalid,
            "module source span mismatch",
        )?;
        require(
            self.initializer.module_id == self.module_id
                && module.initializer == Some(bc::FunctionId(self.initializer.function_id)),
            Invalid,
            "initializer mismatch",
        )?;
        let init = p
            .functions
            .get(self.initializer.function_id as usize)
            .ok_or_else(|| Error::new(Invalid, "initializer out of range"))?;
        require(
            init.module.0 == self.module_id
                && init.kind == bc::FunctionKind::Plain
                && init.context == bc::Context::Init
                && init.captures.is_empty(),
            Invalid,
            "invalid initializer contract",
        )?;
        let mut outputs = BTreeSet::new();
        for f in &self.functions {
            let output = p
                .functions
                .get(f.output as usize)
                .ok_or_else(|| Error::new(Invalid, "function mapping out of range"))?;
            let context = &f.declared_context;
            require(
                !(context.derived && context.init),
                Invalid,
                "conflicting declared contexts",
            )?;
            let expected = if context.derived {
                bc::Context::Derived
            } else if context.init {
                bc::Context::Init
            } else if context.no_wait {
                bc::Context::NoWait
            } else {
                bc::Context::Ordinary
            };
            require(
                output.module.0 == self.module_id
                    && output.kind == bc::FunctionKind::Plain
                    && output.context == expected
                    && f.output != self.initializer.function_id
                    && outputs.insert(f.output),
                Invalid,
                "invalid/duplicate function mapping",
            )?;
        }
        let mut globals = BTreeMap::new();
        let mut global_sources = BTreeSet::new();
        for g in &self.globals {
            require(
                (g.global as usize) < module.globals.len()
                    && g.source < self.symbol_count
                    && globals.insert(g.global, g.source).is_none()
                    && global_sources.insert(g.source),
                Invalid,
                "invalid/duplicate global mapping",
            )?;
        }
        require(
            globals.len() == module.globals.len(),
            Invalid,
            "incomplete global mappings",
        )?;
        let mut bindings = BTreeSet::new();
        let mut function_symbols = BTreeSet::new();
        let mut symbol_contracts = BTreeMap::new();
        for b in &self.bindings {
            let function = p
                .functions
                .get(b.function as usize)
                .ok_or_else(|| Error::new(Invalid, "binding function out of range"))?;
            require(
                (b.binding as usize) < function.bindings.len()
                    && b.source < self.symbol_count
                    && bindings.insert((b.function, b.binding))
                    && function_symbols.insert((b.function, b.source))
                    && !global_sources.contains(&b.source),
                Invalid,
                "invalid/duplicate binding mapping",
            )?;
            let def = &function.bindings[b.binding as usize];
            let contract = (def.name, def.ty, def.mutable);
            require(
                symbol_contracts
                    .insert(b.source, contract)
                    .is_none_or(|old| old == contract),
                Invalid,
                "source binding identity changes name/type/mutability across functions",
            )?;
        }
        require(
            bindings.len() == p.functions.iter().map(|f| f.bindings.len()).sum::<usize>(),
            Invalid,
            "incomplete binding mappings",
        )?;
        let mut names = BTreeSet::new();
        let mut declaration_functions = BTreeSet::new();
        for d in &self.declarations {
            let f = self
                .functions
                .get(d.source_function_id as usize)
                .ok_or_else(|| Error::new(Invalid, "declaration source function out of range"))?;
            let global = module
                .globals
                .get(d.global_id as usize)
                .ok_or_else(|| Error::new(Invalid, "declaration global out of range"))?;
            require(
                f.output == d.function_id && globals.get(&d.global_id) == Some(&d.source_symbol_id),
                Invalid,
                "declaration identity mismatch",
            )?;
            let function = &p.functions[d.function_id as usize];
            require(
                p.symbols[global.name.index()] == d.name
                    && p.symbols[function.name.index()] == d.name
                    && !global.mutable
                    && names.insert(&d.name)
                    && declaration_functions.insert(d.function_id),
                Invalid,
                "declaration/B01 name mismatch or duplicate declaration",
            )?;
            validate_span(d.declaration_span, &source.text)?;
        }
        if let Some(entry) = &self.game_entry {
            let declaration = self
                .declarations
                .get(entry.declaration_index as usize)
                .ok_or_else(|| Error::new(Invalid, "entry declaration out of range"))?;
            require(
                entry.module_id == self.module_id
                    && entry.function_id == declaration.function_id
                    && entry.function_id != self.initializer.function_id,
                Invalid,
                "game entry mapping mismatch",
            )?;
            let function = &p.functions[entry.function_id as usize];
            let signature = &p.signatures[function.signature.index()];
            budget.charge(signature.parameters.len())?;
            require(
                function.kind == bc::FunctionKind::Plain
                    && function.captures.is_empty()
                    && signature.parameters.iter().all(|p| p.default.is_some()),
                Invalid,
                "game entry requires plain, capture-free function with zero required arguments",
            )?;
        }
        Ok(())
    }
}
fn validate_span(span: Span, source: &str) -> Result<(), Error> {
    require(
        span.start <= span.end
            && span.end as usize <= source.len()
            && source.is_char_boundary(span.start as usize)
            && source.is_char_boundary(span.end as usize),
        ErrorKind::Metadata,
        "invalid declaration source span",
    )
}
