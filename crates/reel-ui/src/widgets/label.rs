// Copyright 2019 the Xilem Authors and the Druid Authors
// SPDX-License-Identifier: Apache-2.0

use std::any::TypeId;
use std::mem::Discriminant;

use accesskit::{Node, Role};
use reel_core::text::Text;
use smallvec::SmallVec;
use tracing::{Span, trace_span};
use unicode_segmentation::UnicodeSegmentation;
mod paragraph;
use paragraph::Paragraph;

use crate::core::{
    AccessCtx, ArcStr, BrushIndex, ChildrenIds, EventCtx, LayoutCtx, MeasureCtx, PaintCtx,
    PointerButton, PointerEvent, PropertiesMut, PropertiesRef, RegisterCtx, StyleProperty,
    StyleSet, TextEvent, TextFit, TextOutline, TextShadow, Update, UpdateCtx, UsesProperty, Widget,
    WidgetId, WidgetMut, set_accesskit_brush_properties,
};
use crate::imaging::Painter;
use crate::kurbo::{Affine, Axis, Point, Rect, Size};
use crate::layout::{AsUnit, LenReq, Length};
use crate::properties::{ContentColor, LineBreaking};
use crate::reel_text::{FontContext, LayoutAccessibility, LayoutContext};
use crate::theme::default_text_styles;
use crate::{TextAlign, TextAlignOptions, theme};

/// A text paragraph with optional inline links.
///
/// This is useful for creating interactive widgets which internally
/// need support for displaying text, such as a button.
///
/// You can customize the look of this label with the
/// [`LineBreaking`] and [`ContentColor`] properties.
///
/// ![Styled text label](data:image/png;base64,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)
pub struct Label {
    /// Cached layouts.
    layouts: Vec<TextLayout>,
    /// Time tracking for cache usage.
    cache_time: u8,
    /// The currently active layout index.
    ///
    /// `usize::MAX` works well for none, as it will be overwritten before its used
    /// for layout access, but will be read as-is during cache eviction.
    /// During which any value larger than the cache capacity will be ignored.
    active_layout: usize,

    text: Text,
    visible_bytes: Option<std::ops::Range<usize>>,
    reveal_fade: Option<crate::core::RevealFade>,
    fade_graphemes: Vec<usize>,
    paint_viewport: Option<Rect>,
    paint_line_range: std::ops::Range<usize>,
    styles: StyleSet,
    text_alignment: TextAlign,
    text_fit: TextFit,
    outline: Option<TextOutline>,
    shadow: Option<TextShadow>,
    pressed_link: Option<usize>,
    hovered_link: Option<usize>,
    focused_link: usize,
    link_button: super::ButtonBehavior,

    /// Whether to hint whilst drawing the text.
    ///
    /// Should be disabled whilst an animation involving this label is ongoing.
    // TODO: What classes of animations?
    hint: bool,

    accessibility_hidden: bool,
    accessibility: LayoutAccessibility,
}

/// Text layout computation inputs and output.
struct TextLayout {
    /// Computed text layout.
    layout: Paragraph,
    /// Max advance value that was used when calculating this layout.
    max_advance: Option<f32>,
    scale: f32,
    /// Text alignment of this layout.
    alignment: TextAlign,
    /// Text alignment width of this layout.
    alignment_width: f32,
    /// Last use timestamp for cache eviction purposes.
    last_used: u8,
}

impl TextLayout {
    /// Text layout width differences less than 0.01 pixels can be considered equal.
    const EPSILON: f32 = 0.01;

    /// Creates a new [`TextLayout`] with the specified `max_advance` constraint and `timestamp`.
    fn new(max_advance: Option<f32>, scale: f32, timestamp: u8) -> Self {
        Self {
            layout: Paragraph::default(),
            max_advance,
            scale,
            alignment: TextAlign::Start,
            alignment_width: -1., // Not aligned yet
            last_used: timestamp,
        }
    }

    /// Discards this layout, making it an obvious choice for cache eviction.
    fn discard(&mut self) {
        // Mark it as least recently used.
        self.last_used = 0;
        // Set a fake unlikely-to-be-seen max advance so it won't get any use.
        self.max_advance = Some(-1.);
    }

    /// Align the text layout.
    ///
    /// This method ensures that alignment only happens when the inputs have changed.
    fn align(&mut self, alignment: TextAlign, alignment_width: f32) {
        if self.alignment == alignment
            && (self.alignment_width - alignment_width).abs() < Self::EPSILON
        {
            return;
        }
        self.alignment = alignment;
        self.alignment_width = alignment_width;
        self.layout.align(
            Some(self.alignment_width),
            self.alignment,
            TextAlignOptions::default(),
        );
    }

    /// Returns `true` if this layout would be the result for `max_advance`.
    ///
    /// For example:
    ///
    /// `compute_layout(max_advance == 10) => layout.width == 8`
    /// is also valid for `max_advance == 9`.
    ///
    /// This check assumes that Parley does greedy line-breaking,
    /// which it does at the time of writing this.
    fn satisfies(&self, max_advance: Option<f32>) -> bool {
        // Check if the specified constraint is compatible with this layout's constraint.
        self.max_advance.is_none_or(|layout_max_advance| {
            max_advance.is_some_and(|max_advance| {
                layout_max_advance - max_advance + Self::EPSILON >= 0.
            })
        }) &&
        // Check if the computed layout fits into the specified constraint.
        max_advance.is_none_or(|max_advance| {
            max_advance - self.layout.width() + Self::EPSILON >= 0.
        })
    }

    /// Returns `true` if this layout was more constrained than `max_advance`.
    fn more_constrained(&self, max_advance: Option<f32>) -> bool {
        self.max_advance.is_some_and(|layout_max_advance| {
            max_advance.is_none_or(|max_advance| layout_max_advance < max_advance)
        })
    }

    /// Returns `true` if the layouts are equal.
    ///
    /// That is if they have the same number of line breaks with the same reason at the same places.
    fn equals(&self, other: &Self) -> bool {
        if self.scale != other.scale || self.layout.len() != other.layout.len() {
            return false;
        }
        let mut a = self.layout.lines();
        let mut b = other.layout.lines();
        loop {
            match (a.next(), b.next()) {
                (None, None) => return true,
                (Some(a_line), Some(b_line)) => {
                    if a_line.break_reason() != b_line.break_reason() {
                        return false;
                    }
                    if a_line.text_range() != b_line.text_range() {
                        return false;
                    }
                    let boxes = |item| match item {
                        crate::reel_text::PositionedLayoutItem::InlineBox(b) => Some(b.id),
                        _ => None,
                    };
                    if !a_line
                        .items()
                        .filter_map(boxes)
                        .eq(b_line.items().filter_map(boxes))
                    {
                        return false;
                    }
                }
                _ => return false,
            }
        }
    }
}

