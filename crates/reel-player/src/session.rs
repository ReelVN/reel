//! Real Rust session projection into Reel's maintained UI and presented input path.
//!
//! This layer never executes Aside. The optional native diagnostic below drives
//! explicit host requests and must not be described as a source story runner.
mod geometry;
mod node_events;
mod playback;
#[cfg(test)]
mod popup_tests;
mod popups;
use crate::stage_widget::{StageView, StageWidget};
pub use geometry::RecordedProjection;
use playback::{dialogue_complete, dialogue_revision};
#[cfg(test)]
mod button_tests;
#[cfg(test)]
mod hal_tests;
#[cfg(test)]
mod image_tests;
#[cfg(test)]
mod prepared_image_tests;
#[cfg(test)]
mod text_editing_tests;
use reel_engine::{
    ChoiceId, InputAction, InputRejection, InteractionId, InteractionView, OwnerHandle, Session,
};
#[cfg(any(test, feature = "window"))]
use reel_engine::{ChoiceInput, CompletionOutcome, DialogueLineInput, Epoch, SessionId};
#[cfg(test)]
use reel_engine::{InputEvent, InputSequence};
use reel_ui::{
    accesskit::{Node, Role},
    app::LayerStack,
    bridge::PresentationBridge,
    core::*,
    imaging::Painter,
    kurbo::{Axis, Point, Rect, Size, Vec2},
    layout::{LenDef, LenReq, Length, SizeDef},
    peniko::Color,
    widgets::{Button, ButtonPress, Label, ScrollAxis, ScrollState},
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    ops::Range,
    rc::Rc,
};

type Error = Box<dyn std::error::Error>;

