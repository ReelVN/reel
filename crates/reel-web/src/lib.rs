//! Canvas playback and source editing for the browser.

#[cfg(target_arch = "wasm32")]
mod analysis;
#[cfg(target_arch = "wasm32")]
mod compile;
#[cfg(target_arch = "wasm32")]
mod host;
#[cfg(target_arch = "wasm32")]
mod preview;

#[cfg(target_arch = "wasm32")]
pub use analysis::WebAnalysis;
#[cfg(target_arch = "wasm32")]
pub use preview::WebPreview;
