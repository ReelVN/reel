// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Single-line editing using the shared Parley editor and platform IME events.
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
    width: f64,
    scroll_x: f64,
    text_y: f64,
}

fn single_line(text: &str) -> Cow<'_, str> {
    if text.contains(['\r', '\n']) {
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
        let value = value.into();
        let value = match single_line(&value) {
            Cow::Owned(text) => text.into(),
            Cow::Borrowed(_) => value,
        };
        let mut editor = PlainEditor::new(24.);
        editor.set_text(&value);
        Self {
            editor,
            value,
            composition: None,
            width: 0.,
            scroll_x: 0.,
            text_y: 0.,
        }
    }

    pub fn with_style(mut self, style: StyleProperty) -> Self {
        self.editor.edit_styles().insert(style);
        self
    }

    /// Authoritative model updates preserve selection when they echo the current value.
    pub fn set_value(this: &mut WidgetMut<'_, Self>, value: Arc<str>) {
        let value = match single_line(&value) {
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
        if composing {
            this.ctx.reset_ime();
        }
        this.ctx.request_layout();
    }

    pub fn insert_style(this: &mut WidgetMut<'_, Self>, style: StyleProperty) {
        this.widget.editor.edit_styles().insert(style);
        this.ctx.request_layout();
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
        if let Some(caret) = self.editor.cursor_geometry(1.) {
            if caret.x0 < self.scroll_x {
                self.scroll_x = caret.x0;
            } else if caret.x1 > self.scroll_x + self.width {
                self.scroll_x = (caret.x1 - self.width).max(0.);
            }
        }
        let end = self
            .editor
            .try_layout()
            .map_or(0., |layout| f64::from(layout.full_width()) + 1.);
        self.scroll_x = self.scroll_x.min((end - self.width).max(0.));
    }

    fn ime_area(&self) -> Rect {
        rect(self.editor.ime_cursor_area()) + Vec2::new(-self.scroll_x, self.text_y)
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
        let point = ctx.local_position(position) + Vec2::new(self.scroll_x, -self.text_y);
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
                driver.insert_or_replace_selection(&single_line(text));
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
                    .insert_or_replace_selection(&single_line(text));
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
                    if !event.repeat {
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
                            driver.insert_or_replace_selection(&single_line(key));
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
                            if shift {
                                driver.select_to_text_start();
                            } else {
                                driver.move_to_text_start();
                            }
                        }
                        Key::Named(NamedKey::End) => {
                            if shift {
                                driver.select_to_text_end();
                            } else {
                                driver.move_to_text_end();
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
        _: Option<Length>,
    ) -> Length {
        if axis == Axis::Horizontal {
            return match request {
                LenReq::FitContent(space) => space,
                _ => Length::px(160.),
            };
        }
        let (fonts, layout) = ctx.text_contexts();
        Length::px(f64::from(self.editor.layout(fonts, layout).height()))
    }

    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, size: Size) {
        self.width = size.width;
        let (fonts, layout) = ctx.text_contexts();
        self.editor.refresh_layout(fonts, layout);
        self.text_y = ((size.height
            - self
                .editor
                .try_layout()
                .map_or(0., |layout| f64::from(layout.height())))
            * 0.5)
            .max(0.);
        self.keep_caret_visible();
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
        let transform = Affine::translate((-self.scroll_x, self.text_y));
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
        Role::TextInput
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
            self.editor.set_text(&single_line(text));
            if composing {
                ctx.reset_ime();
            }
            self.publish(ctx);
            ctx.request_layout();
            ctx.set_handled();
        }
    }
    fn get_cursor(&self, _: &QueryCtx<'_>, _: Point) -> CursorIcon {
        CursorIcon::Text
    }
}
