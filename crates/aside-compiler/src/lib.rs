//! Aside v1 lossless syntax and conservative semantic analysis.
//! Source analysis and bounded lowering to the shared bytecode format.
mod diagnostic;
pub mod documentation;
pub mod editor;
mod format;
pub mod hir;
pub mod lowering;
pub mod semantic;
pub use semantic::{
    Analysis, AnalysisContext, AnalysisLimits, AnalysisOptions, analyze, analyze_module,
    analyze_source,
};
mod lexer;
mod parser;
mod raw;
mod syntax;
pub use aside_syntax::{GreenNode, TextRange, TextSize};
pub use diagnostic::*;
pub use format::{FormatError, format, format_parsed};
pub use lexer::{Lexed, Token, lex, lex_with_limits};
pub use parser::{EntryPoint, Parse, ParseOptions, parse, parse_with_options};
pub use syntax::*;
/// Pinned Unicode XID table version used by this package.
pub const UNICODE_VERSION: (u8, u8, u8) = unicode_ident::UNICODE_VERSION;
/// Caller-configurable budgets. Source bytes are rejected before allocation;
/// other exhaustion preserves the remaining source as an error leaf.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_source_bytes: usize,
    pub max_tokens: usize,
    pub max_depth: usize,
    pub max_diagnostics: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            // Source locations use u32 byte offsets; there is no smaller author cap.
            max_source_bytes: u32::MAX as usize,
            max_tokens: u32::MAX as usize,
            max_depth: 128,
            max_diagnostics: 128,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceTooLarge {
    pub actual_bytes: usize,
    pub limit_bytes: usize,
}
impl std::fmt::Display for SourceTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "source is {} bytes; limit is {} bytes",
            self.actual_bytes, self.limit_bytes
        )
    }
}
impl std::error::Error for SourceTooLarge {}
impl Limits {
    pub(crate) fn normalized(mut self) -> Self {
        self.max_source_bytes = self.max_source_bytes.min(u32::MAX as usize);
        self.max_depth = self.max_depth.max(1);
        self.max_diagnostics = self.max_diagnostics.max(1);
        self
    }
    pub(crate) fn check(self, source: &str) -> Result<(), SourceTooLarge> {
        if source.len() > self.max_source_bytes {
            Err(SourceTooLarge {
                actual_bytes: source.len(),
                limit_bytes: self.max_source_bytes,
            })
        } else {
            Ok(())
        }
    }
}
