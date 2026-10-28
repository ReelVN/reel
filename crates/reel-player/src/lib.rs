//! Session, UI, assets, media and renderer assembly.
//!
//! [`aside_session::AsideSessionUi`] drives compiled stories through the shared
//! stage. [`presentation`] coordinates candidate acceptance and input geometry;
//! [`story_runtime`] schedules VM, GC and retirement work. The `winit` feature
//! enables platform event conversion; `window` includes Winit and WGPU adapters.
//! The `vita` feature assembles the fixed-display GXM host, Opus/AudioOut and
//! AvPlayer adapters for `armv7-sony-vita-newlibeabihf`.
mod demand;
pub mod distribution;
pub mod hal;
pub mod platform;
pub mod player;
pub mod presentation;
#[cfg(all(feature = "profiling", any(target_os = "vita", test)))]
mod probe;
pub mod story_runtime;
#[cfg(test)]
mod test_cost;
#[cfg(test)]
mod test_image;

pub mod aside_session;
pub mod assets;
pub mod capture;
pub mod fonts;
mod images;
#[cfg(any(feature = "vita", test))]
mod launcher;
pub mod localization;
pub mod media;
pub mod saves;
pub mod session;
mod stage_widget;
mod transitions;