/// One recorded frame, with a separate identity for dialogue confirmation.
/// Animation and control feedback can repaint without replacing dialogue input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectionStamp {
    pub(crate) presentation_request: u64,
    dialogue_complete: bool,
    dialogue_revision: u64,
    revision: u64,
    advance_revision: u64,
    button_revision: u64,
    interaction: Option<InteractionId>,
    scroll_bits: u64,
}
impl ProjectionStamp {
    fn same_advance(self, other: Self) -> bool {
        self.advance_revision == other.advance_revision
            && self.interaction == other.interaction
            && self.dialogue_complete == other.dialogue_complete
            && self.dialogue_revision == other.dialogue_revision
            && self.scroll_bits == other.scroll_bits
    }
}
#[derive(Default)]
struct InputBinding {
    accepted: Option<ProjectionStamp>,
    control_accepted: Option<ProjectionStamp>,
    down: Option<ProjectionStamp>,
    contacts: Vec<(PointerInfo, ProjectionStamp, Option<u64>)>,
    button_revision: u64,
    accepted_buttons: Option<u64>,
    buttons: BTreeMap<reel_ui::stage::NodeId, u64>,
    button_down: Option<u64>,
    non_pointer: bool,
    navigation_down: Option<ProjectionStamp>,
}
impl InputBinding {
    fn permits_control(&self) -> bool {
        self.control_accepted.is_some() && (self.down.is_some() || self.non_pointer)
    }
    fn permits_text(&self, event: &reel_ui::widgets::TextInputEvent) -> bool {
        // Replays already own accepted input. Their output and terminal cleanup
        // arrive during projection, after the physical dispatch window closes.
        // SessionVm verifies the complete replay identity against the queue head.
        self.permits_control()
            || event.replay_id().is_some()
            || matches!(
                event,
                reel_ui::widgets::TextInputEvent::ReplayCompleted(_)
                    | reel_ui::widgets::TextInputEvent::CompositionCancelled
            )
    }
    fn permits_button(&self, node: reel_ui::stage::NodeId) -> bool {
        self.buttons.get(&node).is_some_and(|revision| {
            self.accepted_buttons
                .is_some_and(|accepted| accepted >= *revision)
                && (self.non_pointer || self.button_down.is_some_and(|down| down >= *revision))
        })
    }
    fn permits(&self, current: ProjectionStamp) -> bool {
        self.accepted
            .is_some_and(|stamp| stamp.same_advance(current))
            && (self.down.is_some_and(|stamp| stamp.same_advance(current)) || self.non_pointer)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct TextRow {
    speaker: String,
    text: String,
    visible: Range<usize>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct OptionRow {
    id: ChoiceId,
    label: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Projection {
    interaction: Option<InteractionId>,
    lines: Vec<TextRow>,
    choices: Vec<OptionRow>,
    history_count: usize,
    status: reel_engine::SessionStatus,
}
impl Projection {
    fn from_session(session: &Session, preview: bool) -> Result<Self, Error> {
        let view = session.view();
        let mut projection = Self {
            interaction: None,
            lines: vec![],
            choices: vec![],
            history_count: view.history.len(),
            status: view.status,
        };
        match view.interaction {
            Some(InteractionView::Dialogue(dialogue)) => {
                projection.interaction = Some(dialogue.id());
                if !preview {
                    return Ok(projection);
                }
                projection.lines = dialogue
                    .lines()
                    .iter()
                    .map(|line| TextRow {
                        speaker: line.speaker_name().into(),
                        text: line.resolved_text().into(),
                        visible: line.reveal().visible_bytes,
                    })
                    .collect();
            }
            Some(InteractionView::Choice(choice)) => {
                projection.interaction = Some(choice.id());
                if !preview {
                    return Ok(projection);
                }
                projection.choices = choice
                    .options()
                    .iter()
                    .map(|row| OptionRow {
                        id: row.id().clone(),
                        label: row.label().into(),
                    })
                    .collect();
            }
            None => {}
        }
        Ok(projection)
    }
}
#[derive(Debug)]
enum UiInput {
    Session {
        owner: OwnerHandle,
        action: InputAction,
    },
}

/// A control activation uses the same consumed input as the session projection.
pub struct UiDispatch {
    pub handled: Handled,
    pub events: Vec<reel_ui::stage::NodeEvent>,
}
/// Native widget routing precedes mutable access to a captured session.
pub(crate) struct TextDispatch {
    pub(crate) handled: Handled,
    pub(crate) changes_session: bool,
    focused: Option<bool>,
    pause_dialogue: bool,
}
struct LineWidgets {
    speaker: WidgetPod<Label>,
    text: WidgetPod<Label>,
}
struct ChoiceWidget {
    id: ChoiceId,
    owner: OwnerHandle,
    button: WidgetPod<Button>,
}

struct PreviewChrome {
    title: WidgetPod<Label>,
    status: WidgetPod<Label>,
    backdrop: WidgetPod<DialogueBackdrop>,
}

/// Author stage and accepted session input. Diagnostic chrome is opt-in.
pub struct SessionWidget {
    projection: Projection,
    stamp: ProjectionStamp,
    source_owner: OwnerHandle,
    binding: Rc<RefCell<InputBinding>>,
    inputs: Rc<RefCell<Option<UiInput>>>,
    events: Rc<RefCell<Vec<reel_ui::stage::NodeEvent>>>,
    preview: Option<PreviewChrome>,
    lines: Vec<LineWidgets>,
    choices: Vec<ChoiceWidget>,
    background: Color,
    scroll: Rc<Cell<f64>>,
    content_height: f64,
    scrolling: ScrollState,
    viewport: Size,
    dialogue_rect: Rect,
    content: WidgetPod<LayerStack>,
    stage: WidgetId,
    popup_layers: Vec<popups::LayerRecord>,
    authored: bool,
    dialogue_active: bool,
    custom_dialogue: bool,
}
impl SessionWidget {
    fn scroll_changed(&mut self) {
        self.scroll.set(self.scrolling.offset().y);
        let mut binding = self.binding.borrow_mut();
        *binding = InputBinding {
            button_revision: binding.button_revision,
            buttons: std::mem::take(&mut binding.buttons),
            ..Default::default()
        };
    }
    fn new(ui: &SessionUi) -> Self {
        let stage = NewWidget::new(StageWidget::new(ui.stage.as_ref()));
        let stage_id = stage.id();
        let mut widget = Self {
            projection: ui.projection.clone(),
            stamp: ui.stamp,
            source_owner: ui.source_owner,
            binding: ui.binding.clone(),
            inputs: ui.inputs.clone(),
            events: ui.events.clone(),
            preview: ui.preview.then(|| PreviewChrome {
                title: WidgetPod::new(Label::new("Reel")),
                status: WidgetPod::new(Label::new(ui.status_text())),
                backdrop: WidgetPod::new(DialogueBackdrop { rect: Rect::ZERO }),
            }),
            lines: vec![],
            choices: vec![],
            background: ui.background,
            scroll: ui.scroll.clone(),
            content_height: 420.,
            scrolling: ScrollState::default(),
            viewport: Size::new(640., 420.),
            dialogue_rect: Rect::new(30., 245., 610., 380.),
            content: WidgetPod::new(LayerStack::new(stage)),
            stage: stage_id,
            popup_layers: Vec::new(),
            authored: (ui.stage.is_some() || !ui.preview),
            dialogue_active: matches!(
                ui.session.view().interaction,
                Some(InteractionView::Dialogue(_))
            ),
            custom_dialogue: !ui.preview,
        };
        widget.make_children(&ui.choice_owners);
        widget
    }
    fn current_stamp(&self) -> ProjectionStamp {
        ProjectionStamp {
            scroll_bits: self.scroll.get().to_bits(),
            ..self.stamp
        }
    }
    fn make_children(&mut self, owners: &[OwnerHandle]) {
        self.lines = self
            .projection
            .lines
            .iter()
            .map(|line| LineWidgets {
                speaker: WidgetPod::new(Label::new(line.speaker.clone())),
                text: WidgetPod::new(
                    Label::new(line.text.clone()).with_visible_bytes(line.visible.clone()),
                ),
            })
            .collect();
        self.choices = self
            .projection
            .choices
            .iter()
            .zip(owners.iter().cycle())
            .map(|(row, owner)| ChoiceWidget {
                id: row.id.clone(),
                owner: *owner,
                button: WidgetPod::new(Button::with_text(row.label.clone())),
            })
            .collect();
    }
    fn sync(this: &mut WidgetMut<'_, Self>, ui: &SessionUi) {
        if this.widget.source_owner != ui.source_owner {
            // A restored session has fresh input ownership even when node IDs match.
            let stage = NewWidget::new(StageWidget::new(ui.stage.as_ref()));
            this.widget.stage = stage.id();
            let old = std::mem::replace(
                &mut this.widget.content,
                WidgetPod::new(LayerStack::new(stage)),
            );
            this.widget.popup_layers.clear();
            this.ctx.remove_child(old);
            this.widget.source_owner = ui.source_owner;
            this.widget.binding = ui.binding.clone();
            this.widget.inputs = ui.inputs.clone();
            this.widget.events = ui.events.clone();
            this.widget.scroll = ui.scroll.clone();
            this.widget.scrolling = ScrollState::default();
            this.ctx.children_changed();
        }
        let authored_changed = this.widget.authored != (ui.stage.is_some() || !ui.preview);
        this.widget.authored = ui.stage.is_some() || !ui.preview;
        this.widget.dialogue_active = matches!(
            ui.session.view().interaction,
            Some(InteractionView::Dialogue(_))
        );
        this.widget.custom_dialogue = !ui.preview;
        if ui.stage_dirty
            && let Some(mut content) = this.ctx.get_mut(&mut this.widget.content)
            && let Some(mut stage) = LayerStack::layer_root_mut(&mut content, 0)
        {
            StageWidget::sync(
                &mut stage.downcast::<StageWidget>(),
                ui.stage.as_ref().unwrap_or(&StageView::default()),
            );
        }
        this.widget.stamp = ui.stamp;
        if this.widget.background != ui.background {
            this.widget.background = ui.background;
            this.ctx.request_render();
        }
        let same_interaction = this.widget.projection.interaction == ui.projection.interaction;
        let status_changed = this.widget.projection.status != ui.projection.status
            || this.widget.projection.history_count != ui.projection.history_count;
        if same_interaction {
            for ((widgets, previous), row) in this
                .widget
                .lines
                .iter_mut()
                .zip(&mut this.widget.projection.lines)
                .zip(&ui.projection.lines)
            {
                if previous.visible != row.visible {
                    if let Some(mut label) = this.ctx.get_mut(&mut widgets.text) {
                        Label::set_visible_bytes(&mut label, row.visible.clone());
                    }
                    previous.visible = row.visible.clone();
                }
            }
            this.widget.projection.status = ui.projection.status;
            this.widget.projection.history_count = ui.projection.history_count;
        } else {
            this.widget.scroll.set(0.);
            this.widget.scrolling.set_offset(Vec2::ZERO);
            for line in std::mem::take(&mut this.widget.lines) {
                this.ctx.remove_child(line.speaker);
                this.ctx.remove_child(line.text);
            }
            for choice in std::mem::take(&mut this.widget.choices) {
                this.ctx.remove_child(choice.button);
            }
            this.widget.projection = ui.projection.clone();
            this.widget.make_children(&ui.choice_owners);
            this.ctx.children_changed();
        }
        if status_changed
            && let Some(preview) = &mut this.widget.preview
            && let Some(mut status) = this.ctx.get_mut(&mut preview.status)
        {
            Label::set_text(&mut status, ui.status_text());
        }
        if !same_interaction || status_changed || authored_changed {
            this.ctx.request_layout();
        }
    }
}
impl Widget for SessionWidget {
    type Action = NoAction;
    fn on_pointer_preview(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if !self.lines.is_empty() || !self.choices.is_empty() {
            self.scrolling.preview(ctx, event);
        }
    }
    fn scroll_to_child(
        &mut self,
        ctx: &mut UpdateCtx<'_>,
        _: &mut PropertiesMut<'_>,
        rect: Rect,
    ) -> Rect {
        if !self
            .choices
            .iter()
            .any(|choice| Some(choice.button.id()) == ctx.focus_target_id())
        {
            return rect;
        }
        let delta = self.scrolling.reveal(rect);
        if delta != Vec2::ZERO {
            self.scroll_changed();
            ctx.request_render();
            ctx.request_compose();
        }
        rect + delta
    }

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if self.scrolling.pointer(ctx, event) {
            self.scroll_changed();
            ctx.request_render();
            ctx.request_compose();
            return;
        }
        if matches!(event, PointerEvent::Scroll(_)) || ctx.is_handled() {
            return;
        }
        // Choice controls own their click. It cannot fall through into a dialogue
        // that is installed after this dispatch completes.
        if !self.dialogue_active {
            return;
        }
        let Some(interaction) = self.projection.interaction else {
            return;
        };
        match event {
            PointerEvent::Down(e)
                if e.button.is_none_or(|b| b == PointerButton::Primary)
                    && self.dialogue_rect.contains(
                        ctx.local_position(e.state.position) + Vec2::new(0., self.scroll.get()),
                    ) =>
            {
                ctx.capture_pointer_for_click();
                ctx.set_handled();
            }
            PointerEvent::Up(e)
                if ctx.is_active() && e.button.is_none_or(|b| b == PointerButton::Primary) =>
            {
                if self.binding.borrow().permits(self.current_stamp())
                    && self.dialogue_rect.contains(
                        ctx.local_position(e.state.position) + Vec2::new(0., self.scroll.get()),
                    )
                {
                    self.inputs.borrow_mut().get_or_insert(UiInput::Session {
                        owner: self.source_owner,
                        action: InputAction::Advance(interaction),
                    });
                }
                ctx.release_pointer();
                ctx.set_handled();
            }
            PointerEvent::Cancel(_) => {
                ctx.release_pointer();
            }
            _ => {}
        }
    }
    fn on_text_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &TextEvent,
    ) {
        if matches!(event, TextEvent::WindowFocusChange(false)) {
            self.scrolling.cancel();
        }
        use reel_ui::input::{ActionPhase, NavigationAction};
        if ctx.is_handled() || !self.dialogue_active {
            return;
        }
        let phase = match event {
            TextEvent::Navigation(event) if event.action == NavigationAction::Confirm => {
                event.phase
            }
            TextEvent::Keyboard(key)
                if key.key == keyboard::Key::Named(keyboard::NamedKey::Enter) =>
            {
                if key.state.is_up() {
                    ActionPhase::Released
                } else if key.repeat {
                    ActionPhase::Repeated
                } else {
                    ActionPhase::Pressed
                }
            }
            _ => return,
        };
        let Some(interaction) = self.projection.interaction else {
            return;
        };
        let current = self.current_stamp();
        let mut binding = self.binding.borrow_mut();
        match phase {
            ActionPhase::Pressed => binding.navigation_down = binding.accepted,
            ActionPhase::Cancelled => binding.navigation_down = None,
            ActionPhase::Released
                if binding
                    .navigation_down
                    .take()
                    .is_some_and(|stamp| stamp.same_advance(current))
                    && binding.permits(current) =>
            {
                self.inputs.borrow_mut().get_or_insert(UiInput::Session {
                    owner: self.source_owner,
                    action: InputAction::Advance(interaction),
                });
            }
            _ => {}
        }
        ctx.set_handled();
    }
    fn on_action(
        &mut self,
        ctx: &mut ActionCtx<'_>,
        _: &mut PropertiesMut<'_>,
        action: &ErasedAction,
        source: WidgetId,
    ) {
        if let Some(reason) = action.downcast_ref::<reel_ui::widgets::PopupDismiss>()
            && let Some(layer) = self.popup_layers.iter().find(|layer| layer.layer == source)
        {
            node_events::admit(
                &self.binding,
                &self.events,
                &reel_ui::stage::NodeEvent::PopupDismiss(reel_ui::stage::NodePopupDismiss {
                    node: layer.node,
                    reason: *reason,
                }),
            );
            ctx.set_handled();
            return;
        }
        if node_events::accept(&self.binding, &self.events, action) {
            ctx.set_handled();
            return;
        }
        if action.downcast_ref::<ButtonPress>().is_some()
            && let Some(choice) = self.choices.iter().find(|row| row.button.id() == source)
        {
            if self.binding.borrow().permits(self.current_stamp()) {
                self.inputs.borrow_mut().get_or_insert(UiInput::Session {
                    owner: choice.owner,
                    action: InputAction::Select(choice.id.clone()),
                });
            }
            ctx.set_handled();
        }
    }
    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        ctx.register_child(&mut self.content);
        if let Some(preview) = &mut self.preview {
            ctx.register_child(&mut preview.backdrop);
            ctx.register_child(&mut preview.title);
            ctx.register_child(&mut preview.status);
        }
        for line in &mut self.lines {
            ctx.register_child(&mut line.speaker);
            ctx.register_child(&mut line.text);
        }
        for choice in &mut self.choices {
            ctx.register_child(&mut choice.button);
        }
    }
    fn update(&mut self, _: &mut UpdateCtx<'_>, _: &mut PropertiesMut<'_>, event: &Update) {
        if matches!(
            event,
            Update::DisabledChanged(true) | Update::StashedChanged(true)
        ) {
            self.scrolling.cancel();
        }
    }
    fn measure(
        &mut self,
        _: &mut MeasureCtx<'_>,
        _: &PropertiesRef<'_>,
        axis: Axis,
        request: LenReq,
        _: Option<Length>,
    ) -> Length {
        match request {
            LenReq::FitContent(space) => space,
            _ => Length::px(if axis == Axis::Horizontal { 640. } else { 420. }),
        }
    }
    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, size: Size) {
        self.viewport = size;
        ctx.set_clip_path(size.to_rect());
        ctx.run_layout(&mut self.content, size);
        ctx.place_child(&mut self.content, Point::ORIGIN);
        if let Some(preview) = &mut self.preview {
            ctx.set_stashed(&mut preview.title, self.authored);
            ctx.set_stashed(&mut preview.status, self.authored);
        }
        let text_width = (size.width - 90.).max(0.);
        let button_width = (size.width - 140.).max(0.);
        let preferred = |width| SizeDef::new(LenDef::Fixed(Length::px(width)), LenDef::MaxContent);
        let mut y = if self.authored { 16. } else { 250. };
        let mut positions = Vec::with_capacity(self.lines.len() + self.choices.len());
        for line in &mut self.lines {
            let speaker = ctx.compute_size(&mut line.speaker, preferred(text_width), size.into());
            let text = ctx.compute_size(&mut line.text, preferred(text_width), size.into());
            let speaker_height = speaker.height.max(24.);
            let text_height = text.height.max(28.);
            ctx.run_layout(&mut line.speaker, Size::new(text_width, speaker_height));
            ctx.run_layout(&mut line.text, Size::new(text_width, text_height));
            positions.push((y, speaker_height));
            y += speaker_height + text_height + 10.;
        }
        for choice in &mut self.choices {
            let preferred =
                ctx.compute_size(&mut choice.button, preferred(button_width), size.into());
            let height = preferred.height.max(45.);
            ctx.run_layout(&mut choice.button, Size::new(button_width, height));
            positions.push((y, height));
            y += height + 10.;
        }
        let origin = if self.authored {
            (size.height - y - 40.).max(24.)
        } else {
            0.
        };
        let bottom = if self.authored {
            origin + y
        } else {
            y.max(380.)
        };
        self.dialogue_rect = Rect::new(
            30.,
            if self.authored { origin + 10. } else { 245. },
            (size.width - 30.).max(30.),
            bottom,
        );
        if self.custom_dialogue {
            self.dialogue_rect = size.to_rect();
        }
        self.content_height = bottom + 40.;
        self.scrolling.layout(
            size,
            Size::new(size.width, self.content_height),
            ScrollAxis::Vertical,
            false,
        );
        self.scroll.set(self.scrolling.offset().y);
        if let Some(preview) = &mut self.preview {
            if !self.authored {
                ctx.run_layout(
                    &mut preview.title,
                    Size::new((size.width - 60.).max(0.), 36.),
                );
                ctx.place_child(&mut preview.title, Point::new(30., 20.));
                ctx.run_layout(
                    &mut preview.status,
                    Size::new((size.width - 60.).max(0.), 30.),
                );
                ctx.place_child(&mut preview.status, Point::new(30., bottom + 5.));
            }
            if let Some((backdrop, mut context)) = ctx.get_raw_mut(&mut preview.backdrop) {
                backdrop.rect =
                    if self.authored && (!self.lines.is_empty() || !self.choices.is_empty()) {
                        self.dialogue_rect
                    } else {
                        Rect::ZERO
                    };
                context.request_paint_only();
            }
            ctx.run_layout(&mut preview.backdrop, size);
            ctx.place_child(&mut preview.backdrop, Point::ORIGIN);
        }
        for (line, &(y, height)) in self.lines.iter_mut().zip(&positions) {
            ctx.place_child(&mut line.speaker, Point::new(45., origin + y));
            ctx.place_child(&mut line.text, Point::new(45., origin + y + height));
        }
        for (choice, &(y, _)) in self.choices.iter_mut().zip(&positions[self.lines.len()..]) {
            ctx.place_child(&mut choice.button, Point::new(70., origin + y));
        }
    }
    fn compose(&mut self, ctx: &mut ComposeCtx<'_>) {
        let offset = -self.scrolling.offset();
        if let Some(preview) = &mut self.preview {
            ctx.set_child_scroll_translation(&mut preview.title, offset);
            ctx.set_child_scroll_translation(&mut preview.status, offset);
            ctx.set_child_scroll_translation(&mut preview.backdrop, offset);
        }
        for line in &mut self.lines {
            ctx.set_child_scroll_translation(&mut line.speaker, offset);
            ctx.set_child_scroll_translation(&mut line.text, offset);
        }
        for choice in &mut self.choices {
            ctx.set_child_scroll_translation(&mut choice.button, offset);
        }
    }
    fn paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, painter: &mut Painter<'_>) {
        painter.fill_rect(self.viewport.to_rect(), self.background);
        if self.authored {
            return;
        }
        let offset = Vec2::new(0., -self.scroll.get());
        painter.fill_rect(
            Rect::new(30., 70., (self.viewport.width - 30.).max(30.), 235.) + offset,
            Color::from_rgb8(122, 151, 157),
        );
        painter.fill_rect(self.dialogue_rect + offset, Color::WHITE);
    }
    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, node: &mut Node) {
        node.set_label("Story; scroll to see all content, click dialogue to reveal then advance, or select a choice");
    }
    fn children_ids(&self) -> ChildrenIds {
        let mut ids = ChildrenIds::from_slice(&[self.content.id()]);
        if let Some(preview) = &self.preview {
            ids.extend([
                preview.backdrop.id(),
                preview.title.id(),
                preview.status.id(),
            ]);
        }
        for line in &self.lines {
            ids.push(line.speaker.id());
            ids.push(line.text.id());
        }
        ids.extend(self.choices.iter().map(|row| row.button.id()));
        ids
    }
    fn accepts_pointer_interaction(&self) -> bool {
        true
    }
}

