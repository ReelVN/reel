// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Text editing using the shared Parley editor, scroll geometry and platform IME events.
use std::{borrow::Cow, sync::Arc};

use crate::{
    TextAlign,
    accesskit::{Action, ActionData, Node, Role},
    core::{
        keyboard::{Key, NamedKey},
        *,
    },
    imaging::Painter,
    kurbo::{Affine, Axis, Point, Rect, Size, Vec2},
    layout::{LenReq, Length},
    peniko::Color,
    properties::ContentColor,
    reel_text::{TextWrapMode, editing::PlainEditor},
    widgets::{ScrollAxis, ScrollState},
};

#[derive(Clone, Debug, PartialEq)]
pub enum TextInputEvent {
    Changed { previous: Arc<str>, value: Arc<str> },
    Submitted(Arc<str>),
}

/// A plain-text editor sharing its layout between text, selection, caret and IME geometry.
///
/// Single-line mode never wraps. Multiline mode preserves explicit newlines and supports
/// optional soft wrapping. Editing state remains in the same editor when formatting changes.
pub struct TextInput {
    editor: PlainEditor<BrushIndex>,
    value: Arc<str>,
    composition: Option<(Arc<str>, usize, usize)>,
    multiline: bool,
    text_wrap: bool,
    text_alignment: TextAlign,
    layout_width: Option<f32>,
    geometry_dirty: bool,
    scroll: ScrollState,
    text_x: f64,
    text_y: f64,
    reveal_caret: bool,
}

fn input_text(text: &str, multiline: bool) -> Cow<'_, str> {
    if multiline {
        if text.contains('\r') {
            Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
        } else {
            Cow::Borrowed(text)
        }
    } else if text.contains(['\r', '\n']) {
        Cow::Owned(text.replace("\r\n", " ").replace(['\r', '\n'], " "))
    } else {
        Cow::Borrowed(text)
    }
}

fn rect(bounds: crate::reel_text::BoundingBox) -> Rect {
    Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1)
}

impl TextInput {
    pub fn new(value: impl Into<Arc<str>>) -> Self {
        Self::with_mode(value, false)
    }

    pub fn with_mode(value: impl Into<Arc<str>>, multiline: bool) -> Self {
        let value = value.into();
        let value = match if multiline {
            Cow::Borrowed(value.as_ref())
        } else {
            input_text(&value, false)
        } {
            Cow::Owned(text) => text.into(),
            Cow::Borrowed(_) => value,
        };
        let mut editor = PlainEditor::new(24.);
        editor.set_text(&value);
        editor
            .edit_styles()
            .insert(StyleProperty::TextWrapMode(if multiline {
                TextWrapMode::Wrap
            } else {
                TextWrapMode::NoWrap
            }));
        Self {
            editor,
            value,
            composition: None,
            multiline,
            text_wrap: true,
            text_alignment: TextAlign::Start,
            layout_width: None,
            geometry_dirty: true,
            scroll: ScrollState::default(),
            text_x: 0.,
            text_y: 0.,
            reveal_caret: true,
        }
    }

    /// Sets a whole-editor style, including font-size-relative or absolute line height.
    /// `TextWrapMode` follows [`with_text_wrap`](Self::with_text_wrap), keeping single-line
    /// mode unwrapped.
    pub fn with_style(mut self, style: StyleProperty) -> Self {
        if let StyleProperty::TextWrapMode(mode) = style {
            return self.with_text_wrap(mode == TextWrapMode::Wrap);
        }
        self.editor.edit_styles().insert(style);
        self.geometry_dirty = true;
        self
    }

    /// Aligns text within the input's content width without changing its value or selection.
    ///
    /// Alignment uses the paragraph direction for `Start` and `End`. A single-line input
    /// remains unwrapped, including when its text overflows the available width.
    pub fn with_text_alignment(mut self, text_alignment: TextAlign) -> Self {
        self.align_text(text_alignment);
        self
    }

    /// Enables soft wrapping for a multiline input. The default is `true`.
    ///
    /// Disabling wrapping preserves explicit newlines and allows horizontal scrolling.
    /// Single-line inputs never wrap; this setting is retained if the mode changes later.
    pub fn with_text_wrap(mut self, text_wrap: bool) -> Self {
        self.text_wrap = text_wrap;
        self.update_wrap_mode();
        self
    }

    /// Updates alignment while retaining text, selection and an active IME composition.
    /// Repeating the current value preserves the cached layout.
    pub fn set_text_alignment(this: &mut WidgetMut<'_, Self>, text_alignment: TextAlign) {
        if this.widget.align_text(text_alignment) {
            this.ctx.request_layout();
        }
    }

