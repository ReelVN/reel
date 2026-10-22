// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Text editing using the shared Parley editor, scroll geometry and platform IME events.
use std::{borrow::Cow, sync::Arc};
mod intent;
pub use intent::{
    TextInputEditingLimits, TextInputIntent, TextInputKey, TextInputReplay, TextInputReplayId,
};

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
    reel_text::{AlignmentOptions, Layout, TextWrapMode, editing::PlainEditor},
    widgets::{ScrollAxis, ScrollState},
};

/// Logical UTF-8 byte endpoints. Anchor and focus preserve selection direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextSelection {
    pub anchor: usize,
    pub focus: usize,
}

/// Adjacent edits of the same directional kind may form one undo transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextEditKind {
    Insert,
    Backspace,
    Delete,
    Transaction,
}

/// One committed replacement. Preedit never creates an edit or changes the model.
#[derive(Clone, Debug, PartialEq)]
pub struct TextEdit {
    pub range: std::ops::Range<usize>,
    pub before: TextSelection,
    pub after: TextSelection,
    pub kind: TextEditKind,
    pub revision: u64,
    pub replay: Option<TextInputReplayId>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TextInputEvent {
    Changed {
        previous: Arc<str>,
        value: Arc<str>,
        edit: TextEdit,
    },
    Selection {
        value: Arc<str>,
        selection: TextSelection,
        revision: u64,
        replay: Option<TextInputReplayId>,
    },
    Undo {
        value: Arc<str>,
        revision: u64,
        replay: Option<TextInputReplayId>,
    },
    Redo {
        value: Arc<str>,
        revision: u64,
        replay: Option<TextInputReplayId>,
    },
    Deferred {
        intent: Arc<TextInputIntent>,
        revision: u64,
    },
    ReplayCompleted(TextInputReplayId),
    CompositionCancelled,
    Submitted {
        value: Arc<str>,
        replay: Option<TextInputReplayId>,
    },
}

impl TextInputEvent {
    /// Identity of an accepted replay whose publication must still match its queue head.
    pub fn replay_id(&self) -> Option<TextInputReplayId> {
        match self {
            Self::Changed { edit, .. } => edit.replay,
            Self::Selection { replay, .. }
            | Self::Undo { replay, .. }
            | Self::Redo { replay, .. }
            | Self::Submitted { replay, .. } => *replay,
            _ => None,
        }
    }
    /// Build a complete-value replacement using the presented options' selection revision.
    /// Accessibility and embedding hosts still participate in stale-event validation.
    pub fn replace_all(
        previous: impl Into<Arc<str>>,
        value: impl Into<Arc<str>>,
        revision: u64,
    ) -> Self {
        let previous = previous.into();
        let value = value.into();
        let edit = TextEdit {
            range: 0..previous.len(),
            before: TextSelection::default(),
            after: TextSelection {
                anchor: value.len(),
                focus: value.len(),
            },
            kind: TextEditKind::Transaction,
            revision,
            replay: None,
        };
        Self::Changed {
            previous,
            value,
            edit,
        }
    }
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
    placeholder: Option<Arc<str>>,
    placeholder_layout: Option<Box<Placeholder>>,
    read_only: bool,
    selection_color: Color,
    caret_color: Option<Color>,
    outline: Option<TextOutline>,
    shadow: Option<TextShadow>,
    selection_revision: u64,
    initial_selection: Option<TextSelection>,
    pending_edit: bool,
    replaying: Option<TextInputReplayId>,
    last_replay: Option<TextInputReplayId>,
    initial_replay: Option<Arc<TextInputReplay>>,
}

struct Placeholder {
    layout: Layout<BrushIndex>,
    dirty: bool,
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
            placeholder: None,
            placeholder_layout: None,
            read_only: false,
            selection_color: Color::from_rgba8(60, 120, 220, 95),
            caret_color: None,
            outline: None,
            shadow: None,
            selection_revision: 0,
            initial_selection: None,
            pending_edit: false,
            replaying: None,
            last_replay: None,
            initial_replay: None,
        }
    }

    /// Restores logical selection before the first layout, without requesting focus.
    pub fn with_selection(mut self, selection: TextSelection, revision: u64) -> Self {
        self.initial_selection = Some(selection);
        self.selection_revision = revision;
        self
    }

    /// Restores pending editor ownership; queued work waits for the first real layout.
    pub fn with_editing(mut self, pending: bool, replay: Option<Arc<TextInputReplay>>) -> Self {
        self.pending_edit = pending;
        self.initial_replay = replay;
        self
    }

    /// Applies an authoritative selection revision using the shared editor geometry.
    /// Ordinary feedback with the same revision leaves newer local pointer input alone.
    pub fn set_selection(this: &mut WidgetMut<'_, Self>, selection: TextSelection, revision: u64) {
        if this.widget.selection_revision == revision {
            return;
        }
        let composing = this.widget.composition.is_some();
        let (fonts, layouts) = this.ctx.text_contexts();
        this.widget.cancel_compose(fonts, layouts);
        this.widget
            .editor
            .driver(fonts, layouts)
            .select_byte_range(selection.anchor, selection.focus);
        this.widget.selection_revision = revision;
        let actual = this.widget.selection();
        if actual != selection {
            this.ctx
                .submit_action::<TextInputEvent>(TextInputEvent::Selection {
                    value: this.widget.value.clone(),
                    selection: actual,
                    revision,
                    replay: None,
                });
        }
        this.widget.reveal_caret = true;
        this.widget.keep_caret_visible();
        if composing {
            this.ctx.reset_ime();
            this.ctx.request_layout();
        }
        this.ctx.set_ime_area(this.widget.ime_area());
        this.ctx.request_render();
    }

    /// Replay an already accepted editing intent after its preceding operation commits.
    /// The same editor parses relative navigation against its current layout and caret.
    /// Focus changes route new input elsewhere; accepted work remains owned by this input.
    pub fn set_editing(
        this: &mut WidgetMut<'_, Self>,
        pending: bool,
        replay: Option<&TextInputReplay>,
    ) {
        this.widget.pending_edit = pending;
        let Some(replay) = replay.filter(|r| Some(r.id) != this.widget.last_replay) else {
            return;
        };
        let MutateCtx {
            geometry,
            global_state,
            widget_state,
            children,
            property_arena,
            ..
        } = &mut this.ctx;
        let target = widget_state.id;
        let mut ctx = EventCtx {
            geometry: *geometry,
            global_state,
            widget_state,
            children: children.reborrow_mut(),
            property_arena,
            target,
            allow_pointer_capture: false,
            is_handled: false,
            propagation_stopped: false,
        };
        this.widget.replay(&mut ctx, replay);
    }

    fn replay(&mut self, ctx: &mut EventCtx<'_>, replay: &TextInputReplay) {
        self.last_replay = Some(replay.id);
        self.replaying = Some(replay.id);
        if let TextInputIntent::Selection { selection } = replay.intent.as_ref() {
            let before = self.selection();
            let normalize = |mut offset: usize| {
                offset = offset.min(self.value.len());
                while !self.value.is_char_boundary(offset) {
                    offset -= 1;
                }
                offset
            };
            let (anchor, focus) = (normalize(selection.anchor), normalize(selection.focus));
            let (fonts, layout) = ctx.text_contexts();
            self.editor
                .driver(fonts, layout)
                .select_byte_range(anchor, focus);
            self.publish_selection(ctx, before);
            self.keep_caret_visible();
            ctx.set_ime_area(self.ime_area());
            ctx.request_render();
        } else if let TextInputIntent::ReplaceAll(text) = replay.intent.as_ref() {
            self.replace_value(ctx, text);
        } else if let Some(event) = replay.intent.event() {
            self.handle_text_event(ctx, &event);
        }
        ctx.submit_action::<TextInputEvent>(TextInputEvent::ReplayCompleted(replay.id));
        self.replaying = None;
    }

    fn selection(&self) -> TextSelection {
        let selection = self.editor.raw_selection();
        TextSelection {
            anchor: selection.anchor().index(),
            focus: selection.focus().index(),
        }
    }

    fn publish_selection(&self, ctx: &mut EventCtx<'_>, before: TextSelection) {
        let selection = self.selection();
        if selection != before {
            if self.pending_edit && self.replaying.is_none() {
                ctx.submit_action::<TextInputEvent>(TextInputEvent::Deferred {
                    intent: Arc::new(TextInputIntent::Selection { selection }),
                    revision: self.selection_revision,
                });
                return;
            }
            ctx.submit_action::<TextInputEvent>(TextInputEvent::Selection {
                value: self.value.clone(),
                selection,
                revision: self.selection_revision,
                replay: self.replaying,
            });
        }
    }

    /// Displays a hint while the value is empty and no IME composition is active.
    /// The hint shares editor formatting and creates a layout only when visible.
    pub fn with_placeholder(mut self, text: impl Into<Arc<str>>) -> Self {
        let text = text.into();
        self.placeholder = (!text.is_empty()).then_some(text);
        self
    }

    /// Prevents user mutation while retaining focus, selection, copying and submission.
    /// Scripted value updates remain available. System IME is disabled while read-only.
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Displays one bullet per grapheme, including preedit, while retaining source UTF-8.
    /// Copy exports no text; cut deletes without writing to the clipboard.
    pub fn with_password(mut self, password: bool) -> Self {
        self.editor.set_password(password);
        self
    }

    /// Sets the selection background; the default is RGBA (60, 120, 220, 95).
    pub fn with_selection_color(mut self, color: Color) -> Self {
        self.selection_color = color;
        self
    }

    /// Sets the caret color. `None` follows the content color.
    pub fn with_caret_color(mut self, color: Option<Color>) -> Self {
        self.caret_color = color;
        self
    }

    /// Paints an outline using the editor's existing glyph layout.
    pub fn with_outline(mut self, outline: Option<TextOutline>) -> Self {
        self.outline = outline;
        self
    }

    /// Paints a shadow using the editor's existing glyph layout.
    pub fn with_shadow(mut self, shadow: Option<TextShadow>) -> Self {
        self.shadow = shadow;
        self
    }

    /// Updates the empty-value hint without changing the editor or its selection.
    /// None or an empty string hides the hint and releases its cached layout.
    pub fn set_placeholder(this: &mut WidgetMut<'_, Self>, text: Option<Arc<str>>) {
        let text = text.filter(|text| !text.is_empty());
        if this.widget.placeholder == text {
            return;
        }
        this.widget.placeholder = text;
        this.widget.placeholder_layout = None;
        if this.widget.value.is_empty() && this.widget.composition.is_none() {
            this.ctx.request_layout();
        }
        this.ctx.request_accessibility_update();
    }

    /// Cancels preedit before changing system IME eligibility. Value and selection survive.
    pub fn set_read_only(this: &mut WidgetMut<'_, Self>, read_only: bool) {
        if this.widget.read_only == read_only {
            return;
        }
        let composing = this.widget.composition.is_some();
        let (fonts, layouts) = this.ctx.text_contexts();
        this.widget.cancel_compose(fonts, layouts);
        this.widget.read_only = read_only;
        this.ctx.set_ime_enabled(!read_only);
        if composing {
            this.ctx.request_layout();
        }
        this.ctx.request_render();
    }

    /// Changes display policy without replacing text, selection, preedit or editing history.
    pub fn set_password(this: &mut WidgetMut<'_, Self>, password: bool) {
        if this.widget.editor.is_password() == password {
            return;
        }
        this.widget.editor.set_password(password);
        this.widget.geometry_dirty = true;
        this.widget.reveal_caret = true;
        this.ctx.request_layout();
        this.ctx.request_accessibility_update();
    }

    /// Updates selection paint without invalidating text layout.
    pub fn set_selection_color(this: &mut WidgetMut<'_, Self>, color: Color) {
        if this.widget.selection_color != color {
            this.widget.selection_color = color;
            this.ctx.request_render();
        }
    }

    /// Updates caret paint without invalidating text layout. `None` follows content color.
    pub fn set_caret_color(this: &mut WidgetMut<'_, Self>, color: Option<Color>) {
        if this.widget.caret_color != color {
            this.widget.caret_color = color;
            this.ctx.request_render();
        }
    }

    /// Updates outline paint while retaining the shaped text layout.
    pub fn set_outline(this: &mut WidgetMut<'_, Self>, outline: Option<TextOutline>) {
        if this.widget.outline != outline {
            this.widget.outline = outline;
            this.ctx.request_render();
        }
    }

    /// Updates shadow paint while retaining the shaped text layout.
    pub fn set_shadow(this: &mut WidgetMut<'_, Self>, shadow: Option<TextShadow>) {
        if this.widget.shadow != shadow {
            this.widget.shadow = shadow;
            this.ctx.request_render();
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
        self.invalidate_placeholder();
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
        this.widget.invalidate_placeholder();
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
        self.invalidate_placeholder();
        self.geometry_dirty = true;
        self.reveal_caret = true;
        true
    }

    fn update_wrap_mode(&mut self) {
        self.invalidate_placeholder();
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
            self.invalidate_placeholder();
            self.geometry_dirty = true;
            self.reveal_caret = true;
        }
    }

    fn invalidate_placeholder(&mut self) {
        if let Some(hint) = &mut self.placeholder_layout {
            hint.dirty = true;
        }
    }

    fn refresh_placeholder(
        &mut self,
        fonts: &mut crate::reel_text::FontContext,
        layouts: &mut crate::reel_text::LayoutContext<BrushIndex>,
    ) {
        if !self.value.is_empty() || self.composition.is_some() {
            return;
        }
        let Some(text) = &self.placeholder else {
            return;
        };
        let hint = self.placeholder_layout.get_or_insert_with(|| {
            Box::new(Placeholder {
                layout: Layout::default(),
                dirty: true,
            })
        });
        if !hint.dirty {
            return;
        }
        let text = input_text(text, self.multiline);
        let mut builder = layouts.ranged_builder(fonts, &text, 1., true);
        for style in self.editor.get_styles().inner().values() {
            builder.push_default(style.clone());
        }
        hint.layout = builder.build(&text);
        hint.layout.break_all_lines(self.layout_width);
        hint.layout.align(
            self.layout_width,
            self.text_alignment,
            AlignmentOptions::default(),
        );
        hint.dirty = false;
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

    fn publish(
        &mut self,
        ctx: &mut EventCtx<'_>,
        before: TextSelection,
        kind: TextEditKind,
        replace_all: bool,
    ) {
        if self.editor.raw_text() != self.value.as_ref() {
            let value: Arc<str> = self.editor.raw_text().into();
            let after = self.selection();
            let mut range = before.anchor.min(before.focus)..before.anchor.max(before.focus);
            if replace_all {
                range = 0..self.value.len();
            } else if range.is_empty() && value.len() < self.value.len() {
                let removed = self.value.len() - value.len();
                if kind == TextEditKind::Backspace {
                    range.start -= removed;
                } else if kind == TextEditKind::Delete {
                    range.end += removed;
                }
            }
            let previous = std::mem::replace(&mut self.value, value.clone());
            ctx.submit_action::<TextInputEvent>(TextInputEvent::Changed {
                previous,
                value,
                edit: TextEdit {
                    range,
                    before,
                    after,
                    kind,
                    revision: self.selection_revision,
                    replay: self.replaying,
                },
            });
        } else if kind == TextEditKind::Transaction {
            ctx.submit_action::<TextInputEvent>(TextInputEvent::Selection {
                value: self.value.clone(),
                selection: self.selection(),
                revision: self.selection_revision,
                replay: self.replaying,
            });
        } else {
            self.publish_selection(ctx, before);
        }
    }

    fn replace_value(&mut self, ctx: &mut EventCtx<'_>, text: &str) {
        let composing = self.composition.is_some();
        let before = self.selection();
        let (fonts, layout) = ctx.text_contexts();
        self.cancel_compose(fonts, layout);
        self.editor.set_text(&input_text(text, self.multiline));
        self.geometry_dirty = true;
        if composing {
            ctx.reset_ime();
        }
        self.publish(ctx, before, TextEditKind::Transaction, true);
        self.reveal_caret = true;
        ctx.request_layout();
        ctx.set_handled();
    }

    fn handle_text_event(&mut self, ctx: &mut EventCtx<'_>, event: &TextEvent) {
        if ctx.is_disabled()
            || (self.replaying.is_none() && !ctx.is_focus_target())
            || !ctx.input_configuration_is_current()
        {
            return;
        }
        if self.pending_edit
            && !self.read_only
            && self.replaying.is_none()
            && let Some(intent) = TextInputIntent::capture(event)
        {
            ctx.submit_action::<TextInputEvent>(TextInputEvent::Deferred {
                intent: Arc::new(intent),
                revision: self.selection_revision,
            });
            ctx.set_handled();
            return;
        }
        if self.read_only && matches!(event, TextEvent::Ime(_) | TextEvent::ClipboardPaste(_)) {
            ctx.set_handled();
            return;
        }
        let mut changed = false;
        let before = self.composition.as_ref().map_or_else(
            || self.selection(),
            |(_, anchor, focus)| TextSelection {
                anchor: *anchor,
                focus: *focus,
            },
        );
        let mut kind = TextEditKind::Transaction;
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
                if self.pending_edit {
                    ctx.submit_action::<TextInputEvent>(TextInputEvent::CompositionCancelled);
                }
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
                    && (key.eq_ignore_ascii_case("z")
                        || (!cfg!(target_os = "macos") && key.eq_ignore_ascii_case("y")))
                {
                    if !self.read_only {
                        let redo = shift || key.eq_ignore_ascii_case("y");
                        self.pending_edit = true;
                        ctx.submit_action::<TextInputEvent>(if redo {
                            TextInputEvent::Redo {
                                value: self.value.clone(),
                                revision: self.selection_revision,
                                replay: self.replaying,
                            }
                        } else {
                            TextInputEvent::Undo {
                                value: self.value.clone(),
                                revision: self.selection_revision,
                                replay: self.replaying,
                            }
                        });
                    }
                    ctx.set_handled();
                    return;
                }
                if let Key::Character(key) = &event.key
                    && command
                    && (key.eq_ignore_ascii_case("c") || key.eq_ignore_ascii_case("x"))
                {
                    if !self.editor.is_password()
                        && (!self.read_only || key.eq_ignore_ascii_case("c"))
                        && let Some(text) = self.editor.selected_text()
                    {
                        ctx.set_clipboard(text.to_owned());
                    }
                    if !self.read_only && key.eq_ignore_ascii_case("x") {
                        let (fonts, layout) = ctx.text_contexts();
                        self.editor.driver(fonts, layout).delete_selection();
                        changed = true;
                    }
                } else if event.key == Key::Named(NamedKey::Enter) {
                    if self.multiline && !command {
                        if !self.read_only {
                            let (fonts, layout) = ctx.text_contexts();
                            self.editor
                                .driver(fonts, layout)
                                .insert_or_replace_selection("\n");
                            changed = true;
                        }
                    } else if !event.repeat {
                        ctx.submit_action::<TextInputEvent>(TextInputEvent::Submitted {
                            value: self.value.clone(),
                            replay: self.replaying,
                        });
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
                            if !self.read_only
                                && !command
                                && (alt_graph
                                    || !event.modifiers.alt()
                                    || cfg!(target_os = "macos")) =>
                        {
                            driver.insert_or_replace_selection(&input_text(key, self.multiline));
                            kind = TextEditKind::Insert;
                            changed = true;
                        }
                        Key::Named(NamedKey::Backspace) if !self.read_only => {
                            kind = TextEditKind::Backspace;
                            if word {
                                driver.backdelete_word();
                            } else {
                                driver.backdelete();
                            }
                            changed = true;
                        }
                        Key::Named(NamedKey::Delete) if !self.read_only => {
                            kind = TextEditKind::Delete;
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
                        Key::Character(_) | Key::Named(NamedKey::Backspace | NamedKey::Delete)
                            if self.read_only => {}
                        _ => return,
                    }
                }
            }
            _ => return,
        }
        if changed {
            self.geometry_dirty = true;
            self.publish(ctx, before, kind, false);
        }
        if !changed && self.composition.is_none() {
            self.publish_selection(ctx, before);
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
        let before = self.selection();
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
        self.publish_selection(ctx, before);
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
        self.handle_text_event(ctx, event);
    }

    fn update(&mut self, ctx: &mut UpdateCtx<'_>, _: &mut PropertiesMut<'_>, event: &Update) {
        match event {
            Update::WidgetAdded => ctx.set_ime_enabled(!self.read_only),
            Update::FocusChanged(false)
            | Update::WindowFocusChanged(false)
            | Update::DisabledChanged(true)
            | Update::StashedChanged(true) => {
                if self.pending_edit {
                    ctx.submit_action::<TextInputEvent>(TextInputEvent::CompositionCancelled);
                }
                let (fonts, layout) = ctx.text_contexts();
                self.cancel_compose(fonts, layout);
                ctx.request_layout();
            }
            Update::FocusChanged(_) | Update::WindowFocusChanged(_) => ctx.request_render(),
            Update::FontsChanged => {
                self.editor.set_scale(1.);
                self.invalidate_placeholder();
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
        if let Some(selection) = self.initial_selection.take() {
            self.editor
                .driver(fonts, layout)
                .select_byte_range(selection.anchor, selection.focus);
        }
        self.refresh_placeholder(fonts, layout);
        self.update_geometry(size);
        if self.reveal_caret {
            self.keep_caret_visible();
            self.reveal_caret = false;
        }
        ctx.set_clip_path(size.to_rect());
        ctx.set_ime_area(self.ime_area());
        if let Some(replay) = self.initial_replay.take() {
            let LayoutCtx {
                geometry,
                global_state,
                widget_state,
                children,
                property_arena,
                ..
            } = ctx;
            let target = widget_state.id;
            let mut events = EventCtx {
                geometry: *geometry,
                global_state,
                widget_state,
                children: children.reborrow_mut(),
                property_arena,
                target,
                allow_pointer_capture: false,
                is_handled: false,
                propagation_stopped: false,
            };
            self.replay(&mut events, &replay);
        }
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
                    .fill(rect(bounds), self.selection_color)
                    .transform(transform)
                    .draw();
            });
        }
        let layout = if self.value.is_empty() && self.composition.is_none() {
            self.placeholder_layout
                .as_ref()
                .map(|hint| &hint.layout)
                .or_else(|| self.editor.try_layout())
        } else {
            self.editor.try_layout()
        };
        if let Some(layout) = layout {
            if let Some(shadow) = self.shadow.filter(|s| s.offset != Vec2::ZERO) {
                render_text(
                    painter,
                    Affine::translate(shadow.offset) * transform,
                    layout,
                    &[shadow.color.into()],
                    true,
                    self.outline.map(|outline| TextOutline {
                        color: shadow.color,
                        ..outline
                    }),
                );
            }
            render_text(
                painter,
                transform,
                layout,
                &[color.into()],
                true,
                self.outline,
            );
        }
        if ctx.is_focus_target()
            && ctx.is_window_focused()
            && let Some(caret) = self.editor.cursor_geometry(1.)
        {
            painter
                .fill(rect(caret), self.caret_color.unwrap_or(color))
                .transform(transform)
                .draw();
        }
    }

    fn accessibility_role(&self) -> Role {
        if self.editor.is_password() {
            Role::PasswordInput
        } else if self.multiline {
            Role::MultilineTextInput
        } else {
            Role::TextInput
        }
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, node: &mut Node) {
        if self.editor.is_password() {
            node.clear_value();
        } else {
            node.set_value(self.value.as_ref());
        }
        if let Some(placeholder) = &self.placeholder {
            node.set_placeholder(placeholder.as_ref());
        }
        if self.read_only {
            node.set_read_only();
        } else {
            node.add_action(Action::SetValue);
        }
    }
    fn on_access_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &AccessEvent,
    ) {
        if self.read_only || ctx.is_disabled() || !ctx.input_configuration_is_current() {
            return;
        }
        if event.action == Action::SetValue
            && let Some(ActionData::Value(text)) = &event.data
        {
            if self.pending_edit {
                ctx.submit_action::<TextInputEvent>(TextInputEvent::Deferred {
                    intent: Arc::new(TextInputIntent::ReplaceAll(Arc::from(text.as_ref()))),
                    revision: self.selection_revision,
                });
                ctx.set_handled();
            } else {
                self.replace_value(ctx, text);
            }
        }
    }

    fn get_cursor(&self, _: &QueryCtx<'_>, _: Point) -> CursorIcon {
        CursorIcon::Text
    }
}

#[cfg(test)]
mod tests;
