//! ASB 1.0 containers for linked source graphs. No compiler, runtime admission or IO.
//! Integrity hashes are not signatures or host authorization.
pub mod archive;
mod codec;
mod declarations;
mod metadata;
pub mod distribution;
use aside_vm::bytecode as bc;
pub use codec::{HEADER_LEN, INDEX_ENTRY_LEN, MAGIC, decode, encode};
pub use metadata::*;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_file_bytes: usize,
    pub max_metadata_bytes: usize,
    pub max_sections: usize,
    pub max_metadata_records: usize,
    pub max_source_symbols: u32,
    pub max_json_depth: usize,
    pub max_json_string_bytes: usize,
    pub max_json_tokens: usize,
    /// Container hashing/scanning and conservative metadata validation work.
    /// B01 decoding/verification each retain their own independent work budget.
    pub max_work: u64,
    pub bytecode: bc::Limits,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: isize::MAX as usize,
            max_metadata_bytes: isize::MAX as usize,
            max_sections: u32::MAX as usize,
            max_metadata_records: u32::MAX as usize,
            max_source_symbols: u32::MAX,
            max_json_depth: 16,
            max_json_string_bytes: u32::MAX as usize,
            max_json_tokens: u32::MAX as usize,
            max_work: u64::MAX,
            bytecode: bc::Limits::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Limit,
    Encoding,
    Version,
    Integrity,
    Metadata,
    Bytecode,
}
#[derive(Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub detail: String,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}
impl std::error::Error for Error {}
impl Error {
    pub(crate) fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}
pub(crate) fn require(ok: bool, kind: ErrorKind, detail: &str) -> Result<(), Error> {
    if ok {
        Ok(())
    } else {
        Err(Error::new(kind, detail))
    }
}

/// Constructed only after B01 verification and metadata cross-validation.
#[derive(Debug)]
pub struct CodePackage {
    program: bc::VerifiedProgram,
    metadata: Metadata,
}
impl CodePackage {
    /// Bind the external immutable text storage before runtime admission.
    pub fn bind_text_pool(mut self, pool: bc::TextPool) -> Result<Self, Error> {
        self.program = self.program.bind_text_pool(pool)
            .map_err(|error| Error::new(ErrorKind::Bytecode, error.to_string()))?;
        Ok(self)
    }
    pub fn program(&self) -> &bc::VerifiedProgram {
        &self.program
    }
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
    pub fn into_parts(self) -> (bc::VerifiedProgram, Metadata) {
        (self.program, self.metadata)
    }
}
pub fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub(crate) struct Budget {
    remaining: u64,
}
impl Budget {
    pub fn new(limits: Limits) -> Self {
        Self {
            remaining: limits.max_work,
        }
    }
    pub fn charge(&mut self, amount: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(amount as u64)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "ASB work budget exceeded"))?;
        Ok(())
    }
}