struct DialogueBackdrop {
    rect: Rect,
}
impl AllowRawMut for DialogueBackdrop {}
impl Widget for DialogueBackdrop {
    type Action = NoAction;
    fn register_children(&mut self, _: &mut RegisterCtx<'_>) {}
    fn measure(
        &mut self,
        _: &mut MeasureCtx<'_>,
        _: &PropertiesRef<'_>,
        _: Axis,
        request: LenReq,
        _: Option<Length>,
    ) -> Length {
        match request {
            LenReq::FitContent(space) => space,
            _ => Length::ZERO,
        }
    }
    fn layout(&mut self, _: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, _: Size) {}
    fn paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, painter: &mut Painter<'_>) {
        painter.fill_rect(self.rect, Color::WHITE);
    }
    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, _: &mut Node) {}
    fn children_ids(&self) -> ChildrenIds {
        ChildrenIds::new()
    }
    fn accepts_pointer_interaction(&self) -> bool {
        false
    }
}

fn allocate_choice_owners(session: &mut Session, count: usize) -> Result<Vec<OwnerHandle>, Error> {
    // A choice projection shares one lifetime. ChoiceId still identifies each row.
    let count = usize::from(count != 0);
    let mut owners = Vec::with_capacity(count);
    for _ in 0..count {
        match session.create_owner() {
            Ok(owner) => owners.push(owner),
            Err(error) => {
                for owner in owners {
                    session.revoke_owner(owner)?;
                }
                return Err(format!("UI choice source owner capacity: {error}").into());
            }
        }
    }
    Ok(owners)
}

