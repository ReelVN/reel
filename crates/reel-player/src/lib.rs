//! Session, UI, assets, media and renderer assembly.
//!
//! [`aside_session::AsideSessionUi`] drives compiled stories through the shared
//! stage. [`presentation`] coordinates candidate acceptance and input geometry;
//! [`story_runtime`] schedules VM, GC and retirement work. The `winit` feature
//! enables platform event conversion; `window` includes Winit and WGPU adapters.
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
pub mod capture;
pub mod fonts;
mod images;
pub mod localization;
pub mod media;
pub mod saves;
pub mod session;
mod stage_widget;
mod transitions;
