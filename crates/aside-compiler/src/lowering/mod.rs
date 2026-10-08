//! L04 IR v1 and sequential lowering. No VM execution is performed here.
mod budget;
mod build;
mod emit;
pub mod ir;
mod registers;

use crate::{TextRange, hir};
use aside_vm::bytecode as bc;
pub use ir::{BlockId, IrModule};

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_work: u64,
    pub max_capacity_bytes: usize,
    pub max_records: usize,
    pub max_instructions: usize,
    pub max_blocks: usize,
    pub max_registers: u32,
    pub max_depth: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_work: 16_777_216,
            max_capacity_bytes: 64 * 1024 * 1024,
            max_records: 1_000_000,
            max_instructions: 200_000,
            max_blocks: 100_000,
            max_registers: 65_536,
            max_depth: 128,
        }
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub work_units: u64,
    pub capacity_bytes: usize,
    pub capacity_peak_bytes: usize,
    pub records: usize,
    pub blocks: usize,
    pub instructions: usize,
    pub registers: u64,
    pub max_function_registers: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Unsupported,
    MissingLink,
    LinkContract,
    Limit,
    Allocation,
    InvalidIr,
    Bytecode,
}
#[derive(Debug, Clone)]
pub struct Error {
    pub kind: ErrorKind,
    pub span: TextRange,
    pub detail: String,
    pub stats: Stats,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} at {:?}: {}", self.kind, self.span, self.detail)
    }
}
impl std::error::Error for Error {}

/// An exact L03 binding/node identity, never an inferred native name match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalTarget {
    Symbol(hir::SymbolId),
    Node(hir::NodeId),
}
#[derive(Debug, Clone, Copy)]
pub struct NativeLink<'a> {
    pub target: ExternalTarget,
    pub native: bc::NativeId,
    /// Canonical registration identity supplied independently by the trusted linker.
    pub reference: &'a crate::semantic::SymbolReference,
}
/// The embedder supplies a trusted package with real native signatures,
/// capabilities, ABI and default function implementations. B01 verification
/// establishes format validity; trusting a host registration remains the
/// embedder's responsibility. Borrowed input storage is outside lowering's ledger.
#[derive(Debug, Default)]
pub struct Linkage<'a> {
    pub package: Option<&'a bc::VerifiedProgram>,
    pub natives: &'a [NativeLink<'a>],
}
#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub limits: Limits,
    pub bytecode_limits: bc::Limits,
    /// Resolver-provided version token, independent of the preserved source version string.
    pub content_version: u64,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            bytecode_limits: bc::Limits::default(),
            content_version: 1,
        }
    }
}

pub fn lower(
    module: &hir::AnalyzedModule,
    linkage: &Linkage<'_>,
    options: Options,
) -> Result<IrModule, Error> {
    build::lower(module, linkage, options)
}
#[derive(Debug)]
pub struct Compilation {
    pub program: bc::VerifiedProgram,
    pub stats: Stats,
    pub metadata: Metadata,
    pub entry: bc::FunctionId,
}
/// Exact source/resolver identities retained beside the numeric wire version.
/// Arc descriptors retain their original analysis/link identity.
#[derive(Debug)]
pub struct Metadata {
    pub entry_context: crate::semantic::AnalysisContext,
    pub declared_contexts: Vec<hir::DeclaredContext>,
    pub canonical_path: String,
    pub source_version: String,
    pub imports: Vec<hir::Import>,
    pub external_bindings: Vec<hir::ExternalBinding>,
}
impl IrModule {
    /// Consumes the IR, relocates block edges/regions and runs the shared verifier.
    pub fn emit(self) -> Result<Compilation, Error> {
        emit::emit(self)
    }
}
pub fn compile(
    module: &hir::AnalyzedModule,
    linkage: &Linkage<'_>,
    options: Options,
) -> Result<Compilation, Error> {
    lower(module, linkage, options)?.emit()
}