// --- MARK: BUILDERS
impl Label {
    /// Creates a new label with the given text.
    ///
    // This is written out fully to appease rust-analyzer; StyleProperty is imported but not recognised.
    /// To change the font size, use `with_style`, setting [`StyleProperty::FontSize`](crate::reel_text::StyleProperty::FontSize).
    pub fn new(text: impl Into<ArcStr>) -> Self {
        let mut styles = StyleSet::new(theme::TEXT_SIZE_NORMAL);
        default_text_styles(&mut styles);
        Self {
            layouts: Vec::new(),
            cache_time: 0,
            active_layout: usize::MAX,
            text: Text::from(text.into()),
            visible_bytes: None,
            reveal_fade: None,
            fade_graphemes: Vec::new(),
            paint_viewport: None,
            paint_line_range: 0..0,
            styles,
            text_alignment: TextAlign::Start,
            text_fit: TextFit::None,
            outline: None,
            shadow: None,
            pressed_link: None,
            hovered_link: None,
            focused_link: 0,
            link_button: Default::default(),
            hint: true,
            accessibility_hidden: false,
            accessibility: LayoutAccessibility::default(),
        }
    }

    /// Paint only complete shaping clusters inside this UTF-8 range while laying
    /// out the complete text.
    ///
    /// # Panics
    /// Panics when the range is reversed, exceeds the text, or is not on UTF-8 boundaries.
    pub fn with_visible_bytes(mut self, range: std::ops::Range<usize>) -> Self {
        assert!(
            range.start <= range.end
                && range.end <= self.text.len()
                && self.text.is_char_boundary(range.start)
                && self.text.is_char_boundary(range.end)
        );
        self.visible_bytes = Some(range);
        self
    }

    pub fn with_rich_text(mut self, text: Text) -> Self {
        self.text = text;
        self.index_fade_graphemes();
        self
    }

    /// Set a scalar opacity wavefront while preserving the shaped text layout.
    pub fn with_reveal_fade(mut self, fade: Option<crate::core::RevealFade>) -> Self {
        self.reveal_fade = fade;
        self.index_fade_graphemes();
        self
    }
    fn index_fade_graphemes(&mut self) {
        if self.reveal_fade.is_none() {
            self.fade_graphemes = Vec::new();
            return;
        }
        self.fade_graphemes.clear();
        if self.reveal_fade.is_some() {
            self.fade_graphemes
                .extend(self.text.grapheme_indices(true).map(|(i, _)| i));
        }
    }
    /// Update only painting and input eligibility; no shaping/layout work.
    pub fn set_reveal_fade(this: &mut WidgetMut<'_, Self>, fade: Option<crate::core::RevealFade>) {
        if this.widget.reveal_fade != fade {
            let was_enabled = this.widget.reveal_fade.is_some();
            this.widget.reveal_fade = fade;
            if !was_enabled || fade.is_none() {
                this.widget.index_fade_graphemes();
            }
            this.ctx.request_paint_only();
            this.ctx.request_accessibility_update();
        }
    }

    /// Sets a style property for the new label.
    ///
    /// Setting [`StyleProperty::Brush`](crate::reel_text::StyleProperty::Brush) is not supported.
    /// Use the [`ContentColor`] property instead.
    ///
    /// To set a style property on an active label, use [`insert_style`](Self::insert_style).
    pub fn with_style(mut self, property: impl Into<StyleProperty>) -> Self {
        self.insert_style_inner(property.into());
        self
    }

    /// Sets a style property for the new label, returning the old value.
    ///
    /// Most users should prefer [`with_style`](Self::with_style) instead.
    pub fn try_with_style(
        mut self,
        property: impl Into<StyleProperty>,
    ) -> (Self, Option<StyleProperty>) {
        let old = self.insert_style_inner(property.into());
        (self, old)
    }

    /// Sets the alignment of the text.
    ///
    /// Text alignment might have unexpected results when the label has no horizontal constraints.
    /// To modify this on an active label, use [`set_text_alignment`](Self::set_text_alignment).
    pub fn with_text_alignment(mut self, text_alignment: TextAlign) -> Self {
        self.text_alignment = text_alignment;
        self
    }

    pub fn with_text_fit(mut self, fit: TextFit) -> Self {
        self.text_fit = fit;
        self
    }
    /// Paint one local-offset shadow using the same shaped paragraph and glyphs.
    pub fn with_shadow(mut self, shadow: Option<TextShadow>) -> Self {
        self.shadow = shadow;
        self
    }

    pub fn with_outline(mut self, outline: Option<TextOutline>) -> Self {
        self.outline = outline;
        self
    }

    /// Sets whether [hinting](https://en.wikipedia.org/wiki/Font_hinting) will be used for this label.
    ///
    /// Hinting is a process where text is drawn "snapped" to pixel boundaries to improve fidelity.
    /// The default is true, i.e. hinting is enabled by default.
    ///
    /// This should be set to false if the label will be animated at creation.
    /// The kinds of relevant animations include changing variable font parameters,
    /// translating or scaling.
    /// Failing to do so will likely lead to an unpleasant shimmering effect, as different parts of the
    /// text "snap" at different times.
    ///
    /// To modify this on an active label, use [`set_hint`](Self::set_hint).
    // TODO: Should we tell each widget if smooth scrolling is ongoing so they can disable their hinting?
    // Alternatively, we should automate disabling hinting at the Vello layer when composing.
    pub fn with_hint(mut self, hint: bool) -> Self {
        self.hint = hint;
        self
    }

    /// Sets whether this label is hidden from the accessibility tree.
    pub fn accessibility_hidden(mut self, hidden: bool) -> Self {
        self.accessibility_hidden = hidden;
        self
    }

    /// Shared logic between `with_style` and `insert_style`
    fn insert_style_inner(&mut self, property: StyleProperty) -> Option<StyleProperty> {
        if let StyleProperty::Brush(idx @ BrushIndex(1..))
        | StyleProperty::UnderlineBrush(Some(idx @ BrushIndex(1..)))
        | StyleProperty::StrikethroughBrush(Some(idx @ BrushIndex(1..))) = &property
        {
            debug_panic!(
                "Can't set a non-zero brush index ({idx:?}) on a `Label`, as it only supports global styling."
            );
        }
        self.styles.insert(property)
    }
}

// --- MARK: METHODS
impl Label {
    /// Returns a reference to the current text of this label.
    ///
    /// To update the text of an active label, use [`set_text`](Self::set_text).
    pub fn text(&self) -> &ArcStr {
        self.text.plain()
    }

    /// Returns the lower endpoint of the complete text's final insertion caret
    /// in label coordinates, using the active layout. Empty or unlaid text has no endpoint.
    pub fn text_end(&self) -> Option<Point> {
        self.layouts.get(self.active_layout)?.layout.text_end()
    }
}

// --- MARK: WIDGETMUT
impl Label {
    // Note: These docs are lazy, but also have a decreased likelihood of going out of date.
    /// The runtime equivalent of [`with_style`](Self::with_style).
    ///
    /// Setting [`StyleProperty::Brush`](crate::reel_text::StyleProperty::Brush) is not supported.
    /// Use the [`ContentColor`] property instead.
    pub fn insert_style(
        this: &mut WidgetMut<'_, Self>,
        property: impl Into<StyleProperty>,
    ) -> Option<StyleProperty> {
        let old = this.widget.insert_style_inner(property.into());

        this.widget.clear_cache();
        this.ctx.request_layout();
        old
    }

    /// Keeps only the styles for which `f` returns true.
    ///
    /// Styles which are removed return to Parley's default values.
    /// In most cases, these are the defaults for this widget.
    ///
    /// Of note, behaviour is unspecified for unsetting the [`FontSize`](crate::reel_text::StyleProperty::FontSize).
    pub fn retain_styles(this: &mut WidgetMut<'_, Self>, f: impl FnMut(&StyleProperty) -> bool) {
        this.widget.styles.retain(f);

        this.widget.clear_cache();
        this.ctx.request_layout();
    }

