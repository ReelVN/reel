// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Text editing using the shared Parley editor, scroll geometry and platform IME events.
use std::{borrow::Cow, sync::Arc};

use crate::{
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
    reel_text::editing::PlainEditor,
    widgets::{ScrollAxis, ScrollState},
};

#[derive(Clone, Debug, PartialEq)]
pub enum TextInputEvent {
    Changed { previous: Arc<str>, value: Arc<str> },
    Submitted(Arc<str>),
}

pub struct TextInput {
    editor: PlainEditor<BrushIndex>,
    value: Arc<str>,
    composition: Option<(Arc<str>, usize, usize)>,
    multiline: bool,
    wrap_width: Option<f32>,
    scroll: ScrollState,
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
        Self {
            editor,
            value,
            composition: None,
            multiline,
            wrap_width: None,
            scroll: ScrollState::default(),
            text_y: 0.,
            reveal_caret: true,
        }
    }

    pub fn with_style(mut self, style: StyleProperty) -> Self {
        self.editor.edit_styles().insert(style);
        self
    }

    pub fn set_multiline(this: &mut WidgetMut<'_, Self>, multiline: bool) {
        if this.widget.multiline == multiline {
            return;
        }
        let composing = this.widget.composition.is_some();
        let (fonts, layout) = this.ctx.text_contexts();
        this.widget.cancel_compose(fonts, layout);
        this.widget.multiline = multiline;
        this.widget.wrap_at(None);
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
        this.widget.value = value;
        this.widget.reveal_caret = true;
        if composing {
            this.ctx.reset_ime();
        }
        this.ctx.request_layout();
    }

    pub fn insert_style(this: &mut WidgetMut<'_, Self>, style: StyleProperty) {
        this.widget.editor.edit_styles().insert(style);
        this.widget.reveal_caret = true;
        this.ctx.request_layout();
    }

    fn wrap_at(&mut self, width: Option<f32>) {
        let width = if self.multiline { width } else { None };
        if self.wrap_width != width {
            self.wrap_width = width;
            self.editor.set_width(width);
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

    fn keep_caret_visible(&mut self) {
        let content = self.editor.try_layout().map_or(Size::ZERO, |layout| {
            Size::new(
                f64::from(layout.full_width()) + 1.,
                f64::from(layout.height()),
            )
        });
        self.scroll
            .layout(self.scroll.viewport(), content, ScrollAxis::Both, false);
        if let Some(caret) = self.editor.cursor_geometry(1.) {
            self.scroll.reveal(rect(caret) - self.scroll.offset());
        }
    }

    fn ime_area(&self) -> Rect {
        rect(self.editor.ime_cursor_area()) + self.translation()
    }

    fn translation(&self) -> Vec2 {
        Vec2::new(0., self.text_y) - self.scroll.offset()
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
        self.wrap_at(cross_length.map(|width| width.get() as f32));
        let (fonts, layout) = ctx.text_contexts();
        Length::px(f64::from(self.editor.layout(fonts, layout).height()))
    }

    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, size: Size) {
        self.wrap_at(Some(size.width as f32));
        if self.scroll.viewport() != size {
            self.reveal_caret = true;
        }
        let (fonts, layout) = ctx.text_contexts();
        self.editor.refresh_layout(fonts, layout);
        self.text_y = if self.multiline {
            0.
        } else {
            ((size.height
                - self
                    .editor
                    .try_layout()
                    .map_or(0., |layout| f64::from(layout.height())))
                * 0.5)
                .max(0.)
        };
        let content = self.editor.try_layout().map_or(Size::ZERO, |layout| {
            Size::new(
                f64::from(layout.full_width()) + 1.,
                f64::from(layout.height()),
            )
        });
        self.scroll.layout(size, content, ScrollAxis::Both, false);
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