    /// Updates multiline soft wrapping while retaining text, selection and IME composition.
    /// Single-line inputs retain their unwrapped layout.
    pub fn set_text_wrap(this: &mut WidgetMut<'_, Self>, text_wrap: bool) {
        if this.widget.text_wrap == text_wrap {
            return;
        }
        this.widget.text_wrap = text_wrap;
        if this.widget.multiline {
            this.widget.update_wrap_mode();
            this.ctx.request_layout();
        }
    }

    pub fn set_multiline(this: &mut WidgetMut<'_, Self>, multiline: bool) {
        if this.widget.multiline == multiline {
            return;
        }
        let composing = this.widget.composition.is_some();
        let (fonts, layout) = this.ctx.text_contexts();
        this.widget.cancel_compose(fonts, layout);
        this.widget.multiline = multiline;
        this.widget.update_wrap_mode();
        this.widget.reveal_caret = true;
        if composing {
            this.ctx.reset_ime();
        }
        this.ctx.request_layout();
    }

    /// Authoritative model updates preserve selection when they echo the current value.
    pub fn set_value(this: &mut WidgetMut<'_, Self>, value: Arc<str>) {
        let value = match if this.widget.multiline {
            Cow::Borrowed(value.as_ref())
        } else {
            input_text(&value, false)
        } {
            Cow::Owned(text) => text.into(),
            Cow::Borrowed(_) => value,
        };
        if this.widget.value == value {
            return;
        }
        let composing = this.widget.composition.is_some();
        let (fonts, layout) = this.ctx.text_contexts();
        this.widget.cancel_compose(fonts, layout);
        this.widget.editor.set_text(&value);
        this.widget.geometry_dirty = true;
        this.widget.value = value;
        this.widget.reveal_caret = true;
        if composing {
            this.ctx.reset_ime();
        }
        this.ctx.request_layout();
    }

    /// Updates a whole-editor style without replacing text, selection or IME composition.
    /// `TextWrapMode` follows [`set_text_wrap`](Self::set_text_wrap).
    pub fn insert_style(this: &mut WidgetMut<'_, Self>, style: StyleProperty) {
        if let StyleProperty::TextWrapMode(mode) = style {
            Self::set_text_wrap(this, mode == TextWrapMode::Wrap);
            return;
        }
        this.widget.editor.edit_styles().insert(style);
        this.widget.geometry_dirty = true;
        this.widget.reveal_caret = true;
        this.ctx.request_layout();
    }

    fn align_text(&mut self, text_alignment: TextAlign) -> bool {
        if self.text_alignment == text_alignment {
            return false;
        }
        self.text_alignment = text_alignment;
        self.editor.set_alignment(text_alignment);
        self.geometry_dirty = true;
        self.reveal_caret = true;
        true
    }

    fn update_wrap_mode(&mut self) {
        self.editor
            .edit_styles()
            .insert(StyleProperty::TextWrapMode(
                if self.multiline && self.text_wrap {
                    TextWrapMode::Wrap
                } else {
                    TextWrapMode::NoWrap
                },
            ));
        self.geometry_dirty = true;
        self.reveal_caret = true;
    }

    fn layout_at(&mut self, width: Option<f32>) {
        if self.layout_width != width {
            self.layout_width = width;
            self.editor.set_width(width);
            self.geometry_dirty = true;
            self.reveal_caret = true;
        }
    }

    fn cancel_compose(
        &mut self,
        fonts: &mut crate::reel_text::FontContext,
        layout: &mut crate::reel_text::LayoutContext<BrushIndex>,
    ) {
        if let Some((value, anchor, focus)) = self.composition.take() {
            self.editor.driver(fonts, layout).clear_compose();
            self.editor.set_text(&value);
            self.editor
                .driver(fonts, layout)
                .select_byte_range(anchor, focus);
            self.geometry_dirty = true;
            self.reveal_caret = true;
        }
    }

    fn publish(&mut self, ctx: &mut EventCtx<'_>) {
        if self.editor.raw_text() != self.value.as_ref() {
            let value: Arc<str> = self.editor.raw_text().into();
            let previous = std::mem::replace(&mut self.value, value.clone());
            ctx.submit_action::<TextInputEvent>(TextInputEvent::Changed { previous, value });
        }
    }