/// Owns a real Session and maps accepted UI events to its public host boundary.
/// The future executor adapter can drain Session completions independently.
/// Content uses natural heights and scrolling within the presented viewport.
/// Session resource settings are the only row/choice admission bounds.
pub struct SessionUi {
    session: Session,
    projection: Projection,
    stamp: ProjectionStamp,
    source_owner: OwnerHandle,
    choice_owners: Vec<OwnerHandle>,
    binding: Rc<RefCell<InputBinding>>,
    inputs: Rc<RefCell<Option<UiInput>>>,
    events: Rc<RefCell<Vec<reel_ui::stage::NodeEvent>>>,
    background: Color,
    scroll: Rc<Cell<f64>>,
    stage: Option<StageView>,
    stage_dirty: bool,
    pending_stage: Option<StageView>,
    pending_images_ready: bool,
    image_demand_dirty: bool,
    image_layout_pending: bool,
    image_motion: image_requests::ImageMotion,
    virtual_images_ready: bool,
    pub(crate) images_waiting_presentation: bool,
    images: crate::images::Images,
    playback: playback::Playback,
    preview: bool,
    reading_modal: bool,
    input_fallback: Option<WidgetId>,
    reading_focused: bool,
}
mod image_requests;
mod input_dispatch;

impl SessionUi {
    /// Present the authored stage over an opaque black background.
    pub fn new(session: Session) -> Result<Self, Error> {
        Self::create(session, false)
    }
    /// Explicit session preview used by the native host diagnostic.
    #[cfg(any(test, feature = "window"))]
    fn preview(session: Session) -> Result<Self, Error> {
        Self::create(session, true)
    }
    fn create(mut session: Session, preview: bool) -> Result<Self, Error> {
        let source_owner = session.create_owner()?;
        let projection = Projection::from_session(&session, preview)?;
        let choice_owners = allocate_choice_owners(&mut session, projection.choices.len())?;
        let stamp = ProjectionStamp {
            presentation_request: 0,
            dialogue_complete: dialogue_complete(&session),
            dialogue_revision: dialogue_revision(&session),
            revision: 1,
            advance_revision: 1,
            button_revision: 0,
            interaction: projection.interaction,
            scroll_bits: 0_f64.to_bits(),
        };
        Ok(Self {
            session,
            projection,
            stamp,
            source_owner,
            choice_owners,
            binding: Rc::new(RefCell::new(InputBinding::default())),
            inputs: Rc::new(RefCell::new(None)),
            events: Rc::new(RefCell::new(Vec::new())),
            background: Color::BLACK,
            scroll: Rc::new(Cell::new(0.)),
            stage: None,
            stage_dirty: false,
            pending_stage: None,
            pending_images_ready: false,
            image_demand_dirty: false,
            image_layout_pending: false,
            image_motion: Default::default(),
            virtual_images_ready: false,
            images_waiting_presentation: false,
            playback: Default::default(),
            preview,
            reading_modal: false,
            input_fallback: None,
            reading_focused: true,
            images: crate::images::Images::new(
                crate::assets::Assets::directory(Default::default()),
            ),
        })
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
    /// External stamped input shares the Session watermark with new UI actions.
    /// Call `sync` after model changes, then present the matching frame before
    /// allowing input to the replacement projection.
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }
    pub fn widget(&self) -> SessionWidget {
        SessionWidget::new(self)
    }
    pub fn set_stage(&mut self, stage: &reel_ui::stage::Stage) {
        self.update_stage(stage, false);
    }
    /// Input feedback changes appearance without replacing the accepted button action.
    pub(crate) fn set_feedback_stage(&mut self, stage: &reel_ui::stage::Stage) {
        self.update_stage(stage, true);
    }
    pub(crate) fn permits_virtual_feedback(&self, stage: &reel_ui::stage::Stage) -> bool {
        self.pending_stage
            .as_ref()
            .is_none_or(|pending| pending.permits_feedback(stage))
            && self
                .stage
                .as_ref()
                .is_some_and(|view| view.permits_feedback(stage))
    }
    pub(crate) fn virtual_feedback_visible(
        &self,
        bridge: &mut PresentationBridge,
        root: WidgetId,
    ) -> Result<bool, Error> {
        let geometry = bridge
            .layout_geometry()
            .map_err(|error| format!("scroll layout geometry: {error:?}"))?;
        Ok(self
            .stage
            .as_ref()
            .is_some_and(|view| self.virtual_popup_images_ready(bridge, root, view, &geometry)))
    }
    pub(crate) fn current_presentation_ready(&self) -> bool {
        !self.images_pending()
            && !self.images_waiting_presentation
            && self.binding.borrow().accepted == Some(self.projection_stamp())
    }
    fn update_stage(&mut self, stage: &reel_ui::stage::Stage, feedback: bool) {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::UiStageView);
        self.virtual_images_ready = false;
        if self
            .pending_stage
            .as_ref()
            .or(self.stage.as_ref())
            .is_none_or(|view| !view.matches(stage))
        {
            reel_core::profile::count(reel_core::profile::Phase::ProjectionChanged);
            let mut view =
                StageView::update(stage, self.pending_stage.as_ref().or(self.stage.as_ref()));
            view.presentation_request = self
                .pending_stage
                .as_ref()
                .or(self.stage.as_ref())
                .map_or(0, |previous| previous.presentation_request);
            view.feedback = feedback
                && self
                    .pending_stage
                    .as_ref()
                    .is_none_or(|pending| pending.feedback);
            let scope = view.input_scope();
            if self
                .pending_stage
                .as_ref()
                .or(self.stage.as_ref())
                .is_some_and(|previous| !previous.same_stage(&view))
            {
                self.image_motion = Default::default();
            }
            let scope_changed = self
                .pending_stage
                .as_ref()
                .or(self.stage.as_ref())
                .is_none_or(|previous| {
                    !previous.same_stage(&view) || previous.input_scope() != scope
                });
            let modal = scope.1.is_some();
            if modal {
                self.session.set_skip_reading(false, false, false);
            }
            if modal != self.reading_modal {
                self.pause_dialogue();
                self.reading_modal = modal;
            }
            let demand_changed = self
                .pending_stage
                .as_ref()
                .or(self.stage.as_ref())
                .is_none_or(|previous| !view.same_image_demand(previous));
            if demand_changed {
                self.image_demand_dirty = true;
                self.image_layout_pending = false;
            }
            self.pending_images_ready = !self.image_demand_dirty
                && (self.pending_stage.is_none() || self.pending_images_ready);
            let mut binding = self.binding.borrow_mut();
            if scope_changed {
                self.stamp.advance_revision = self.stamp.advance_revision.wrapping_add(1);
                binding.accepted = None;
            }
            if let Some(buttons) = view.button_changes(
                self.pending_stage.as_ref().or(self.stage.as_ref()),
                view.feedback,
            ) {
                if buttons.iter().any(|(_, changed)| *changed) {
                    binding.button_revision = binding.button_revision.wrapping_add(1);
                }
                let revision = binding.button_revision;
                binding.buttons = buttons
                    .into_iter()
                    .map(|(id, changed)| {
                        let version = if changed {
                            revision
                        } else {
                            binding.buttons.get(&id).copied().unwrap_or(revision)
                        };
                        (id, version)
                    })
                    .collect();
            }
            view.button_revision = binding.button_revision;
            self.pending_stage = Some(view);
            self.images_waiting_presentation = true;
            // Stable control identities retain gestures across value/layout changes.
            // Scene retirement, configuration and modal changes are checked by routing.
        }
    }
    /// Configure before creating a widget or driving the story.
    pub fn with_asset_options(mut self, options: crate::assets::AssetOptions) -> Self {
        let format = self.images.shader_format;
        self.images = crate::images::Images::new(crate::assets::Assets::directory(options));
        self.images.shader_format = format;
        self.image_demand_dirty = self.pending_stage.is_some() || self.stage.is_some();
        self.image_layout_pending = false;
        self
    }
    pub fn with_assets(mut self, assets: crate::assets::Assets) -> Self {
        self.set_assets(assets);
        self
    }
    pub(crate) fn set_assets(&mut self, assets: crate::assets::Assets) {
        let format = self.images.shader_format;
        self.images = crate::images::Images::new(assets);
        self.images.shader_format = format;
        self.image_demand_dirty = self.pending_stage.is_some() || self.stage.is_some();
        self.image_layout_pending = false;
    }
    pub(crate) fn image_restore_diagnostic(
        &self,
        lease: &reel_ui::resources::ResourceLease,
        error: &str,
    ) -> String {
        self.images.restore_diagnostic(lease, error)
    }
    pub fn assets(&self) -> crate::assets::Assets {
        self.images.assets.clone()
    }
    /// Select the prepared fragment representation used by this presentation host.
    pub fn set_effect_shader_format(&mut self, format: reel_ui::resources::EffectShaderFormat) {
        if self.images.shader_format != format {
            self.images.shader_format = format;
            self.image_demand_dirty = self.pending_stage.is_some() || self.stage.is_some();
            self.image_layout_pending = false;
        }
    }
    pub(crate) fn adopt_presentation(&mut self) {
        self.clear_presented();
        self.stage_dirty = true;
    }
    /// Called by resource adapters when a terminal result is ready to poll.
    pub fn set_resource_waker(&mut self, wake: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.images.set_waker(wake);
    }
    pub fn images_pending(&self) -> bool {
        self.pending_stage.is_some() || self.image_demand_dirty
    }
    pub(crate) fn images_block_presentation(&self) -> bool {
        self.images_pending() && !self.virtual_images_ready
    }
    /// Bind the request to the candidate carrying the stable authored state.
    /// Pending image work must not attach it to the retained previous frame.
    pub(crate) fn request_presentation(&mut self, request: u64) {
        if let Some(view) = &mut self.pending_stage {
            view.presentation_request = request;
        } else if let Some(view) = &mut self.stage
            && view.presentation_request != request
        {
            view.presentation_request = request;
            self.stage_dirty = true;
        }
    }
    pub(crate) fn has_stage_root(&self) -> bool {
        self.stage.as_ref().is_some_and(StageView::has_root)
    }
    pub(crate) fn images_need_poll(&self) -> bool {
        self.images_pending() || self.images.has_result_pending()
    }
    pub(crate) fn set_image_prediction(&mut self, sources: &[Box<str>]) {
        self.images.set_image_prediction(sources);
    }
    /// Immutable leases for the currently published stage, suitable for a frame snapshot.
    pub fn image_resources(&self) -> &[reel_ui::resources::ResourceLease] {
        self.stage.as_ref().map_or(&[], |view| &view.resources)
    }
    pub(crate) fn close_resources(&mut self) {
        self.pending_stage = None;
        self.pending_images_ready = false;
        self.image_demand_dirty = false;
        self.image_layout_pending = false;
        self.virtual_images_ready = false;
        self.images_waiting_presentation = false;
        self.images.close();
        self.stage = None;
        self.stage_dirty = true;
        self.clear_presented();
    }
    pub fn projection_stamp(&self) -> ProjectionStamp {
        ProjectionStamp {
            scroll_bits: self.scroll.get().to_bits(),
            ..self.stamp
        }
    }
    pub(crate) fn invalidate_projection(&mut self) {
        self.stage_dirty = true;
    }
    pub(crate) fn gesture_active(&self) -> bool {
        let binding = self.binding.borrow();
        !binding.contacts.is_empty() || binding.navigation_down.is_some()
    }
    pub fn clear_presented(&mut self) {
        self.pause_dialogue();
        self.image_demand_dirty = self.pending_stage.is_some() || self.stage.is_some();
        self.image_layout_pending = false;
        let mut binding = self.binding.borrow_mut();
        *binding = InputBinding {
            button_revision: binding.button_revision,
            buttons: std::mem::take(&mut binding.buttons),
            ..Default::default()
        };
    }
    fn status_text(&self) -> String {
        format!(
            "{:?} | history: {} | scroll for more; click dialogue twice; close window to exit",
            self.projection.status, self.projection.history_count
        )
    }
    pub fn sync(
        &mut self,
        bridge: &mut PresentationBridge,
        root_id: WidgetId,
    ) -> Result<(), Error> {
        bridge.set_focus_fallback(Some(self.input_fallback.unwrap_or(root_id)));
        self.virtual_images_ready = false;
        let demand_changed = self.image_demand_dirty;
        let reuse_images = std::mem::take(&mut self.pending_images_ready) && !demand_changed;
        let content_revision = self.images.content_revision;
        if !self.prepare_image_requests(bridge, root_id)? {
            self.images_waiting_presentation = true;
            self.wait_for_image_presentation();
            return Ok(());
        }
        // Geometry may change while the viewport still requests the same loaded
        // sources. Reuse the candidate's inherited leases after that check.
        let reuse_images = std::mem::take(&mut self.pending_images_ready) || reuse_images;
        let images_ready = self.images.poll()?;
        let content_changed = content_revision != self.images.content_revision;
        if reuse_images && images_ready && !content_changed {
            // Keep authored/feedback handoff queued while sharing prepared content.
            if let Some(view) = self.pending_stage.take() {
                self.stage = Some(view);
                self.stage_dirty = true;
            }
        } else if self.images.metadata_ready() && !self.images.manifests.is_empty() {
            if (demand_changed || content_changed || self.pending_stage.is_some())
                && !self.prepare_tiles(bridge, root_id, content_changed)?
            {
                self.wait_for_image_presentation();
                return Ok(());
            }
        } else if images_ready {
            if let Some(mut view) = self.pending_stage.take() {
                view.resolve_metadata(&self.images)?;
                self.stage = Some(view);
                self.stage_dirty = true;
                self.images.commit();
            }
        } else if self.images_pending() {
            self.images_waiting_presentation = true;
            if let Some(candidate) = self.pending_stage.as_ref()
                && self.stage.as_ref().is_some_and(|current| {
                    candidate.same_stage(current)
                        && candidate.input_scope() == current.input_scope()
                })
                && self.images.manifests.is_empty()
                && candidate.can_layout_virtual_images(&self.images.loaded)
            {
                let mut view = candidate.clone();
                view.resolve_metadata(&self.images)?;
                bridge.edit_widget(Self::stage_widget(bridge, root_id).unwrap(), |mut stage| {
                    StageWidget::sync(&mut stage.downcast::<StageWidget>(), &view);
                });
                self.stage = Some(view);
                self.stage_dirty = true;
                self.sync_popups(bridge, root_id);
                let geometry = bridge
                    .layout_geometry()
                    .map_err(|error| format!("scroll layout geometry: {error:?}"))?;
                self.virtual_images_ready = self.virtual_popup_images_ready(
                    bridge,
                    root_id,
                    self.pending_stage.as_ref().unwrap(),
                    &geometry,
                );
            }
            self.wait_for_image_presentation();
            if !self.virtual_images_ready {
                return Ok(());
            }
        }
        let view = self.session.view();
        let interaction = view.interaction.map(|i| i.id());
        let mut changed = view.status != self.projection.status
            || view.history.len() != self.projection.history_count;
        let dialogue_revision = dialogue_revision(&self.session);
        if interaction != self.projection.interaction
            || dialogue_revision != self.stamp.dialogue_revision
        {
            let projection = Projection::from_session(&self.session, self.preview)?;
            let replacement =
                match allocate_choice_owners(&mut self.session, projection.choices.len()) {
                    Ok(owners) => owners,
                    Err(error) => {
                        self.clear_presented();
                        return Err(error);
                    }
                };
            for owner in std::mem::replace(&mut self.choice_owners, replacement) {
                self.session.revoke_owner(owner)?;
            }
            self.projection = projection;
            changed = true;
        } else {
            self.projection.status = view.status;
            self.projection.history_count = view.history.len();
            if let Some(InteractionView::Dialogue(dialogue)) = view.interaction {
                for (row, line) in self.projection.lines.iter_mut().zip(dialogue.lines()) {
                    let visible = line.reveal().visible_bytes;
                    changed |= row.visible != visible;
                    row.visible = visible;
                }
            }
        }
        let complete = dialogue_complete(&self.session);
        changed |= complete != self.stamp.dialogue_complete;
        if changed {
            self.stamp.advance_revision = self.stamp.advance_revision.wrapping_add(1);
            self.binding.borrow_mut().accepted = None;
        }
        if changed || self.stage_dirty {
            reel_core::profile::count(reel_core::profile::Phase::ProjectionPublished);
            self.stamp.dialogue_complete = complete;
            self.stamp.dialogue_revision = dialogue_revision;
            self.stamp.revision = self
                .stamp
                .revision
                .checked_add(1)
                .ok_or("UI projection revision exhausted")?;
            self.stamp.interaction = self.projection.interaction;
            self.stamp.button_revision = self.stage.as_ref().map_or(0, |view| view.button_revision);
            self.stamp.presentation_request = self
                .stage
                .as_ref()
                .map_or(0, |view| view.presentation_request);
            let stage_id = bridge.edit_widget(root_id, |mut widget| {
                let mut widget = widget.downcast::<SessionWidget>();
                SessionWidget::sync(&mut widget, self);
                widget.widget.stage
            });
            self.input_fallback = bridge
                .root()
                .get_widget(stage_id)
                .and_then(|stage| stage.downcast::<StageWidget>())
                .and_then(|stage| stage.input_fallback(bridge.root()));
            bridge.set_focus_fallback(Some(self.input_fallback.unwrap_or(root_id)));
            self.stage_dirty = false;
        }
        self.sync_popups(bridge, root_id);
        Ok(())
    }

    pub fn take_events(&mut self) -> Vec<reel_ui::stage::NodeEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }
    pub(crate) fn recycle_events(&mut self, mut events: Vec<reel_ui::stage::NodeEvent>) {
        events.clear();
        if self.events.borrow().is_empty() {
            *self.events.borrow_mut() = events;
        }
    }
    pub fn cancel(&mut self) -> bool {
        self.close_resources();
        self.clear_presented();
        self.session.cancel()
    }
}

