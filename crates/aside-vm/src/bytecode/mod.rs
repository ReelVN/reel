//! Versioned, pointer-free Aside bytecode. This module does not execute programs.
//!
//! Tooling uses [`verify`]; dispatchers must additionally use
//! [`VerifiedProgram::require_runtime`] with their real execution capabilities.
mod code_residency;
mod codec;
mod compact;
mod descriptors;
mod encoded;
pub mod fixtures;
mod model;
mod source_map;
mod spec;
#[cfg(test)]
mod tests;
mod text_pool;
mod verify;

pub use crate::{HostCallableSupport, HostMethodSupport, HostTypeSupport};
pub(crate) use code_residency::CodeWindow;
#[cfg(test)]
pub(crate) use code_residency::PAGE_WORDS;
pub use code_residency::{CodeCacheStats, CodeStore};
pub use codec::{decode, encode};
pub use compact::{CompactCode, CompactIter, InstructionSequence, ResidentCodeReader};
use descriptors::DescriptorInterner;
pub use descriptors::{DescriptorSequence, SharedDescriptors};
pub use encoded::EncodedProgram;
pub use model::*;
pub use source_map::{CompactSourceMap, SourceMapIter, SourceMapSequence};
pub use spec::*;
pub use text_pool::{TextPool, externalize_text};
pub(crate) use verify::ValueEntry;
pub use verify::{
    CleanupDestination, EditableCloneCost, GlobalBinding, HandlerKind, HandlerTarget, PackageStats,
    VerifiablePackage, VerifiedProgram, disassemble, verify,
};

/// Verified metadata and compact instructions share the editable package model.
pub type RuntimePackage = BytecodePackage<CompactCode, CompactSourceMap, SharedDescriptors>;
pub type RuntimeFunction = FunctionDef<CompactCode, CompactSourceMap, SharedDescriptors>;

/// All limits apply to the whole package, including nested descriptor records.
/// Work is bounded independently for decoding, verification and runtime admission;
/// VerifiedProgram retains these limits for every require_runtime call.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_records: u32,
    pub max_registers: u32,
    pub max_allocation_bytes: usize,
    pub max_work: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: isize::MAX as usize,
            max_records: u32::MAX,
            max_registers: u32::MAX,
            max_allocation_bytes: isize::MAX as usize,
            max_work: u64::MAX,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub offset: Option<usize>,
    pub function: Option<FunctionId>,
    pub pc: Option<Pc>,
    pub source: Option<SourceSpan>,
    pub detail: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Truncated,
    Limit,
    Allocation,
    Version,
    Encoding,
    Index,
    Window,
    Opcode,
    Flags,
    Signature,
    Capture,
    Region,
    ControlFlow,
    Capability,
    Source,
}
impl Error {
    pub(crate) fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            offset: None,
            function: None,
            pc: None,
            source: None,
            detail: detail.into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)?;
        if let Some(function) = self.function {
            write!(f, " (function {})", function.0)?;
        }
        if let Some(pc) = self.pc {
            write!(f, " at pc {}", pc.0)?;
        }
        if let Some(offset) = self.offset {
            write!(f, " at byte {offset}")?;
        }
        Ok(())
    }
}
impl std::error::Error for Error {}