    fn update_geometry(&mut self, viewport: Size) {
        let mut content = self.scroll.content();
        if self.geometry_dirty
            && let Some(layout) = self.editor.try_layout()
        {
            // RTL overflow can extend left of the alignment width. Normalize layout
            // coordinates once so ScrollState can retain its nonnegative offsets.
            let (mut left, mut right) = (0_f64, 0_f64);
            for line in layout.lines() {
                let metrics = line.metrics();
                let start = f64::from(metrics.offset);
                left = left.min(start);
                right = right.max(start + f64::from(metrics.advance));
            }
            self.text_x = -left;
            content = Size::new(right - left + 1., f64::from(layout.height()));
            self.geometry_dirty = false;
        }
        self.text_y = if self.multiline {
            0.
        } else {
            ((viewport.height - content.height) * 0.5).max(0.)
        };
        self.scroll
            .layout(viewport, content, ScrollAxis::Both, false);
    }

    fn keep_caret_visible(&mut self) {
        self.update_geometry(self.scroll.viewport());
        if let Some(caret) = self.editor.cursor_geometry(1.) {
            self.scroll.reveal(rect(caret) + self.translation());
        }
    }

    fn ime_area(&self) -> Rect {
        let area = rect(self.editor.ime_cursor_area()) + self.translation();
        let viewport = self.scroll.viewport();
        Rect::new(
            area.x0.clamp(0., viewport.width),
            area.y0.clamp(0., viewport.height),
            area.x1.clamp(0., viewport.width),
            area.y1.clamp(0., viewport.height),
        )
    }

    fn translation(&self) -> Vec2 {
        Vec2::new(self.text_x, self.text_y) - self.scroll.offset()
    }
}

impl UsesProperty<ContentColor> for TextInput {}