    /// Removes the style with the discriminant `property`.
    ///
    /// To get the discriminant requires constructing a valid `StyleProperty` for the
    /// the desired property and passing it to [`core::mem::discriminant`].
    /// Getting this discriminant is usually possible in a `const` context.
    ///
    /// Styles which are removed return to Parley's default values.
    /// In most cases, these are the defaults for this widget.
    ///
    /// Of note, behaviour is unspecified for unsetting the [`FontSize`](crate::reel_text::StyleProperty::FontSize).
    pub fn remove_style(
        this: &mut WidgetMut<'_, Self>,
        property: Discriminant<StyleProperty>,
    ) -> Option<StyleProperty> {
        let old = this.widget.styles.remove(property);

        this.widget.clear_cache();
        this.ctx.request_layout();
        old
    }

    /// Replaces the text of this widget.
    pub fn set_text(this: &mut WidgetMut<'_, Self>, new_text: impl Into<ArcStr>) {
        Self::set_rich_text(this, Text::from(new_text.into()));
    }

    pub fn set_rich_text(this: &mut WidgetMut<'_, Self>, text: Text) {
        this.ctx.set_interactive(text.has_links(), text.has_links());
        this.widget.text = text;
        this.widget.pressed_link = None;
        this.widget.hovered_link = None;
        this.widget.focused_link = 0;
        this.widget.link_button = Default::default();
        this.widget.visible_bytes = None;
        this.widget.index_fade_graphemes();

        this.widget.clear_cache();
        this.ctx.request_layout();
    }

    /// Update reveal without clearing the full-text layout cache or requesting layout.
    ///
    /// # Panics
    /// Panics when the range is reversed, exceeds the text, or is not on UTF-8 boundaries.
    pub fn set_visible_bytes(this: &mut WidgetMut<'_, Self>, range: std::ops::Range<usize>) {
        assert!(
            range.start <= range.end
                && range.end <= this.widget.text.len()
                && this.widget.text.is_char_boundary(range.start)
                && this.widget.text.is_char_boundary(range.end)
        );
        if this.widget.visible_bytes.as_ref() != Some(&range) {
            this.widget.visible_bytes = Some(range);
            this.ctx.request_paint_only();
            this.ctx.request_accessibility_update();
        }
    }

    /// The runtime equivalent of [`with_text_alignment`](Self::with_text_alignment).
    pub fn set_text_alignment(this: &mut WidgetMut<'_, Self>, text_alignment: TextAlign) {
        this.widget.text_alignment = text_alignment;
        this.ctx.request_layout();
    }

    pub fn set_text_fit(this: &mut WidgetMut<'_, Self>, fit: TextFit) {
        if this.widget.text_fit != fit {
            this.widget.text_fit = fit;
            this.widget.clear_cache();
            this.ctx.request_layout();
        }
    }

    pub fn set_outline(this: &mut WidgetMut<'_, Self>, outline: Option<TextOutline>) {
        if this.widget.outline != outline {
            let width = |value: Option<TextOutline>| value.map_or(0., |o| o.width);
            if width(this.widget.outline) != width(outline) {
                // Ink bounds change; the shaped paragraph remains cached.
                this.ctx.request_layout();
            } else {
                this.ctx.request_paint_only();
            }
            this.widget.outline = outline;
        }
    }
    /// Update shadow color or ink extent while preserving cached text layout.
    pub fn set_shadow(this: &mut WidgetMut<'_, Self>, shadow: Option<TextShadow>) {
        if this.widget.shadow != shadow {
            if this.widget.shadow.map(|s| s.offset) != shadow.map(|s| s.offset) {
                this.ctx.request_layout();
            } else {
                this.ctx.request_paint_only();
            }
            this.widget.shadow = shadow;
        }
    }

    /// The runtime equivalent of [`with_hint`](Self::with_hint).
    pub fn set_hint(this: &mut WidgetMut<'_, Self>, hint: bool) {
        this.widget.hint = hint;
        this.ctx.request_paint_only();
    }
}

impl Label {
    /// Clears the text layout cache.
    ///
    /// Call this whenever text, styles, or fonts have changed.
    fn clear_cache(&mut self) {
        self.layouts.clear();
        self.active_layout = usize::MAX;
    }

    /// Total number of text layouts to cache.
    ///
    /// Must be at least `2`, to allow for one active layout and one speculative one.
    /// Must be less than `u8::MAX` because it's also used as the cache time reset value.
    const CACHE_CAPACITY: usize = 5;

    /// Increments and returns the cache timestamp.
    fn cache_time(&mut self) -> u8 {
        if self.cache_time == u8::MAX {
            // Compress all last_used timestamps
            let n = self.layouts.len();
            let mut idx: SmallVec<[usize; Self::CACHE_CAPACITY]> = (0..n).collect();
            idx.sort_unstable_by_key(|&i| self.layouts[i].last_used);
            for (rank, &i) in idx.iter().enumerate() {
                self.layouts[i].last_used =
                    u8::try_from(rank).expect("layout cache has at most five entries");
            }
            self.cache_time =
                u8::try_from(Self::CACHE_CAPACITY).expect("layout cache capacity fits u8");
        } else {
            self.cache_time += 1;
        }
        self.cache_time
    }

    /// Builds the text layout and breaks the text into lines.
    ///
    /// Backed by a cache layer.
    fn build_and_break(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<BrushIndex>,
        max_advance: Option<f32>,
    ) -> usize {
        let TextFit::Shrink { min_font_size } = self.text_fit else {
            return self.build_scaled(font_ctx, layout_ctx, max_advance, 1.);
        };
        let natural = self.build_scaled(font_ctx, layout_ctx, None, 1.);
        let Some(width) = max_advance else {
            return natural;
        };
        let advance = self.layouts[natural].layout.width();
        if advance <= width || advance == 0. {
            return natural;
        }
        let font_size = self
            .styles
            .inner()
            .values()
            .find_map(|p| match p {
                StyleProperty::FontSize(size) => Some(*size),
                _ => None,
            })
            .unwrap_or(theme::TEXT_SIZE_NORMAL);
        let minimum = if font_size > 0. {
            (min_font_size / font_size).min(1.)
        } else {
            1.
        };
        let scale = (width / advance).max(minimum).clamp(0., 1.);
        self.build_scaled(font_ctx, layout_ctx, None, scale)
    }