/// A small explicit Rust caller, exercising host/UI contracts without running a
/// language program, copied executor, bytecode, save system or VM interface.
#[cfg(feature = "window")]
pub(crate) struct SessionHostDiagnostic {
    pub(crate) ui: SessionUi,
    step: u8,
}
#[cfg(feature = "window")]
impl SessionHostDiagnostic {
    #[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
    pub(crate) fn diagnostic_step(&self) -> u8 {
        self.step
    }
    pub(crate) fn new() -> Result<Self, Error> {
        let mut session = Session::new(SessionId(1), Epoch(1))?;
        session.begin_say(
            session.root_owner(),
            &[DialogueLineInput {
                history: Default::default(),
                speaker_id: Some("host"),
                speaker_name: "Host",
                content_id: "host-diagnostic/first",
                text: ("This window displays a real Rust session through the existing Reel UI.")
                    .into(),
            }],
            "en",
            true,
        )?;
        Ok(Self {
            ui: SessionUi::preview(session)?,
            step: 0,
        })
    }
    pub(crate) fn after_input(
        &mut self,
        bridge: &mut PresentationBridge,
        root_id: WidgetId,
    ) -> Result<(), Error> {
        if let Some(completion) = self.ui.session.take_completion() {
            match (self.step, completion.outcome()) {
                (0, CompletionOutcome::Advanced) => {
                    self.ui.session.begin_say(self.ui.session.root_owner(), &[DialogueLineInput {
                        history: Default::default(),
                        speaker_id: Some("host"), speaker_name: "Host", content_id: "host-diagnostic/second",
                        text: ("The first click reveals the full line. The next click settles this interaction once.").into(),
                    }], "en", true)?;
                    self.step = 1;
                }
                (1, CompletionOutcome::Advanced) => {
                    self.ui.session.begin_choice(
                        self.ui.session.root_owner(),
                        &[
                            ChoiceInput {
                                key: "window",
                                label: "Walk to the window",
                            },
                            ChoiceInput {
                                key: "door",
                                label: "Stay by the door",
                            },
                        ],
                    )?;
                    self.step = 2;
                }
                (2, CompletionOutcome::Selected(key)) => {
                    self.ui.background = if key == "window" {
                        Color::from_rgb8(57, 105, 94)
                    } else {
                        Color::from_rgb8(104, 77, 91)
                    };
                    self.ui.session.end()?;
                    self.step = 3;
                }
                (_, CompletionOutcome::Cancelled(_)) => {
                    self.step = 3;
                }
                _ => return Err("unexpected Rust host diagnostic completion".into()),
            }
        }
        self.ui.sync(bridge, root_id)
    }
}

#[cfg(test)]
pub(crate) mod tests;