impl Widget for TextInput {
    type Action = TextInputEvent;
    fn accepts_focus(&self) -> bool {
        true
    }
    fn accepts_text_input(&self) -> bool {
        true
    }
    fn register_children(&mut self, _: &mut RegisterCtx<'_>) {}
    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::new()
    }

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if ctx.is_disabled() || !ctx.input_configuration_is_current() {
            return;
        }
        if matches!(event, PointerEvent::Scroll(_)) {
            if self.scroll.pointer(ctx, event) {
                ctx.set_ime_area(self.ime_area());
                ctx.request_render();
            }
            return;
        }
        let (position, extend) = match event {
            PointerEvent::Down(e) if e.button.is_none_or(|b| b == PointerButton::Primary) => {
                ctx.request_focus();
                ctx.capture_pointer();
                (e.state.position, false)
            }
            PointerEvent::Move(e) if ctx.is_active() => (e.current.position, true),
            PointerEvent::Up(_) if ctx.is_active() => {
                ctx.set_handled();
                return;
            }
            _ => return,
        };
        let point = ctx.local_position(position) - self.translation();
        let composing = self.composition.is_some();
        let (fonts, layout) = ctx.text_contexts();
        self.cancel_compose(fonts, layout);
        let mut driver = self.editor.driver(fonts, layout);
        if extend {
            driver.extend_selection_to_point(point.x as f32, point.y as f32);
        } else {
            driver.move_to_point(point.x as f32, point.y as f32);
        }
        self.keep_caret_visible();
        if composing {
            ctx.reset_ime();
        }
        ctx.set_ime_area(self.ime_area());
        ctx.set_handled();
        ctx.request_render();
    }

    fn on_text_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &TextEvent,
    ) {
        if ctx.is_disabled() || !ctx.is_focus_target() || !ctx.input_configuration_is_current() {
            return;
        }
        let mut changed = false;
        match event {
            TextEvent::Ime(Ime::Preedit(text, cursor)) => {
                if text.is_empty() {
                    let (fonts, layout) = ctx.text_contexts();
                    self.cancel_compose(fonts, layout);
                    ctx.request_layout();
                    ctx.set_handled();
                    return;
                }
                if self.composition.is_none() {
                    let selection = self.editor.raw_selection();
                    self.composition = Some((
                        self.value.clone(),
                        selection.anchor().index(),
                        selection.focus().index(),
                    ));
                }
                let (fonts, layout) = ctx.text_contexts();
                let mut driver = self.editor.driver(fonts, layout);
                driver.set_compose(text, *cursor);
                self.geometry_dirty = true;
            }
            TextEvent::Ime(Ime::Commit(text)) => {
                let (fonts, layout) = ctx.text_contexts();
                let mut driver = self.editor.driver(fonts, layout);
                driver.clear_compose();
                driver.insert_or_replace_selection(&input_text(text, self.multiline));
                self.composition = None;
                changed = true;
            }
            TextEvent::Ime(Ime::Disabled) => {
                let (fonts, layout) = ctx.text_contexts();
                self.cancel_compose(fonts, layout);
            }
            TextEvent::ClipboardPaste(text) => {
                let composing = self.composition.is_some();
                let (fonts, layout) = ctx.text_contexts();
                self.cancel_compose(fonts, layout);
                self.editor
                    .driver(fonts, layout)
                    .insert_or_replace_selection(&input_text(text, self.multiline));
                if composing {
                    ctx.reset_ime();
                }
                changed = true;
            }
            TextEvent::Keyboard(event) => {
                if event.state.is_up() {
                    return;
                }
                if self.composition.is_some() || event.is_composing {
                    ctx.set_handled();
                    return;
                }
                let alt_graph = event.modifiers.contains(keyboard::Modifiers::ALT_GRAPH)
                    || (!cfg!(target_os = "macos")
                        && event.modifiers.ctrl()
                        && event.modifiers.alt());
                let command = !alt_graph
                    && !event.modifiers.alt()
                    && if cfg!(target_os = "macos") {
                        event.modifiers.meta()
                    } else {
                        event.modifiers.ctrl()
                    };
                let word = if cfg!(target_os = "macos") {
                    event.modifiers.alt()
                } else {
                    command
                };
                let shift = event.modifiers.shift();
                if let Key::Character(key) = &event.key
                    && command
                    && (key.eq_ignore_ascii_case("c") || key.eq_ignore_ascii_case("x"))
                {
                    if let Some(text) = self.editor.selected_text() {
                        ctx.set_clipboard(text.to_owned());
                    }
                    if key.eq_ignore_ascii_case("x") {
                        let (fonts, layout) = ctx.text_contexts();
                        self.editor.driver(fonts, layout).delete_selection();
                        changed = true;
                    }
                } else if event.key == Key::Named(NamedKey::Enter) {
                    if self.multiline && !command {
                        let (fonts, layout) = ctx.text_contexts();
                        self.editor
                            .driver(fonts, layout)
                            .insert_or_replace_selection("\n");
                        changed = true;
                    } else if !event.repeat {
                        ctx.submit_action::<TextInputEvent>(TextInputEvent::Submitted(
                            self.value.clone(),
                        ));
                    }
                } else {
                    let (fonts, layout) = ctx.text_contexts();
                    let mut driver = self.editor.driver(fonts, layout);
                    match &event.key {
                        Key::Character(key) if command && key.eq_ignore_ascii_case("a") => {
                            driver.select_all()
                        }
                        // The host supplies ClipboardPaste after resolving the system clipboard.
                        Key::Character(key) if command && key.eq_ignore_ascii_case("v") => {}
                        Key::Character(key)
                            if !command
                                && (alt_graph
                                    || !event.modifiers.alt()
                                    || cfg!(target_os = "macos")) =>
                        {
                            driver.insert_or_replace_selection(&input_text(key, self.multiline));
                            changed = true;
                        }
                        Key::Named(NamedKey::Backspace) => {
                            if word {
                                driver.backdelete_word();
                            } else {
                                driver.backdelete();
                            }
                            changed = true;
                        }
                        Key::Named(NamedKey::Delete) => {
                            if word {
                                driver.delete_word();
                            } else {
                                driver.delete();
                            }
                            changed = true;
                        }
                        Key::Named(NamedKey::ArrowLeft) => match (word, shift) {
                            (true, true) => driver.select_word_left(),
                            (true, false) => driver.move_word_left(),
                            (false, true) => driver.select_left(),
                            (false, false) => driver.move_left(),
                        },
                        Key::Named(NamedKey::ArrowRight) => match (word, shift) {
                            (true, true) => driver.select_word_right(),
                            (true, false) => driver.move_word_right(),
                            (false, true) => driver.select_right(),
                            (false, false) => driver.move_right(),
                        },
                        Key::Named(NamedKey::Home) => {
                            if self.multiline && !command {
                                if shift {
                                    driver.select_to_line_start();
                                } else {
                                    driver.move_to_line_start();
                                }
                            } else if shift {
                                driver.select_to_text_start();
                            } else {
                                driver.move_to_text_start();
                            }
                        }
                        Key::Named(NamedKey::End) => {
                            if self.multiline && !command {
                                if shift {
                                    driver.select_to_line_end();
                                } else {
                                    driver.move_to_line_end();
                                }
                            } else if shift {
                                driver.select_to_text_end();
                            } else {
                                driver.move_to_text_end();
                            }
                        }
                        Key::Named(NamedKey::ArrowUp) if self.multiline => {
                            if shift {
                                driver.select_up();
                            } else {
                                driver.move_up();
                            }
                        }
                        Key::Named(NamedKey::ArrowDown) if self.multiline => {
                            if shift {
                                driver.select_down();
                            } else {
                                driver.move_down();
                            }
                        }
                        _ => return,
                    }
                }
            }
            _ => return,
        }
        if changed {
            self.geometry_dirty = true;
            self.publish(ctx);
        }
        self.keep_caret_visible();
        ctx.set_ime_area(self.ime_area());
        if changed || matches!(event, TextEvent::Ime(_) | TextEvent::ClipboardPaste(_)) {
            self.reveal_caret = true;
            ctx.request_layout();
        } else {
            ctx.request_render();
        }
        ctx.set_handled();
    }

    fn update(&mut self, ctx: &mut UpdateCtx<'_>, _: &mut PropertiesMut<'_>, event: &Update) {
        match event {
            Update::FocusChanged(false)
            | Update::WindowFocusChanged(false)
            | Update::DisabledChanged(true)
            | Update::StashedChanged(true) => {
                let (fonts, layout) = ctx.text_contexts();
                self.cancel_compose(fonts, layout);
                ctx.request_layout();
            }
            Update::FocusChanged(_) | Update::WindowFocusChanged(_) => ctx.request_render(),
            Update::FontsChanged => {
                self.editor.set_scale(1.);
                self.geometry_dirty = true;
                self.reveal_caret = true;
                ctx.request_layout();
            }
            _ => {}
        }
    }

    fn property_changed(&mut self, ctx: &mut UpdateCtx<'_>, property: std::any::TypeId) {
        ContentColor::prop_changed(ctx, property);
    }

    fn measure(
        &mut self,
        ctx: &mut MeasureCtx<'_>,
        _: &PropertiesRef<'_>,
        axis: Axis,
        request: LenReq,
        cross_length: Option<Length>,
    ) -> Length {
        if axis == Axis::Horizontal {
            return match request {
                LenReq::FitContent(space) => space,
                _ => Length::px(160.),
            };
        }
        self.layout_at(cross_length.map(|width| width.get() as f32));
        let (fonts, layout) = ctx.text_contexts();
        Length::px(f64::from(self.editor.layout(fonts, layout).height()))
    }

    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, size: Size) {
        self.layout_at(Some(size.width as f32));
        if self.scroll.viewport() != size {
            self.reveal_caret = true;
        }
        let (fonts, layout) = ctx.text_contexts();
        self.editor.refresh_layout(fonts, layout);
        self.update_geometry(size);
        if self.reveal_caret {
            self.keep_caret_visible();
            self.reveal_caret = false;
        }
        ctx.set_clip_path(size.to_rect());
        ctx.set_ime_area(self.ime_area());
    }

    fn paint(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        props: &PropertiesRef<'_>,
        painter: &mut Painter<'_>,
    ) {
        let color = props.get::<ContentColor>(ctx.property_cache()).color;
        let transform = Affine::translate(self.translation());
        if ctx.is_focus_target() {
            self.editor.selection_geometry_with(|bounds, _| {
                painter
                    .fill(rect(bounds), Color::from_rgba8(60, 120, 220, 95))
                    .transform(transform)
                    .draw();
            });
        }
        if let Some(layout) = self.editor.try_layout() {
            render_text(painter, transform, layout, &[color.into()], true, None);
        }
        if ctx.is_focus_target()
            && ctx.is_window_focused()
            && let Some(caret) = self.editor.cursor_geometry(1.)
        {
            painter.fill(rect(caret), color).transform(transform).draw();
        }
    }

    fn accessibility_role(&self) -> Role {
        if self.multiline {
            Role::MultilineTextInput
        } else {
            Role::TextInput
        }
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, node: &mut Node) {
        node.set_value(self.value.as_ref());
        node.add_action(Action::SetValue);
    }
    fn on_access_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &AccessEvent,
    ) {
        if ctx.is_disabled() || !ctx.input_configuration_is_current() {
            return;
        }
        if event.action == Action::SetValue
            && let Some(ActionData::Value(text)) = &event.data
        {
            let composing = self.composition.is_some();
            let (fonts, layout) = ctx.text_contexts();
            self.cancel_compose(fonts, layout);
            self.editor.set_text(&input_text(text, self.multiline));
            self.geometry_dirty = true;
            if composing {
                ctx.reset_ime();
            }
            self.publish(ctx);
            self.reveal_caret = true;
            ctx.request_layout();
            ctx.set_handled();
        }
    }
    fn get_cursor(&self, _: &QueryCtx<'_>, _: Point) -> CursorIcon {
        CursorIcon::Text
    }
}

#[cfg(test)]
mod tests;