    fn build_scaled(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<BrushIndex>,
        max_advance: Option<f32>,
        scale: f32,
    ) -> usize {
        let timestamp = self.cache_time();

        // Check if the cache already has a suitable entry.
        // A suitable entry is one that was calculated with the same or larger constraint,
        // and resulted in a layout that still fits within this newly requested constraint.
        for (idx, layout) in self.layouts.iter_mut().enumerate() {
            if layout.scale == scale && layout.satisfies(max_advance) {
                layout.last_used = timestamp;
                return idx;
            }
        }

        // No known compatible cache entry, so need to do text layout.
        let (mut idx, layout) = if self.layouts.len() < Self::CACHE_CAPACITY {
            // Create a new cache entry.
            self.layouts
                .push(TextLayout::new(max_advance, scale, timestamp));
            (self.layouts.len() - 1, self.layouts.last_mut().unwrap())
        } else {
            // Repurpose the least recently used non-active cache entry.
            let (idx, layout) = self
                .layouts
                .iter_mut()
                .enumerate()
                .filter(|(idx, _)| *idx != self.active_layout)
                .min_by(|a, b| a.1.last_used.cmp(&b.1.last_used))
                .unwrap();
            layout.max_advance = max_advance;
            layout.scale = scale;
            layout.last_used = timestamp;
            (idx, layout)
        };

        // TODO: Should we use a different scale?
        // See https://github.com/linebender/xilem/issues/1264
        layout.layout = Paragraph::build(
            font_ctx,
            layout_ctx,
            &self.text,
            &self.styles,
            max_advance,
            scale,
            self.text_fit == TextFit::None,
        );

        // Check if the layout result matches an existing cache entry.
        // This happens when slightly increasing max_advance, as we can't then safely pre-identify
        // an existing cache entry because more text might fit inside this new larger constraint.
        // However, if it actually resulted in the same layout as with a slightly lower constraint,
        // then we don't want to have two cache entries with the same identical layout result.
        if let Some((equal_idx, _)) = self
            .layouts
            .iter()
            .enumerate()
            // Only those that are more constrained than the new constraint are viable.
            .filter(|(_, layout)| layout.more_constrained(max_advance))
            // We want the one that is closest to the new constraint.
            .max_by(|a, b| {
                // Because we only look at more constrained entries,
                // these are all guaranteed to be Option::Some.
                a.1.max_advance
                    .unwrap()
                    .total_cmp(&b.1.max_advance.unwrap())
            })
            // Make sure that it actually matches the new layout.
            .filter(|(_, layout)| layout.equals(&self.layouts[idx]))
        {
            // Though these two layouts are equal, we want to keep the older one.
            // Because the old one might be the currently active layout.
            let equal_layout = &mut self.layouts[equal_idx];
            // Mark the old layout as applicable up to this new constraint.
            equal_layout.max_advance = max_advance;
            equal_layout.last_used = timestamp;

            // Discard the new layout that we just created as it is a duplicate.
            let layout = &mut self.layouts[idx];
            layout.discard();

            // Return the updated old entry.
            idx = equal_idx;
        }

        idx
    }
}

impl UsesProperty<ContentColor> for Label {}
impl UsesProperty<LineBreaking> for Label {}

#[derive(Clone, Debug, PartialEq)]
pub struct LinkActivated {
    pub id: ArcStr,
    pub text: Text,
}
impl Label {
    fn link_at(&self, point: Point) -> Option<usize> {
        let offset = self
            .layouts
            .get(self.active_layout)?
            .layout
            .hit(point.x as f32, point.y as f32)?;
        self.text
            .links()
            .iter()
            .enumerate()
            .find_map(|(index, link)| {
                (link.range.contains(&offset)
                    && self.link_visible(index)
                    && self
                        .visible_bytes
                        .as_ref()
                        .is_none_or(|r| r.start <= link.range.start && link.range.end <= r.end))
                .then_some(index)
            })
    }
    fn link_action(&self, index: usize) -> Option<LinkActivated> {
        let link = self.text.links().get(index)?;
        self.link_visible(index).then(|| LinkActivated {
            id: link.id.clone(),
            text: self.text.clone(),
        })
    }
    fn link_visible(&self, index: usize) -> bool {
        self.text.links().get(index).is_some_and(|link| {
            if self
                .reveal_fade
                .is_some_and(|fade| fade.alpha(&self.fade_graphemes, link.range.clone()) <= 0.)
            {
                return false;
            }
            self.visible_bytes
                .as_ref()
                .is_none_or(|r| r.start <= link.range.start && link.range.end <= r.end)
        })
    }
}

// --- MARK: IMPL WIDGET
impl Widget for Label {
    type Action = LinkActivated;

