//! Canvas and source compiler embedding for browser previews.

#[cfg(target_arch = "wasm32")]
mod compile;
#[cfg(target_arch = "wasm32")]
mod host;
#[cfg(target_arch = "wasm32")]
mod preview;

#[cfg(target_arch = "wasm32")]
pub use preview::WebPreview;
