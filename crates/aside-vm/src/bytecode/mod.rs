//! Versioned, pointer-free Aside bytecode. This module does not execute programs.
//!
//! Tooling uses [`verify`]; dispatchers must additionally use
//! [`VerifiedProgram::require_runtime`] with their real execution capabilities.
mod codec;
pub mod fixtures;
mod model;
mod spec;
#[cfg(test)]
mod tests;
mod verify;

pub use codec::{decode, encode};
pub use model::*;
pub use spec::*;
pub use verify::{
    CleanupDestination, HandlerKind, HandlerTarget, PackageStats, VerifiedProgram, disassemble,
    verify,
};

/// All limits apply to the whole package, including nested descriptor records.
/// Work is bounded independently for decoding, verification and runtime admission;
/// VerifiedProgram retains these limits for every require_runtime call.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_records: u32,
    pub max_string_bytes: u32,
    pub max_registers: u32,
    pub max_allocation_bytes: usize,
    pub max_work: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_records: 262_144,
            max_string_bytes: 1024 * 1024,
            max_registers: 65_536,
            max_allocation_bytes: 64 * 1024 * 1024,
            max_work: 16_777_216,
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