    fn accepts_pointer_interaction(&self) -> bool {
        self.text.has_links()
    }
    fn accepts_focus(&self) -> bool {
        self.text.has_links()
    }

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if ctx.is_disabled() || !ctx.input_configuration_is_current() {
            self.pressed_link = None;
            return;
        }
        match event {
            PointerEvent::Move(e) => {
                let hovered = self.link_at(ctx.local_position(e.current.position));
                if self.hovered_link != hovered {
                    self.hovered_link = hovered;
                    ctx.request_render();
                }
            }
            PointerEvent::Down(e) if e.button.is_none_or(|b| b == PointerButton::Primary) => {
                self.pressed_link = self.link_at(ctx.local_position(e.state.position));
                if let Some(index) = self.pressed_link {
                    self.focused_link = index;
                    ctx.request_focus();
                    ctx.capture_pointer_for_click();
                    ctx.set_handled();
                }
            }
            PointerEvent::Up(e) if ctx.is_active() => {
                let pressed = self.pressed_link.take();
                let hit = self.link_at(ctx.local_position(e.state.position));
                if pressed.is_some()
                    && pressed == hit
                    && let Some(action) = self.link_action(hit.unwrap())
                {
                    ctx.submit_action::<LinkActivated>(action);
                }
                ctx.set_handled();
            }
            PointerEvent::Cancel(_) => self.pressed_link = None,
            _ => {}
        }
    }
    fn on_text_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &TextEvent,
    ) {
        use crate::core::keyboard::{Key, NamedKey};
        if ctx.is_disabled() || !ctx.is_focus_target() || !ctx.input_configuration_is_current() {
            return;
        }
        if !self.link_visible(self.focused_link) {
            let Some(index) = (0..self.text.links().len()).find(|&i| self.link_visible(i)) else {
                return; // Unrevealed links leave confirmation to the dialogue's reveal/advance path.
            };
            self.focused_link = index;
        }
        let direction = match event {
            TextEvent::Keyboard(e) if e.state.is_down() => match &e.key {
                Key::Named(NamedKey::ArrowRight) => 1,
                Key::Named(NamedKey::ArrowLeft) => -1,
                _ => 0,
            },
            TextEvent::Navigation(e) if e.phase == crate::input::ActionPhase::Pressed => {
                match e.action {
                    crate::input::NavigationAction::Right => 1,
                    crate::input::NavigationAction::Left => -1,
                    _ => 0,
                }
            }
            _ => 0,
        };
        if direction != 0 && !self.text.links().is_empty() {
            let len = self.text.links().len() as isize;
            self.focused_link = (1..=len)
                .map(|step| {
                    (self.focused_link as isize + direction * step).rem_euclid(len) as usize
                })
                .find(|&i| self.link_visible(i))
                .unwrap();
            ctx.set_handled();
            ctx.request_render();
        } else if self.link_button.text(ctx, event).is_some()
            && let Some(action) = self.link_action(self.focused_link)
        {
            ctx.submit_action::<LinkActivated>(action);
        }
    }

    fn register_children(&mut self, _ctx: &mut RegisterCtx<'_>) {}

    fn property_changed(&mut self, ctx: &mut UpdateCtx<'_>, property_type: TypeId) {
        LineBreaking::prop_changed(ctx, property_type);
        ContentColor::prop_changed(ctx, property_type);
    }

    fn update(&mut self, ctx: &mut UpdateCtx<'_>, _props: &mut PropertiesMut<'_>, event: &Update) {
        self.link_button.update(event);
        if (!ctx.is_hovered() || ctx.is_disabled()) && self.hovered_link.take().is_some() {
            ctx.request_render();
        }
        if matches!(
            event,
            Update::DisabledChanged(true)
                | Update::StashedChanged(true)
                | Update::WindowFocusChanged(false)
        ) {
            self.pressed_link = None;
        }
        match event {
            Update::FontsChanged => {
                self.clear_cache();
                ctx.request_layout();
            }
            Update::DisabledChanged(_) | Update::FocusChanged(_) => {
                ctx.request_render();
            }
            _ => {}
        }
    }

    fn measure(
        &mut self,
        ctx: &mut MeasureCtx<'_>,
        props: &PropertiesRef<'_>,
        axis: Axis,
        len_req: LenReq,
        cross_length: Option<Length>,
    ) -> Length {
        // Currently we only support the common horizontal-tb writing mode,
        // so we hardcode the assumption that inline axis is horizontal.
        let inline = Axis::Horizontal;

        let cache = ctx.property_cache();
        let line_break_mode = props.get::<LineBreaking>(cache);

        // Calculate the max advance for the inline axis, with None indicating unbounded.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "Parley layout bounds use f32 while Masonry geometry uses f64"
        )]
        let max_advance = match line_break_mode {
            LineBreaking::WordWrap => {
                if axis == inline {
                    // Inline axis measurement ignores cross_length as a performance optimization.
                    // The search complexity of dealing with it is just too prohibitive.
                    // This is a common optimization also present on the web.
                    match len_req {
                        // Zero space will get us the length of longest unbreakable word
                        LenReq::MinContent => Some(Length::ZERO),
                        // Unbounded space will get us the length of the unwrapped string
                        LenReq::MaxContent => None,
                        // Attempt to wrap according to the parent's request
                        LenReq::FitContent(space) => Some(space),
                    }
                } else {
                    // Block axis is dependent on the inline axis, so cross_length dominates.
                    // If there is no explicit cross_length present, we fall back to inline defaults.
                    match len_req {
                        // Fallback is inline axis MinContent
                        LenReq::MinContent => cross_length.or(Some(Length::ZERO)),
                        // Fallback is inline axis MaxContent, even for FitContent, because
                        // as we don't have the inline space bound we'll consider it unbounded.
                        LenReq::MaxContent | LenReq::FitContent(_) => cross_length,
                    }
                }
            }
            // If we're never wrapping, then there's no max advance.
            LineBreaking::Clip | LineBreaking::Overflow => None,
        }
        .map(|v| v.get() as f32);

        let max_advance = if matches!(self.text_fit, TextFit::Shrink { .. }) {
            if axis == inline {
                match len_req {
                    LenReq::MinContent => Some(0.),
                    LenReq::FitContent(space) => Some(space.get() as f32),
                    LenReq::MaxContent => None,
                }
            } else {
                cross_length.map(|length| length.get() as f32)
            }
        } else {
            max_advance
        };

        let (font_ctx, layout_ctx) = ctx.text_contexts();
        let layout_idx = self.build_and_break(font_ctx, layout_ctx, max_advance);
        let layout = &self.layouts[layout_idx];

        let length = if axis == inline {
            layout.layout.width() // Inline length
        } else {
            layout.layout.height() // Block length
        };

        length.px()
    }

    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, props: &PropertiesRef<'_>, size: Size) {
        // Currently we only support the common horizontal-tb writing mode,
        // so we hardcode the assumption that inline axis is horizontal.
        let inline = Axis::Horizontal;

        let cache = ctx.property_cache();
        let line_break_mode = props.get::<LineBreaking>(cache);

        #[expect(
            clippy::cast_possible_truncation,
            reason = "Parley layout bounds use f32 while Masonry geometry uses f64"
        )]
        let inline_space = size.get_coord(inline) as f32;

        let max_advance = match line_break_mode {
            LineBreaking::WordWrap => Some(inline_space),
            LineBreaking::Clip | LineBreaking::Overflow => None,
        };
        let max_advance = if matches!(self.text_fit, TextFit::Shrink { .. }) {
            Some(inline_space)
        } else {
            max_advance
        };

        let (font_ctx, layout_ctx) = ctx.text_contexts();
        self.active_layout = self.build_and_break(font_ctx, layout_ctx, max_advance);
        let layout = &mut self.layouts[self.active_layout];

        layout.align(self.text_alignment, inline_space);
        ctx.set_paint_insets(
            self.outline.map_or(0., |outline| outline.width)
                + self
                    .shadow
                    .map_or(0., |s| s.offset.x.abs().max(s.offset.y.abs())),
        );

        let line_count = layout.layout.len();
        if line_count > 0 {
            let line_first = layout.layout.get(0).unwrap();
            let line_last = layout.layout.get(line_count - 1).unwrap();
            let first_baseline = line_first.metrics().baseline as f64;
            let last_baseline = line_last.metrics().baseline as f64;
            ctx.set_baselines(first_baseline, last_baseline);
        } else {
            ctx.clear_baselines();
        }

        if *line_break_mode == LineBreaking::Clip {
            let border_box = size.to_rect() + ctx.border_box_insets();
            ctx.set_clip_path(border_box);
        } else {
            ctx.clear_clip_path();
        }
    }

    fn set_paint_viewport(&mut self, viewport: Option<Rect>) -> bool {
        self.paint_viewport = viewport;
        let mut range = self.layouts[self.active_layout].layout.visible_lines(
            viewport,
            Affine::IDENTITY,
            self.outline,
        );
        if let Some(shadow) = self.shadow.filter(|s| s.offset != crate::kurbo::Vec2::ZERO) {
            let shadow_range = self.layouts[self.active_layout].layout.visible_lines(
                viewport,
                Affine::translate(shadow.offset),
                self.outline,
            );
            range = range.start.min(shadow_range.start)..range.end.max(shadow_range.end);
        }
        let changed = self.paint_line_range != range;
        self.paint_line_range = range;
        changed
    }

    fn paint(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        props: &PropertiesRef<'_>,
        painter: &mut Painter<'_>,
    ) {
        let cache = ctx.property_cache();
        let text_color = props.get::<ContentColor>(cache);

        let layout = &mut self.layouts[self.active_layout];
        if let Some(shadow) = self.shadow.filter(|s| s.offset != crate::kurbo::Vec2::ZERO) {
            let brush = crate::peniko::Brush::Solid(shadow.color);
            layout.layout.paint_faded(
                painter,
                Affine::translate(shadow.offset),
                brush.clone(),
                self.hint,
                self.visible_bytes.clone().unwrap_or(0..self.text.len()),
                self.outline.map(|o| TextOutline {
                    width: o.width,
                    color: shadow.color,
                }),
                self.paint_viewport,
                self.reveal_fade,
                &self.fade_graphemes,
                1.,
                Some(&brush),
            );
        }
        layout.layout.paint_faded(
            painter,
            Affine::IDENTITY,
            text_color.color.into(),
            self.hint,
            self.visible_bytes.clone().unwrap_or(0..self.text.len()),
            self.outline,
            self.paint_viewport,
            self.reveal_fade,
            &self.fade_graphemes,
            1.,
            None,
        );
        if let Some(index) = self
            .hovered_link
            .or_else(|| ctx.is_focus_target().then_some(self.focused_link))
            && let Some(link) = self.text.links().get(index)
            && self
                .reveal_fade
                .is_none_or(|fade| fade.alpha(&self.fade_graphemes, link.range.clone()) > 0.)
            && self
                .visible_bytes
                .as_ref()
                .is_none_or(|r| r.start <= link.range.start && link.range.end <= r.end)
        {
            layout.layout.paint_focus(
                painter,
                Affine::IDENTITY,
                link.range.clone(),
                text_color.color.into(),
                self.paint_viewport.map(|rect| {
                    let margin = self.outline.map_or(0., |outline| outline.width);
                    rect.inflate(margin, margin)
                }),
            );
        }
    }

    fn accessibility_role(&self) -> Role {
        Role::Label
    }

    fn accessibility(
        &mut self,
        ctx: &mut AccessCtx<'_>,
        props: &PropertiesRef<'_>,
        node: &mut Node,
    ) {
        if self.accessibility_hidden {
            node.set_hidden();
            return;
        }

        if self.visible_bytes.is_some() || self.text.markup().is_some() {
            let range = self.visible_bytes.clone().unwrap_or(0..self.text.len());
            node.set_label(&self.text[range]);
            return;
        }

        let text_origin_in_border_box_space = Point::ORIGIN + ctx.border_box_translation();

        let cache = ctx.property_cache();
        let text_color = props.get::<ContentColor>(cache);

        let layout = &self.layouts[self.active_layout];

        self.accessibility.build_nodes(
            self.text.as_ref(),
            &layout.layout,
            ctx.tree_update(),
            node,
            AccessCtx::next_node_id,
            text_origin_in_border_box_space.x,
            text_origin_in_border_box_space.y,
            |node, style| set_accesskit_brush_properties(node, style, &[text_color.color.into()]),
        );
    }

    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::new()
    }

    fn make_trace_span(&self, id: WidgetId) -> Span {
        trace_span!("Label", id = id.trace())
    }

    fn get_debug_text(&self) -> Option<String> {
        Some(self.text.to_string())
    }
}

