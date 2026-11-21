use aside_lsp::DocumentAnalysis;
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// Aside language services for an in-memory browser editor.
/// The editor can run these queries in a worker without a canvas or WebGPU.
#[wasm_bindgen]
pub struct WebAnalysis {
    document: DocumentAnalysis,
}

#[wasm_bindgen]
impl WebAnalysis {
    /// Start an editor with Reel's registered native declarations.
    #[wasm_bindgen(constructor)]
    pub fn with_reel_api() -> Self {
        console_error_panic_hook::set_once();
        Self {
            document: DocumentAnalysis::new(reel_editor::options()),
        }
    }

    /// Set the document to analyze. Repeated queries reuse that source analysis.
    #[wasm_bindgen(js_name = setSource)]
    pub fn set_source(&mut self, source: String) -> Result<(), JsError> {
        self.document
            .set_source(source)
            .map_err(|message| JsError::new(&message))
    }

    /// Return the LSP completion items as JSON at a zero-based UTF-16 position.
    pub fn completion(&mut self, line: u32, character: u32) -> Result<String, JsError> {
        json(self.document.completion(line, character))
    }

    /// Return the LSP hover as JSON, or null if the position has none.
    pub fn hover(&mut self, line: u32, character: u32) -> Result<String, JsError> {
        json(self.document.hover(line, character))
    }

    /// Return the document's LSP diagnostics as JSON.
    pub fn diagnostics(&mut self) -> Result<String, JsError> {
        json(self.document.diagnostics())
    }
}

fn json(value: Result<impl Serialize, String>) -> Result<String, JsError> {
    let value = value.map_err(|message| JsError::new(&message))?;
    serde_json::to_string(&value).map_err(|error| JsError::new(&error.to_string()))
}
