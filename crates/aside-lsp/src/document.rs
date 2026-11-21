//! Single-document access to the same analysis and queries as the LSP transport.

use crate::{ServerOptions, position::Encoding, workspace::Workspace};
use lsp_types::{CompletionItem, Diagnostic, Hover, Position, Uri};

/// Analyze an in-memory Aside source using the supplied host declarations.
/// Queries use zero-based lines and UTF-16 columns and share a cached analysis.
pub struct DocumentAnalysis {
    workspace: Workspace,
    uri: Uri,
}

impl DocumentAnalysis {
    /// Prepare the host declarations for a document supplied via `set_source`.
    pub fn new(options: ServerOptions) -> Self {
        Self {
            workspace: Workspace::new(options),
            uri: "aside-memory:/src/main.aside"
                .parse()
                .expect("fixed source URI"),
        }
    }

    /// Replace the document. Unchanged text retains its parsed and analyzed state.
    pub fn set_source(&mut self, source: String) -> Result<(), String> {
        if self
            .workspace
            .open_source(&self.uri)
            .is_ok_and(|current| current.text.as_ref() == source)
        {
            return Ok(());
        }
        self.workspace.open(self.uri.clone(), 1, source)
    }

    /// Query completions at the given editor position.
    pub fn completion(&mut self, line: u32, character: u32) -> Result<Vec<CompletionItem>, String> {
        self.workspace
            .complete(&self.uri, Position::new(line, character), Encoding::Utf16)
    }

    /// Query the resolved symbol and source documentation at the given position.
    pub fn hover(&mut self, line: u32, character: u32) -> Result<Option<Hover>, String> {
        self.workspace
            .hover(&self.uri, Position::new(line, character), Encoding::Utf16)
    }

    /// Collect compiler and host diagnostics for the current document.
    pub fn diagnostics(&mut self) -> Result<Vec<Diagnostic>, String> {
        Ok(self
            .workspace
            .diagnostics(&self.uri, Encoding::Utf16)?
            .into_iter()
            .find(|entry| entry.uri == self.uri)
            .map_or_else(Vec::new, |entry| entry.diagnostics))
    }
}