// --- MARK: TESTS

#[cfg(test)]
mod reveal_tests {
    use super::*;
    use crate::{
        app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
        core::{DefaultProperties, Widget},
        dpi::PhysicalSize,
        peniko::Blob,
    };
    use std::sync::Arc;

    #[test]
    fn reveal_fade_uses_unicode_graphemes_and_preserves_opaque_prefix() {
        let text = "Aé e\u{301}Z";
        let starts: Vec<_> = text.grapheme_indices(true).map(|(i, _)| i).collect();
        let fade = crate::core::RevealFade {
            from: 1,
            progress: 0.75,
            step: 0.25,
        };
        assert_eq!(fade.alpha(&starts, 0..1), 1.);
        assert_eq!(fade.alpha(&starts, 1..3), 0.75);
        let combined = text.find('e').unwrap();
        assert_eq!(fade.alpha(&starts, combined..combined + 3), 0.25);
        assert_eq!(fade.alpha(&starts, text.len() - 1..text.len()), 0.);
        assert_eq!(fade.alpha(&starts, 1..text.len()), 0.);
    }
    #[test]
    fn rich_text_and_fade_builder_order_share_graphemes_and_disabling_releases_index() {
        let fade = crate::core::RevealFade {
            from: 0,
            progress: 0.75,
            step: 0.25,
        };
        let source = Text::from("Aé e\u{301}Z");
        let first = Label::new("old")
            .with_reveal_fade(Some(fade))
            .with_rich_text(source.clone());
        let second = Label::new("old")
            .with_rich_text(source)
            .with_reveal_fade(Some(fade));
        assert_eq!(first.fade_graphemes, vec![0, 1, 3, 4, 7]);
        assert_eq!(first.fade_graphemes, second.fade_graphemes);
        let disabled = first.with_reveal_fade(None);
        assert_eq!(disabled.fade_graphemes.capacity(), 0);
    }
    #[test]
    #[ignore = "requires REEL_SESSION_FONT; CPU glyph primitives only"]
    fn reveal_fade_records_alpha_and_keeps_cached_layout_and_grapheme_index() {
        let font = Blob::from(
            std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
        );
        let label = Label::new("ABCDE")
            .with_reveal_fade(Some(crate::core::RevealFade {
                from: 0,
                progress: 0.75,
                step: 0.25,
            }))
            .prepare();
        let id = label.id();
        let mut root = RenderRoot::new(
            label,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(400, 100),
                scale_factor: 1.,
                default_font: Some(font),
            },
        );
        let (plan, _) = root.redraw();
        let mut opacity = Vec::new();
        for layer in &plan.layers {
            if let crate::app::VisualLayerKind::Scene(scene) = &layer.kind {
                for command in scene.commands() {
                    if let crate::imaging::record::Command::Draw(id) = command
                        && let crate::imaging::record::Draw::GlyphRun(run) = scene.draw_op(*id)
                    {
                        opacity.extend(std::iter::repeat_n(run.composite.alpha, run.glyphs.len()));
                    }
                }
            }
        }
        assert_eq!(opacity, [0.75, 0.5, 0.25]);
        let before = {
            let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
            (
                label.cache_time,
                label.active_layout,
                label.fade_graphemes.as_ptr(),
                label.layouts[label.active_layout].layout.width(),
                label.layouts[label.active_layout].layout.height(),
            )
        };
        for progress in [0., 0.2, 0.5, 1., 2.] {
            root.edit_widget(id, |mut widget| {
                Label::set_reveal_fade(
                    &mut widget.downcast::<Label>(),
                    Some(crate::core::RevealFade {
                        from: 0,
                        progress,
                        step: 0.25,
                    }),
                )
            });
            root.redraw();
            let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
            assert_eq!(
                (
                    label.cache_time,
                    label.active_layout,
                    label.fade_graphemes.as_ptr(),
                    label.layouts[label.active_layout].layout.width(),
                    label.layouts[label.active_layout].layout.height()
                ),
                before
            );
        }
        root.edit_widget(id, |mut label| {
            Label::set_reveal_fade(&mut label.downcast::<Label>(), None)
        });
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        assert_eq!(label.fade_graphemes.capacity(), 0);
    }

    #[test]
    #[ignore = "requires REEL_SESSION_FONT; CPU shadow and shared glyph layout only"]
    fn shadow_overrides_rich_color_and_reuses_faded_glyphs_and_layout() {
        let font = Blob::from(
            std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
        );
        let shadow_color = crate::peniko::Color::from_rgb8(13, 29, 71);
        let label = Label::new("")
            .with_rich_text(
                Text::parse("[color=#ff0000][u][ruby=xy]AB[/ruby]CD[/u][/color]").unwrap(),
            )
            .with_shadow(Some(TextShadow {
                offset: crate::kurbo::Vec2::new(2., 3.),
                color: shadow_color,
            }))
            .with_outline(Some(TextOutline {
                width: 1.,
                color: crate::peniko::Color::BLACK,
            }))
            .with_reveal_fade(Some(crate::core::RevealFade {
                from: 0,
                progress: 0.75,
                step: 0.25,
            }))
            .prepare();
        let id = label.id();
        let mut root = RenderRoot::new(
            label,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(400, 150),
                scale_factor: 1.,
                default_font: Some(font),
            },
        );
        let (plan, _) = root.redraw();
        let mut shadow = Vec::new();
        let mut foreground = Vec::new();
        for layer in &plan.layers {
            if let crate::app::VisualLayerKind::Scene(scene) = &layer.kind {
                for command in scene.commands() {
                    if let crate::imaging::record::Command::Draw(draw) = command
                        && let crate::imaging::record::Draw::GlyphRun(run) = scene.draw_op(*draw)
                    {
                        let key = (
                            run.glyphs.to_vec(),
                            run.font_size,
                            run.composite.alpha,
                            run.style.clone(),
                        );
                        if run.brush == crate::peniko::Brush::Solid(shadow_color) {
                            shadow.push((key, run.transform));
                        } else {
                            foreground.push((key, run.transform));
                        }
                    }
                }
            }
        }
        assert!(!shadow.is_empty());
        assert_eq!(shadow.len(), foreground.len());
        for ((a, at), (b, bt)) in shadow.iter().zip(&foreground) {
            assert_eq!(
                a, b,
                "shadow must share glyphs, outline and fade with foreground"
            );
            assert_eq!(*at, Affine::translate((2., 3.)) * *bt);
        }
        let before = {
            let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
            (label.cache_time, label.active_layout, label.layouts.len())
        };
        root.edit_widget(id, |mut label| {
            Label::set_shadow(
                &mut label.downcast::<Label>(),
                Some(TextShadow {
                    offset: crate::kurbo::Vec2::new(2., 3.),
                    color: crate::peniko::Color::WHITE,
                }),
            )
        });
        root.redraw();
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        assert_eq!(
            before,
            (label.cache_time, label.active_layout, label.layouts.len())
        );
    }

    #[test]
    #[ignore = "requires REEL_SESSION_FONT; CPU rich glyph primitives and hit geometry only"]
    fn reveal_fade_ruby_outline_decorations_and_link_visibility_share_alpha() {
        let font = Blob::from(
            std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
        );
        let source =
            Text::parse("[link=entry][u][s][em][ruby=XY]AB[/ruby]CD[/em][/s][/u][/link]").unwrap();
        let label = Label::new("")
            .with_rich_text(source)
            .with_style(StyleProperty::FontSize(24.))
            .with_outline(Some(TextOutline {
                width: 1.,
                color: crate::peniko::Color::BLACK,
            }))
            .with_reveal_fade(Some(crate::core::RevealFade {
                from: 0,
                progress: 0.75,
                step: 0.25,
            }))
            .prepare();
        let id = label.id();
        let mut root = RenderRoot::new(
            label,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(400, 150),
                scale_factor: 1.,
                default_font: Some(font),
            },
        );
        let (plan, _) = root.redraw();
        let mut ruby = 0;
        let mut outline = 0;
        let mut decorations = 0;
        for layer in &plan.layers {
            if let crate::app::VisualLayerKind::Scene(scene) = &layer.kind {
                for command in scene.commands() {
                    if let crate::imaging::record::Command::Draw(draw) = command {
                        match scene.draw_op(*draw) {
                            crate::imaging::record::Draw::GlyphRun(run) => {
                                assert!(run.composite.alpha > 0. && run.composite.alpha <= 0.75);
                                if run.font_size < 20. {
                                    assert_eq!(run.composite.alpha, 0.5);
                                    ruby += run.glyphs.len();
                                }
                                if matches!(run.style, crate::peniko::Style::Stroke(_)) {
                                    outline += run.glyphs.len();
                                }
                            }
                            crate::imaging::record::Draw::Fill { composite, .. } => {
                                assert!(composite.alpha > 0. && composite.alpha <= 0.75);
                                decorations += 1;
                            }
                            crate::imaging::record::Draw::Stroke { composite, .. } => {
                                assert!(composite.alpha > 0. && composite.alpha <= 0.75);
                                decorations += 1;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        assert!(ruby > 0 && outline > 0 && decorations > 0);
        assert!(
            !root
                .get_widget(id)
                .unwrap()
                .downcast::<Label>()
                .unwrap()
                .link_visible(0)
        );
        root.edit_widget(id, |mut widget| {
            Label::set_reveal_fade(
                &mut widget.downcast::<Label>(),
                Some(crate::core::RevealFade {
                    from: 0,
                    progress: 1.01,
                    step: 0.25,
                }),
            )
        });
        root.redraw();
        assert!(
            root.get_widget(id)
                .unwrap()
                .downcast::<Label>()
                .unwrap()
                .link_visible(0)
        );
        root.edit_widget(id, |mut widget| {
            Label::set_visible_bytes(&mut widget.downcast::<Label>(), 0..3)
        });
        root.redraw();
        assert!(
            !root
                .get_widget(id)
                .unwrap()
                .downcast::<Label>()
                .unwrap()
                .link_visible(0)
        );
    }

    #[test]
    #[ignore = "requires REEL_SESSION_FONT; CPU scene recording only"]
    fn moving_long_text_refreshes_visible_glyphs_without_rebuilding_layout() {
        let font = Blob::from(
            std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
        );
        let label = crate::core::NewWidget::new(Label::new("A line of history\n".repeat(2000)))
            .with_props(LineBreaking::WordWrap);
        let id = label.id();
        let mut root = RenderRoot::new(
            label,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(840, 280),
                scale_factor: 1.,
                default_font: Some(font),
            },
        );
        root.redraw();
        let snapshot = |root: &RenderRoot| {
            let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
            let layout = &label.layouts[label.active_layout].layout;
            (
                label.cache_time,
                label.active_layout,
                layout.visible_lines(label.paint_viewport, Affine::IDENTITY, None),
            )
        };
        let before = snapshot(&root);
        assert_eq!(before.2.start, 0);
        root.edit_widget(id, |mut widget| {
            widget.set_transform(Affine::translate((0., -10_000.)))
        });
        let (plan, _) = root.redraw();
        let after = snapshot(&root);
        assert_eq!(
            (before.0, before.1),
            (after.0, after.1),
            "scrolling must reuse shaping"
        );
        assert!(
            after.2.start > before.2.end,
            "newly exposed lines must replace cached paint"
        );
        let glyphs: usize = plan
            .layers
            .iter()
            .map(|layer| match &layer.kind {
                crate::app::VisualLayerKind::Scene(scene) => scene
                    .commands()
                    .iter()
                    .map(|command| match command {
                        crate::imaging::record::Command::Draw(id) => match scene.draw_op(*id) {
                            crate::imaging::record::Draw::GlyphRun(run) => run.glyphs.len(),
                            _ => 0,
                        },
                        _ => 0,
                    })
                    .sum(),
                _ => 0,
            })
            .sum();
        assert!(
            glyphs > 0 && glyphs < 1000,
            "frame should contain only visible history lines: {glyphs}"
        );
        root.edit_widget(id, |mut widget| {
            let label = widget.downcast::<Label>();
            let viewport = label.widget.paint_viewport.unwrap();
            assert!(
                !label
                    .widget
                    .set_paint_viewport(Some(viewport + crate::kurbo::Vec2::new(20., 0.))),
                "horizontal motion with the same visible lines must preserve cached paint"
            );
        });
    }

    #[test]
    #[ignore = "requires REEL_SESSION_FONT for shaping ruby and styled paragraphs"]
    fn styled_ruby_layout_reveal_and_link_hit_geometry_share_one_cached_paragraph() {
        let font = Blob::from(
            std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
        );
        let source = Text::parse("[link=entry][ruby=reading][b]Base[/b][/ruby][/link] [size=36]Large[/size] [em]ffi marked text[/em] across several lines").unwrap();
        let label = crate::core::NewWidget::new(
            Label::new("")
                .with_rich_text(source.clone())
                .with_style(StyleProperty::LineHeight(
                    crate::reel_text::LineHeight::FontSizeRelative(2.),
                ))
                .with_visible_bytes(0..0),
        )
        .with_props(LineBreaking::WordWrap);
        let id = label.id();
        let mut root = RenderRoot::new(
            label,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(210, 400),
                scale_factor: 1.,
                default_font: Some(font),
            },
        );
        root.redraw();
        let snapshot = |root: &RenderRoot| {
            let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
            let layout = &label.layouts[label.active_layout].layout;
            (
                label.cache_time,
                label.active_layout,
                layout.width(),
                layout.height(),
                layout
                    .lines()
                    .map(|l| (l.text_range(), l.metrics().baseline))
                    .collect::<Vec<_>>(),
            )
        };
        let before = snapshot(&root);
        assert!(before.4.len() > 1, "mixed-size paragraph should wrap");
        let (point, baseline) = {
            let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
            let layout = &label.layouts[label.active_layout].layout;
            let line = layout.get(0).unwrap();
            let p = line
                .items()
                .find_map(|i| match i {
                    crate::reel_text::PositionedLayoutItem::InlineBox(p) => Some(p),
                    _ => None,
                })
                .expect("ruby box");
            assert!(p.height > 24. && p.y >= 0. && p.y + p.height <= layout.height());
            let point = Point::new((p.x + p.width / 2.) as f64, (p.y + 2.) as f64);
            assert_eq!(
                label.link_at(point),
                None,
                "hidden text must not be clickable"
            );
            (point, line.metrics().baseline)
        };
        assert!(baseline > 24., "ruby reserves ascent");
        for end in [1, 3, 4, 10, source.len()] {
            root.edit_widget(id, |mut w| {
                Label::set_visible_bytes(&mut w.downcast::<Label>(), 0..end)
            });
            root.redraw();
            assert_eq!(
                snapshot(&root),
                before,
                "reveal must not reshape or change line breaks"
            );
            let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
            assert_eq!(label.link_at(point), (end >= 4).then_some(0));
        }
        root.edit_widget(id, |mut w| {
            Label::set_rich_text(
                &mut w.downcast::<Label>(),
                Text::parse("[ruby=only]base[/ruby]").unwrap(),
            )
        });
        root.redraw();
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        let layout = &label.layouts[label.active_layout].layout;
        assert!(
            layout.width() > 0. && layout.height() > 0.,
            "a ruby-only paragraph has real geometry"
        );

        let fitted = Text::parse("[link=entry][ruby=reading]Extraordinary[/ruby][/link] [size=36]possibilities[/size]\nSecond line").unwrap();
        root.edit_widget(id, |mut widget| {
            let mut label = widget.downcast::<Label>();
            Label::set_rich_text(&mut label, fitted.clone());
            Label::set_text_fit(&mut label, TextFit::Shrink { min_font_size: 0. });
            Label::set_visible_bytes(&mut label, 0..fitted.len());
        });
        root.redraw();
        let (fitted_layout, cache_len) = {
            let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
            let layout = &label.layouts[label.active_layout];
            assert!(layout.scale < 1.);
            assert!(layout.layout.width() <= 210.01, "{}", layout.layout.width());
            assert_eq!(
                layout.layout.len(),
                2,
                "only explicit newline breaks fitted text"
            );
            let ruby = layout
                .layout
                .get(0)
                .unwrap()
                .items()
                .find_map(|item| match item {
                    crate::reel_text::PositionedLayoutItem::InlineBox(p) => Some(p),
                    _ => None,
                })
                .unwrap();
            let point = Point::new((ruby.x + ruby.width / 2.) as f64, (ruby.y + 2.) as f64);
            assert_eq!(
                label.link_at(point),
                Some(0),
                "fitted ruby uses fitted link geometry"
            );
            (label.active_layout, label.layouts.len())
        };
        for end in [0, fitted.len()] {
            root.edit_widget(id, |mut widget| {
                let mut label = widget.downcast::<Label>();
                Label::set_visible_bytes(&mut label, 0..end);
                Label::set_outline(
                    &mut label,
                    Some(TextOutline {
                        width: 2.,
                        color: crate::peniko::Color::BLACK,
                    }),
                );
            });
            root.redraw();
            let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
            assert_eq!(label.active_layout, fitted_layout);
            assert_eq!(
                label.layouts.len(),
                cache_len,
                "outline/reveal reuse shaped layouts"
            );
        }
        root.edit_widget(id, |mut widget| {
            Label::set_text_fit(
                &mut widget.downcast::<Label>(),
                TextFit::Shrink { min_font_size: 24. },
            );
        });
        root.redraw();
        let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
        let layout = &label.layouts[label.active_layout];
        assert_eq!(
            layout.scale, 1.,
            "minimum size cannot enlarge the author's font"
        );
        assert!(
            layout.layout.width() > 210.,
            "minimum-size overflow is explicit"
        );
    }

    #[test]
    fn visible_ranges_keep_utf8_boundaries_bounds_and_empty_range_contract() {
        let text = "aلاe\u{301}";
        for range in [0..0, 1..1, 1..5, 3..5, text.len()..text.len()] {
            let label = Label::new(text).with_visible_bytes(range);
            assert_eq!(label.text().as_ref(), text);
        }
        for range in [
            std::ops::Range { start: 5, end: 1 },
            0..text.len() + 1,
            2..5,
            1..4,
        ] {
            assert!(
                std::panic::catch_unwind(|| Label::new(text).with_visible_bytes(range.clone()))
                    .is_err(),
                "invalid visible range must be rejected: {range:?}"
            );
        }
    }

    #[test]
    #[ignore = "requires REEL_SESSION_FONT for a real cached Parley layout"]
    fn reveal_changes_paint_without_rebuilding_full_text_layout() {
        let font = Blob::from(
            std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
        );
        let text = "office ffi لا ل\u{64e}ا e\u{301} stable line breaks for full text";
        let label = Label::new(text).with_visible_bytes(0..0).prepare();
        let id = label.id();
        let mut root = RenderRoot::new(
            label,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(200, 150),
                scale_factor: 1.,
                default_font: Some(font),
            },
        );
        root.redraw();
        let snapshot = |root: &RenderRoot| {
            let label = root.get_widget(id).unwrap().downcast::<Label>().unwrap();
            let active = label.active_layout;
            let layout = &label.layouts[active].layout;
            (
                label.layouts.len(),
                active,
                label.cache_time,
                layout.width(),
                layout.height(),
                layout
                    .lines()
                    .map(|line| line.text_range())
                    .collect::<Vec<_>>(),
            )
        };
        let before = snapshot(&root);
        let rtl_start = text.find("لا").unwrap();
        for range in [
            0..text.len(),
            rtl_start + 2..rtl_start + 4,
            rtl_start..rtl_start + 4,
            2..3,
            text.len()..text.len(),
        ] {
            root.edit_widget(id, |mut widget| {
                Label::set_visible_bytes(&mut widget.downcast::<Label>(), range)
            });
            root.redraw();
            assert_eq!(snapshot(&root), before);
        }
        assert_eq!(
            root.get_widget(id)
                .unwrap()
                .downcast::<Label>()
                .unwrap()
                .text()
                .as_ref(),
            text
        );
    }
}
