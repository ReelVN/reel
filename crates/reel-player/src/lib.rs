//! Assembly crate: native windows and surfaces never enter stage/core contracts.
#[cfg(feature = "window")]
mod demand;
#[cfg(feature = "window")]
pub mod diagnostic;
#[cfg(feature = "window")]
pub mod shell;
#[cfg(feature = "window")]
mod story_runtime;
#[cfg(feature = "window")]
mod surface;
#[cfg(feature = "window")]
mod window;
#[cfg(feature = "window")]
mod window_session;

pub mod aside_session;
pub mod session;
mod stage_widget;
