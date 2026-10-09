//! Assembly crate: native windows and surfaces never enter stage/core contracts.
#[cfg(feature = "window")]
pub mod diagnostic;
#[cfg(feature = "window")]
pub mod shell;
#[cfg(feature = "window")]
mod window;
