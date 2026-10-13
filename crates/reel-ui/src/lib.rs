// Copyright 2018 the Xilem Authors and the Druid Authors
// Copyright 2025 the Xilem Authors
// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Reel's platform-independent UI: widgets, layout, input, presentation geometry,
//! portable drawing frames and budgeted resource leases.
//!
//! The host drives [`app::RenderRoot`] through [`bridge::PresentationBridge`].
//! Renderers consume [`frame::RenderFrame`]; native windows and GPU objects belong
//! to the renderer/player, outside this crate. See [`doc`] for UI concepts.

#![forbid(unsafe_code)]
#![expect(
    clippy::cast_possible_truncation,
    reason = "Explicit UI numeric boundaries inherited from the maintained widget engine"
)]
#![expect(
    clippy::large_enum_variant,
    reason = "NewWidget owns a widget and its construction state"
)]

pub use imaging;
pub use peniko::color::palette;
pub use reel_text::{Alignment as TextAlign, AlignmentOptions as TextAlignOptions};
pub use {accesskit, anymore, dpi, kurbo, peniko, reel_text, ui_events};

pub mod doc;
#[macro_use]
pub mod util;
pub mod app;
pub mod bridge;
pub mod core;
pub mod frame;
pub mod layout;
pub mod properties;
pub mod resources;
pub mod stage;
pub mod theme;
pub mod widgets;

mod passes;
mod tree_arena;
