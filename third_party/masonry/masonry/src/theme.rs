// Copyright 2019 the Xilem Authors and the Druid Authors
// SPDX-License-Identifier: Apache-2.0

//! Default values used by various widgets in their paint methods.

#![allow(missing_docs, reason = "Names are self-explanatory.")]

use crate::core::{DefaultProperties, PropertyStack, Selector, StyleProperty, StyleSet};
use crate::layout::{AsUnit, Length};
use crate::parley::{GenericFamily, LineHeight};
use crate::peniko::Color;

// We use glob imports here to avoid frequent merge conflicts.
use crate::properties::*;
use crate::widgets::*;

/// Default color for the app background.
///
/// If the app driver does some kind beginning-of-frame clearing,
/// it should clear with this color by default.
pub const BACKGROUND_COLOR: Color = Color::from_rgb8(0x1D, 0x1D, 0x1D);

pub const BORDER_WIDTH: Length = Length::const_px(1.);

// Zync color variations from https://tailwindcss.com/docs/colors
pub const ZYNC_900: Color = Color::from_rgb8(0x18, 0x18, 0x1b);
pub const ZYNC_800: Color = Color::from_rgb8(0x27, 0x27, 0x2a);
pub const ZYNC_700: Color = Color::from_rgb8(0x3f, 0x3f, 0x46);
pub const ZYNC_600: Color = Color::from_rgb8(0x52, 0x52, 0x5b);
pub const ZYNC_500: Color = Color::from_rgb8(0x71, 0x71, 0x7a);

pub const ACCENT_COLOR: Color = Color::from_rgb8(0x3b, 0x7e, 0xe4);
pub const TEXT_COLOR: Color = Color::from_rgb8(0xf2, 0xf2, 0xf2);
pub const DISABLED_TEXT_COLOR: Color = Color::from_rgb8(0xa0, 0xa0, 0x9a);
pub const PLACEHOLDER_COLOR: Color = Color::from_rgba8(0xFF, 0xFF, 0xFF, 0x8F);
pub const TEXT_BACKGROUND_COLOR: Color = Color::from_rgb8(0x16, 0x16, 0x16);
pub const FOCUS_COLOR: Color = Color::from_rgb8(0xff, 0xff, 0xff);

// TODO: The following constants are not being used in properties
pub const TEXT_SIZE_NORMAL: f32 = 15.0;
pub const BASIC_WIDGET_HEIGHT: Length = Length::const_px(18.0);
pub const BORDERED_WIDGET_HEIGHT: f64 = 24.0;
pub const SCROLLBAR_COLOR: Color = Color::from_rgb8(0xff, 0xff, 0xff);
pub const SCROLLBAR_BORDER_COLOR: Color = Color::from_rgb8(0x77, 0x77, 0x77);
pub const SCROLLBAR_WIDTH: f64 = 8.;
pub const SCROLLBAR_PAD: f64 = 2.;
pub const SCROLLBAR_MIN_SIZE: f64 = 45.;
pub const SCROLLBAR_RADIUS: f64 = 5.;
pub const SCROLLBAR_EDGE_WIDTH: f64 = 1.;
pub const DEFAULT_GAP: Length = Length::const_px(10.0);
pub const DEFAULT_SPACER_LEN: Length = Length::const_px(10.0);
pub const WIDGET_CONTROL_COMPONENT_PADDING: Length = Length::const_px(4.0);
pub const SELECTOR_MIN_WIDTH: f64 = 100.0;

pub fn default_property_set() -> DefaultProperties {
    let mut properties = DefaultProperties::new();
    // Button
    properties.insert::<Button, _>(Padding::from_vh(6.px(), 16.px()));
    properties.insert::<Button, _>(CornerRadius { radius: 6.px() });
    properties.insert::<Button, _>(BorderWidth {
        width: BORDER_WIDTH,
    });
    properties.insert::<Button, _>(Background::Color(ZYNC_800));
    properties.insert::<Button, _>(BorderColor { color: ZYNC_700 });
    {
        let mut stack = PropertyStack::new();
        stack.push_layer(
            Selector::new().with_hovered(true),
            BorderColor { color: ZYNC_500 },
        );
        stack.push_layer(
            Selector::new().with_focused(true),
            BorderColor { color: FOCUS_COLOR },
        );
        stack.push_layer(
            Selector::new().with_active(true),
            Background::Color(ZYNC_700),
        );
        stack.push_layer(
            Selector::new().with_disabled(true),
            Background::Color(Color::BLACK),
        );
        properties.insert_stack::<Button>(stack);
    }

    // Label
    properties.insert::<Label, _>(ContentColor::new(TEXT_COLOR));
    {
        let mut stack = PropertyStack::new();
        stack.push_layer(
            Selector::new().with_disabled(true),
            ContentColor::new(DISABLED_TEXT_COLOR),
        );
        properties.insert_stack::<Label>(stack);
    }

    // Slider
    properties.insert::<Slider, _>(TrackThickness(4.px()));
    properties.insert::<Slider, _>(TrackColor {
        active: ACCENT_COLOR,
        inactive: ZYNC_800,
    });
    properties.insert::<Slider, _>(ThumbColor(TEXT_COLOR));
    properties.insert::<Slider, _>(ThumbRadius(7.px()));
    {
        let mut stack = PropertyStack::new();
        stack.push_layer(
            Selector::new().with_hovered(true),
            BorderColor { color: ZYNC_500 },
        );
        stack.push_layer(
            Selector::new().with_focused(true),
            BorderColor { color: FOCUS_COLOR },
        );
        properties.insert_stack::<Slider>(stack);
    }

    properties
}
/// Applies the default text styles for Masonry into `styles`.
pub fn default_text_styles(styles: &mut StyleSet) {
    styles.insert(StyleProperty::LineHeight(LineHeight::FontSizeRelative(1.2)));
    styles.insert(GenericFamily::SystemUi.into());
}
