//! Assembly crate: native windows and surfaces never enter stage/core contracts.
mod demand;
#[cfg(feature = "window")]
pub mod diagnostic;
pub mod platform;
pub mod presentation;
#[cfg(feature = "window")]
pub mod shell;
pub mod story_runtime;
#[cfg(feature = "window")]
mod window;
#[cfg(feature = "window")]
mod window_session;

pub mod aside_session;
pub mod assets;
pub mod audio;
pub mod capture;
pub mod fonts;
mod images;
pub mod localization;
pub mod saves;
pub mod session;
mod stage_widget;
