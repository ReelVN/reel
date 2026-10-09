// Copyright 2025 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

use std::any::TypeId;

use crate::core::{Property, UpdateCtx};
use crate::peniko::color::{AlphaColor, Srgb};

/// The color of a widget's content, often text and text decorations.
///
/// Set this on [`Label`](crate::widgets::Label). Controls such as
/// [`Button`](crate::widgets::Button) own a separate child label.
#[expect(missing_docs, reason = "field names are self-descriptive")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContentColor {
    pub color: AlphaColor<Srgb>,
}

impl Property for ContentColor {
    fn static_default() -> &'static Self {
        static DEFAULT: ContentColor = ContentColor {
            color: AlphaColor::BLACK,
        };
        &DEFAULT
    }
}

impl ContentColor {
    /// Creates new `ContentColor` with given value.
    pub const fn new(color: AlphaColor<Srgb>) -> Self {
        Self { color }
    }
}

// ---

impl Default for ContentColor {
    fn default() -> Self {
        *Self::static_default()
    }
}

impl ContentColor {
    /// Helper function to be called in [`Widget::property_changed`](crate::core::Widget::property_changed).
    pub fn prop_changed(ctx: &mut UpdateCtx<'_>, property_type: TypeId) {
        if property_type != TypeId::of::<Self>() {
            return;
        }
        ctx.request_paint_only();
    }
}
