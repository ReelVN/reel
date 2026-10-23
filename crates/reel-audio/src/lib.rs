//! Host-driven mixing for sounds, tracks, effects and clocks.
//!
//! Create an [`AudioManager`] with a [`backend::Backend`]. The default backend is
//! manually driven; Reel's media adapter supplies native output and background
//! PCM retirement. Mixing uses preallocated storage, and control transfers
//! retired sounds before their physical destruction.
//!
//! Enable the required Symphonia format features when loading encoded sounds.
//! The `serde` feature adds serialization for supported values and settings.
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::doc_lazy_continuation)]
#![allow(clippy::large_enum_variant)]
#![allow(clippy::enum_variant_names)]
#![warn(clippy::todo)]
#![warn(missing_docs)]
#![allow(clippy::tabs_in_doc_comments)]

pub mod backend;
pub mod clock;
pub mod command;
mod decibels;
pub mod effect;
mod error;
mod frame;
pub mod info;
pub mod listener;
mod manager;
mod mix;
pub mod modulator;
mod panning;
mod parameter;
mod playback_rate;
mod playback_state_manager;
mod semitones;
pub mod sound;
mod start_time;
#[cfg(test)]
mod test_helpers;
pub mod track;
mod tween;
mod value;

pub use backend::DefaultBackend;
pub use decibels::*;
pub use error::*;
pub use frame::*;
pub use manager::*;
pub use mix::*;
pub use panning::*;
pub use parameter::*;
pub use pastey;
pub use playback_rate::*;
pub use semitones::*;
pub use start_time::*;
pub use tween::*;
pub use value::*;
