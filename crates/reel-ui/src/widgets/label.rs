// Copyright 2019 the Xilem Authors and the Druid Authors
// SPDX-License-Identifier: Apache-2.0

use std::any::TypeId;
use std::mem::Discriminant;

use accesskit::{Node, Role};
use reel_core::text::Text;
use smallvec::SmallVec;
use tracing::{Span, trace_span};
mod paragraph;
use paragraph::Paragraph;

use crate::core::{
    AccessCtx, ArcStr, BrushIndex, ChildrenIds, EventCtx, LayoutCtx, MeasureCtx, PaintCtx,
    PointerButton, PointerEvent, PropertiesMut, PropertiesRef, RegisterCtx, StyleProperty,
    StyleSet, TextEvent, TextFit, TextOutline, Update, UpdateCtx, UsesProperty, Widget, WidgetId,
    WidgetMut, set_accesskit_brush_properties,
};
use crate::imaging::Painter;
use crate::kurbo::{Affine, Axis, Point, Size};
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
#[doc = include_str!("../doc/widgets/label.md")]
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
    styles: StyleSet,
    text_alignment: TextAlign,
    text_fit: TextFit,
    outline: Option<TextOutline>,
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
            styles,
            text_alignment: TextAlign::Start,
            text_fit: TextFit::None,
            outline: None,
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
        self
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
        self.text.links().iter().position(|link| {
            link.range.contains(&offset)
                && self
                    .visible_bytes
                    .as_ref()
                    .is_none_or(|r| r.start <= link.range.start && link.range.end <= r.end)
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
        ctx.set_paint_insets(self.outline.map_or(0., |outline| outline.width));

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

    fn paint(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        props: &PropertiesRef<'_>,
        painter: &mut Painter<'_>,
    ) {
        let cache = ctx.property_cache();
        let text_color = props.get::<ContentColor>(cache);

        let layout = &mut self.layouts[self.active_layout];
        layout.layout.paint(
            painter,
            Affine::IDENTITY,
            text_color.color.into(),
            self.hint,
            self.visible_bytes.clone().unwrap_or(0..self.text.len()),
            self.outline,
        );
        if let Some(index) = self
            .hovered_link
            .or_else(|| ctx.is_focus_target().then_some(self.focused_link))
            && let Some(link) = self.text.links().get(index)
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
