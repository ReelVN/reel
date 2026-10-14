//! Real Rust session projection into Reel's maintained UI and presented input path.
//!
//! This layer never executes Aside. The optional native diagnostic below drives
//! explicit host requests and must not be described as a source story runner.
mod playback;
use crate::stage_widget::{StageView, StageWidget};
#[cfg(test)]
mod button_tests;
#[cfg(test)]
mod hal_tests;
#[cfg(test)]
mod image_tests;
use reel_engine::{
    ChoiceId, InputAction, InputRejection, InteractionId, InteractionView, OwnerHandle, Session,
};
#[cfg(any(test, feature = "window"))]
use reel_engine::{ChoiceInput, CompletionOutcome, DialogueLineInput, Epoch, SessionId};
#[cfg(test)]
use reel_engine::{InputEvent, InputSequence};
use reel_ui::{
    accesskit::{Node, Role},
    bridge::PresentationBridge,
    core::*,
    imaging::Painter,
    kurbo::{Axis, Point, Rect, Size, Vec2},
    layout::{LenDef, LenReq, Length, SizeDef},
    peniko::Color,
    widgets::{Button, ButtonPress, Label},
};
use std::{
    cell::{Cell, RefCell},
    ops::Range,
    rc::Rc,
};

type Error = Box<dyn std::error::Error>;

/// An immutable binding to the content recorded in one frame. Hover and capture
/// paints preserve this revision; changing interaction or reveal increments it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectionStamp {
    revision: u64,
    button_revision: u64,
    interaction: Option<InteractionId>,
    scroll_bits: u64,
}
#[derive(Default)]
struct InputBinding {
    accepted: Option<ProjectionStamp>,
    control_accepted: Option<ProjectionStamp>,
    down: Option<ProjectionStamp>,
    contacts: Vec<(PointerInfo, ProjectionStamp, Option<u64>)>,
    button_revision: u64,
    accepted_buttons: Option<u64>,
    button_down: Option<u64>,
    non_pointer: bool,
    navigation_down: Option<ProjectionStamp>,
}
impl InputBinding {
    fn permits_control(&self) -> bool {
        self.control_accepted.is_some() && (self.down.is_some() || self.non_pointer)
    }
    fn permits_button(&self) -> bool {
        self.accepted_buttons == Some(self.button_revision)
            && (self.button_down == Some(self.button_revision) || self.non_pointer)
    }
    fn permits(&self, current: ProjectionStamp) -> bool {
        self.accepted == Some(current) && (self.down == Some(current) || self.non_pointer)
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
    fn from_session(
        session: &Session,
        default_dialogue: bool,
        default_choices: bool,
    ) -> Result<Self, Error> {
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
                if !default_dialogue {
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
                if !default_choices {
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
struct LineWidgets {
    speaker: WidgetPod<Label>,
    text: WidgetPod<Label>,
}
struct ChoiceWidget {
    id: ChoiceId,
    owner: OwnerHandle,
    button: WidgetPod<Button>,
}

/// The actual UI tree: full-text Parley labels and the existing real Button widget.
pub struct SessionWidget {
    projection: Projection,
    stamp: ProjectionStamp,
    source_owner: OwnerHandle,
    binding: Rc<RefCell<InputBinding>>,
    inputs: Rc<RefCell<Option<UiInput>>>,
    events: Rc<RefCell<Vec<reel_ui::stage::NodeEvent>>>,
    title: WidgetPod<Label>,
    status: WidgetPod<Label>,
    lines: Vec<LineWidgets>,
    choices: Vec<ChoiceWidget>,
    background: Color,
    scroll: Rc<Cell<f64>>,
    content_height: f64,
    viewport: Size,
    dialogue_rect: Rect,
    stage: WidgetPod<StageWidget>,
    backdrop: WidgetPod<DialogueBackdrop>,
    authored: bool,
}
impl SessionWidget {
    fn new(ui: &SessionUi) -> Self {
        let mut widget = Self {
            projection: ui.projection.clone(),
            stamp: ui.stamp,
            source_owner: ui.source_owner,
            binding: ui.binding.clone(),
            inputs: ui.inputs.clone(),
            events: ui.events.clone(),
            title: WidgetPod::new(Label::new("Reel")),
            status: WidgetPod::new(Label::new(ui.status_text())),
            lines: vec![],
            choices: vec![],
            background: ui.background,
            scroll: ui.scroll.clone(),
            content_height: 420.,
            viewport: Size::new(640., 420.),
            dialogue_rect: Rect::new(30., 245., 610., 380.),
            stage: WidgetPod::new(StageWidget::new(ui.stage.as_ref())),
            backdrop: WidgetPod::new(DialogueBackdrop { rect: Rect::ZERO }),
            authored: ui.stage.is_some(),
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
        let authored_changed = this.widget.authored != ui.stage.is_some();
        this.widget.authored = ui.stage.is_some();
        if ui.stage_dirty
            && let Some(mut widget) = this.ctx.get_mut(&mut this.widget.stage)
        {
            StageWidget::sync(
                &mut widget,
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
        if status_changed && let Some(mut status) = this.ctx.get_mut(&mut this.widget.status) {
            Label::set_text(&mut status, ui.status_text());
        }
        if !same_interaction || status_changed || authored_changed {
            this.ctx.request_layout();
        }
    }
}
impl Widget for SessionWidget {
    type Action = NoAction;
    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if let PointerEvent::Scroll(event) = event {
            let delta = match event.delta {
                ScrollDelta::LineDelta(_, y) => f64::from(y) * 40.,
                ScrollDelta::PageDelta(_, y) => f64::from(y) * self.viewport.height,
                ScrollDelta::PixelDelta(p) => p.y / ctx.scale_factor(),
            };
            if delta.is_finite() {
                let offset = (self.scroll.get() + delta)
                    .clamp(0., (self.content_height - self.viewport.height).max(0.));
                if offset != self.scroll.get() {
                    self.scroll.set(offset);
                    let mut binding = self.binding.borrow_mut();
                    *binding = InputBinding {
                        button_revision: binding.button_revision,
                        ..Default::default()
                    };
                    ctx.request_layout();
                    ctx.set_handled();
                }
            }
            return;
        }
        // Choice controls own their click. It cannot fall through into a dialogue
        // that is installed after this dispatch completes.
        if !self.choices.is_empty() {
            return;
        }
        let Some(interaction) = self.projection.interaction else {
            return;
        };
        match event {
            PointerEvent::Down(e)
                if e.button.is_none_or(|b| b == PointerButton::Primary)
                    && ctx.target() == ctx.widget_id()
                    && self.dialogue_rect.contains(
                        ctx.local_position(e.state.position) + Vec2::new(0., self.scroll.get()),
                    ) =>
            {
                ctx.capture_pointer();
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
        use reel_ui::input::{ActionPhase, NavigationAction};
        let TextEvent::Navigation(event) = event else {
            return;
        };
        if event.action != NavigationAction::Confirm || !self.choices.is_empty() {
            return;
        }
        let Some(interaction) = self.projection.interaction else {
            return;
        };
        let current = self.current_stamp();
        let mut binding = self.binding.borrow_mut();
        match event.phase {
            ActionPhase::Pressed => binding.navigation_down = binding.accepted,
            ActionPhase::Cancelled => binding.navigation_down = None,
            ActionPhase::Released
                if binding.navigation_down.take() == Some(current) && binding.permits(current) =>
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
        if let Some(event) = action.downcast_ref::<reel_ui::stage::NodeEvent>() {
            use reel_ui::{input::ActionPhase, stage::NodeEvent};
            match event {
                NodeEvent::Click(click) if self.binding.borrow().permits_button() => {
                    self.events.borrow_mut().push(NodeEvent::Click(*click));
                }
                NodeEvent::Change(change) if self.binding.borrow().permits_control() => {
                    self.events.borrow_mut().push(NodeEvent::Change(*change));
                }
                NodeEvent::Action(action)
                    if matches!(action.phase, ActionPhase::Released | ActionPhase::Cancelled)
                        || self.binding.borrow().permits_control() =>
                {
                    self.events
                        .borrow_mut()
                        .push(NodeEvent::Action(action.clone()));
                }
                NodeEvent::Interaction(state) => {
                    self.events
                        .borrow_mut()
                        .push(NodeEvent::Interaction(*state));
                }
                _ => {}
            }
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
        ctx.register_child(&mut self.stage);
        ctx.register_child(&mut self.backdrop);
        ctx.register_child(&mut self.title);
        ctx.register_child(&mut self.status);
        for line in &mut self.lines {
            ctx.register_child(&mut line.speaker);
            ctx.register_child(&mut line.text);
        }
        for choice in &mut self.choices {
            ctx.register_child(&mut choice.button);
        }
    }
    fn update(&mut self, ctx: &mut UpdateCtx<'_>, _: &mut PropertiesMut<'_>, event: &Update) {
        let Update::RequestPanToChild(rect) = event else {
            return;
        };
        if !self
            .choices
            .iter()
            .any(|choice| Some(choice.button.id()) == ctx.focus_target_id())
        {
            return;
        }
        let delta = if rect.y0 < 0. {
            rect.y0
        } else {
            (rect.y1 - self.viewport.height).max(0.)
        };
        let offset = (self.scroll.get() + delta)
            .clamp(0., (self.content_height - self.viewport.height).max(0.));
        if offset != self.scroll.get() {
            self.scroll.set(offset);
            let mut binding = self.binding.borrow_mut();
            *binding = InputBinding {
                button_revision: binding.button_revision,
                ..Default::default()
            };
            ctx.request_layout();
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
        ctx.run_layout(&mut self.stage, size);
        ctx.place_child(&mut self.stage, Point::ORIGIN);
        ctx.set_stashed(&mut self.title, self.authored);
        ctx.set_stashed(&mut self.status, self.authored);
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
        self.content_height = bottom + 40.;
        let offset = self
            .scroll
            .get()
            .clamp(0., (self.content_height - size.height).max(0.));
        self.scroll.set(offset);
        if !self.authored {
            ctx.run_layout(&mut self.title, Size::new((size.width - 60.).max(0.), 36.));
            ctx.place_child(&mut self.title, Point::new(30., 20. - offset));
            ctx.run_layout(&mut self.status, Size::new((size.width - 60.).max(0.), 30.));
            ctx.place_child(&mut self.status, Point::new(30., bottom + 5. - offset));
        }
        if let Some((backdrop, mut context)) = ctx.get_raw_mut(&mut self.backdrop) {
            backdrop.rect = if self.authored && self.projection.interaction.is_some() {
                self.dialogue_rect + Vec2::new(0., -offset)
            } else {
                Rect::ZERO
            };
            context.request_paint_only();
        }
        ctx.run_layout(&mut self.backdrop, size);
        ctx.place_child(&mut self.backdrop, Point::ORIGIN);
        for (line, &(y, height)) in self.lines.iter_mut().zip(&positions) {
            ctx.place_child(&mut line.speaker, Point::new(45., origin + y - offset));
            ctx.place_child(
                &mut line.text,
                Point::new(45., origin + y + height - offset),
            );
        }
        for (choice, &(y, _)) in self.choices.iter_mut().zip(&positions[self.lines.len()..]) {
            ctx.place_child(&mut choice.button, Point::new(70., origin + y - offset));
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
        let mut ids = ChildrenIds::from_slice(&[
            self.stage.id(),
            self.backdrop.id(),
            self.title.id(),
            self.status.id(),
        ]);
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
    images: crate::images::Images,
    playback: playback::Playback,
    default_dialogue: bool,
    default_choices: bool,
    narrative_dirty: bool,
}
impl SessionUi {
    pub fn new(mut session: Session) -> Result<Self, Error> {
        let source_owner = session.create_owner()?;
        let projection = Projection::from_session(&session, true, true)?;
        let choice_owners = allocate_choice_owners(&mut session, projection.choices.len())?;
        let stamp = ProjectionStamp {
            revision: 1,
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
            background: Color::from_rgb8(35, 54, 74),
            scroll: Rc::new(Cell::new(0.)),
            stage: None,
            stage_dirty: false,
            pending_stage: None,
            playback: Default::default(),
            default_dialogue: true,
            default_choices: true,
            narrative_dirty: false,
            images: crate::images::Images::new(
                crate::assets::Assets::directory(Default::default()),
            ),
        })
    }
    pub fn set_default_views(&mut self, dialogue: bool, choices: bool) {
        if self.default_dialogue != dialogue || self.default_choices != choices {
            self.default_dialogue = dialogue;
            self.default_choices = choices;
            self.narrative_dirty = true;
        }
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
    fn update_stage(&mut self, stage: &reel_ui::stage::Stage, feedback: bool) {
        if self
            .pending_stage
            .as_ref()
            .or(self.stage.as_ref())
            .is_none_or(|view| view.revision != stage.revision())
        {
            let view = StageView::capture(stage);
            self.images.request(view.image_requests());
            self.pending_stage = Some(view);
            let mut binding = self.binding.borrow_mut();
            binding.accepted = None;
            if !feedback {
                binding.button_revision = binding.button_revision.wrapping_add(1);
                binding.accepted_buttons = None;
            }
            // Stable control identities retain gestures across value/layout changes.
            // Scene retirement, configuration and modal changes are checked by routing.
        }
    }
    /// Configure before creating a widget or driving the story.
    pub fn with_asset_options(mut self, options: crate::assets::AssetOptions) -> Self {
        self.images = crate::images::Images::new(crate::assets::Assets::directory(options));
        if let Some(view) = &self.pending_stage {
            self.images.request(view.image_requests());
        }
        self
    }
    pub fn with_assets(mut self, assets: crate::assets::Assets) -> Self {
        self.images = crate::images::Images::new(assets);
        if let Some(view) = &self.pending_stage {
            self.images.request(view.image_requests());
        }
        self
    }
    pub fn assets(&self) -> crate::assets::Assets {
        self.images.assets.clone()
    }
    /// Called by resource adapters when a terminal result is ready to poll.
    pub fn set_resource_waker(&mut self, wake: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.images.set_waker(wake);
    }
    pub fn images_pending(&self) -> bool {
        self.pending_stage.is_some()
    }
    #[cfg(feature = "window")]
    pub(crate) fn images_need_poll(&self) -> bool {
        self.images_pending() || self.images.has_result_pending()
    }
    /// Immutable leases for the currently published stage, suitable for a frame snapshot.
    pub fn image_resources(&self) -> &[reel_ui::resources::ResourceLease] {
        self.stage.as_ref().map_or(&[], |view| &view.resources)
    }
    #[cfg(feature = "window")]
    pub(crate) fn prepare_assets(&mut self) -> Result<bool, Error> {
        Ok(self.images.poll()?)
    }
    pub(crate) fn close_resources(&mut self) {
        self.pending_stage = None;
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
    /// Call only after PresentationBridge accepted the matching actual frame.
    pub fn presented(&mut self, stamp: ProjectionStamp) {
        let mut binding = self.binding.borrow_mut();
        binding.control_accepted = Some(stamp);
        if stamp.button_revision == binding.button_revision {
            binding.accepted_buttons = Some(binding.button_revision);
        }
        if !self.images_pending() {
            binding.accepted = Some(stamp);
        }
    }
    pub(crate) fn gesture_active(&self) -> bool {
        let binding = self.binding.borrow();
        !binding.contacts.is_empty() || binding.navigation_down.is_some()
    }
    pub fn clear_presented(&mut self) {
        self.pause_dialogue();
        let mut binding = self.binding.borrow_mut();
        *binding = InputBinding {
            button_revision: binding.button_revision,
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
        bridge.set_focus_fallback(Some(root_id));
        if self.images.poll()? {
            if let Some(mut view) = self.pending_stage.take() {
                view.resolve_images(&self.images.loaded);
                self.stage = Some(view);
                self.stage_dirty = true;
                self.images.commit();
            }
        } else if self.images_pending() {
            return Ok(());
        }
        let view = self.session.view();
        let interaction = view.interaction.map(|i| i.id());
        let mut changed = view.status != self.projection.status
            || view.history.len() != self.projection.history_count;
        if interaction != self.projection.interaction || self.narrative_dirty {
            let projection = Projection::from_session(
                &self.session,
                self.default_dialogue,
                self.default_choices,
            )?;
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
            self.narrative_dirty = false;
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
        if changed || self.stage_dirty {
            self.stamp.revision = self
                .stamp
                .revision
                .checked_add(1)
                .ok_or("UI projection revision exhausted")?;
            self.stamp.interaction = self.projection.interaction;
            self.stamp.button_revision = self.binding.borrow().button_revision;
            bridge.edit_widget(root_id, |mut widget| {
                SessionWidget::sync(&mut widget.downcast::<SessionWidget>(), self)
            });
            self.stage_dirty = false;
        }
        Ok(())
    }
    pub fn handle_pointer(
        &mut self,
        bridge: &mut PresentationBridge,
        event: PointerEvent,
    ) -> Result<UiDispatch, Error> {
        if matches!(
            event,
            PointerEvent::Down(_) | PointerEvent::Up(_) | PointerEvent::Cancel(_)
        ) {
            self.pause_dialogue();
        }
        let pointer = pointer_info(&event);
        let terminal = matches!(event, PointerEvent::Up(_) | PointerEvent::Cancel(_));
        {
            let mut binding = self.binding.borrow_mut();
            if matches!(event, PointerEvent::Down(_))
                && let Some(stamp) = binding.control_accepted
            {
                let buttons = binding.accepted_buttons;
                if let Some((_, down, button_down)) = binding
                    .contacts
                    .iter_mut()
                    .find(|(info, _, _)| *info == pointer)
                {
                    *down = stamp;
                    *button_down = buttons;
                } else {
                    binding.contacts.push((pointer, stamp, buttons));
                }
            }
            binding.down = binding
                .contacts
                .iter()
                .find(|(info, _, _)| *info == pointer)
                .map(|(_, stamp, _)| *stamp);
            binding.button_down = binding
                .contacts
                .iter()
                .find(|(info, _, _)| *info == pointer)
                .and_then(|(_, _, buttons)| *buttons);
        }
        let result = bridge.handle_pointer_event(event);
        {
            let mut binding = self.binding.borrow_mut();
            binding.down = None;
            binding.button_down = None;
            if terminal || result.is_err() {
                binding.contacts.retain(|(info, _, _)| *info != pointer);
            }
        }
        let handled = result?;
        self.finish_input(handled)
    }
    pub fn handle_text(
        &mut self,
        bridge: &mut PresentationBridge,
        event: TextEvent,
    ) -> Result<UiDispatch, Error> {
        let was_held = self.gesture_active();
        self.binding.borrow_mut().non_pointer = true;
        let lost_focus = matches!(event, TextEvent::WindowFocusChange(false));
        let handled = bridge.handle_text_event(event);
        self.binding.borrow_mut().non_pointer = false;
        if was_held || self.gesture_active() {
            self.pause_dialogue();
        }
        if lost_focus {
            let mut binding = self.binding.borrow_mut();
            binding.down = None;
            binding.contacts.clear();
        }
        self.finish_input(handled)
    }
    /// Submit an accessibility action from the host through accepted UI geometry.
    pub fn handle_access(
        &mut self,
        bridge: &mut PresentationBridge,
        event: reel_ui::accesskit::ActionRequest,
    ) -> Result<UiDispatch, Error> {
        self.binding.borrow_mut().non_pointer = true;
        let handled = bridge.handle_access_event(event);
        self.binding.borrow_mut().non_pointer = false;
        self.finish_input(handled)
    }
    fn finish_input(&mut self, handled: Handled) -> Result<UiDispatch, Error> {
        if let Some(UiInput::Session { owner, action }) = self.inputs.borrow_mut().take() {
            // Owner/session cancellation may race the final UI dispatch. Host
            // rejections filter that event; they are not window failures.
            // Assign the new event at final dispatch, after any external input
            // through session_mut. Already-sequenced replays use handle_input.
            if self.session.handle_local_input(owner, action)
                == Err(InputRejection::SequenceExhausted)
            {
                return Err("input sequence exhausted".into());
            }
        }
        let events = self.take_events();
        if events
            .iter()
            .any(|event| matches!(event, reel_ui::stage::NodeEvent::Change(_)))
        {
            // Restore authoritative control values even after a rejected proposal.
            self.stage_dirty = true;
        }
        Ok(UiDispatch { handled, events })
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
                speaker_id: Some("host"),
                speaker_name: "Host",
                content_id: "host-diagnostic/first",
                text: "This window displays a real Rust session through the existing Reel UI.",
            }],
            "en",
        )?;
        Ok(Self {
            ui: SessionUi::new(session)?,
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
                        speaker_id: Some("host"), speaker_name: "Host", content_id: "host-diagnostic/second",
                        text: "The first click reveals the full line. The next click settles this interaction once.",
                    }], "en")?;
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
mod tests {
    use super::*;
    use reel_core::identity::{FrameId, SurfaceEpoch};
    use reel_ui::{
        app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
        bridge::{BridgeLimits, NoticeResult, PresentationNotice, PresentationOutcome},
        dpi::{PhysicalPosition, PhysicalSize},
    };
    use std::sync::Arc;

    pub(super) fn session() -> Session {
        Session::new(SessionId(42), Epoch(3)).unwrap()
    }
    fn say(session: &mut Session, text: &str) -> InteractionId {
        session
            .begin_say(
                session.root_owner(),
                &[DialogueLineInput {
                    speaker_id: Some("host"),
                    speaker_name: "Host",
                    content_id: "test/dialogue",
                    text,
                }],
                "en",
            )
            .unwrap()
    }
    pub(super) fn mount(ui: &SessionUi) -> (PresentationBridge, WidgetId) {
        mount_with_font(ui, None)
    }
    fn mount_with_font(
        ui: &SessionUi,
        font: Option<reel_ui::peniko::Blob<u8>>,
    ) -> (PresentationBridge, WidgetId) {
        let widget = ui.widget().prepare();
        let id = widget.id();
        let root = RenderRoot::new(
            widget,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(640, 420),
                scale_factor: 1.,
                default_font: font,
            },
        );
        (
            PresentationBridge::new(
                root,
                SurfaceEpoch(1),
                BridgeLimits {
                    geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
                    max_candidates: 4,
                    max_bridge_bytes: 64 << 10,
                    max_snapshot_bytes: 8 << 20,
                },
            )
            .unwrap(),
            id,
        )
    }
    fn accept(ui: &mut SessionUi, bridge: &mut PresentationBridge, frame: u64) {
        let source = bridge.record(FrameId(frame)).unwrap();
        let stamp = ui.projection_stamp();
        assert_eq!(
            bridge.resolve(PresentationNotice::new(
                source.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(FrameId(frame))
            )),
            NoticeResult::Accepted
        );
        ui.presented(stamp);
    }
    pub(super) fn mouse(kind: &str, point: (f64, f64)) -> PointerEvent {
        let pointer = PointerInfo {
            pointer_id: Some(PointerId::PRIMARY),
            persistent_device_id: None,
            pointer_type: PointerType::Mouse,
        };
        let state = PointerState {
            position: PhysicalPosition::new(point.0, point.1),
            ..Default::default()
        };
        match kind {
            "move" => PointerEvent::Move(PointerUpdate {
                pointer,
                current: state,
                coalesced: vec![],
                predicted: vec![],
            }),
            "down" => PointerEvent::Down(PointerButtonEvent {
                pointer,
                state,
                button: Some(PointerButton::Primary),
            }),
            "up" => PointerEvent::Up(PointerButtonEvent {
                pointer,
                state,
                button: Some(PointerButton::Primary),
            }),
            _ => PointerEvent::Cancel(pointer),
        }
    }
    fn click(ui: &mut SessionUi, bridge: &mut PresentationBridge, point: (f64, f64)) {
        for kind in ["move", "down", "up"] {
            ui.handle_pointer(bridge, mouse(kind, point)).unwrap();
        }
    }
    fn visible(ui: &SessionUi) -> usize {
        ui.projection.lines[0].visible.end
    }

    #[test]
    fn author_scene_renders_edits_original_nodes_and_keeps_final_picture() {
        use crate::aside_session::{AsideSessionUi, StoryProgress, StoryState};
        use reel_core::budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits};
        use reel_core::identity::{AssetKey, ContentVersion, ResourceVersion};
        use reel_engine::aside::{SliceBudget, VmLimits};
        use reel_ui::frame::{BuildLimits, FrameBuild, FrameDraw, FrameInfo, FrameOp, StepBudget};
        use reel_ui::resources::{ResourceData, ResourceHandle, ResourceRegistry};

        fn labels(widget: WidgetRef<'_, dyn Widget>, out: &mut Vec<(WidgetId, String)>) {
            if let Some(label) = widget.downcast::<Label>() {
                out.push((widget.id(), label.text().to_string()));
            }
            for child in widget.children() {
                labels(child, out);
            }
        }
        fn present(
            driver: &mut AsideSessionUi,
            bridge: &mut PresentationBridge,
            root: WidgetId,
            frame: u64,
            resources: (&ResourceRegistry, &[ResourceHandle]),
        ) -> Vec<(WidgetId, String)> {
            driver.sync(bridge, root).unwrap();
            let source = bridge.record(FrameId(frame)).unwrap();
            let key = source.key();
            let pool = BudgetPool::new(
                BudgetCategory::CpuOutput,
                QueueLimits {
                    max_requests: 16,
                    max_bytes: 32 << 20,
                },
            );
            let mut build = FrameBuild::new(
                source,
                resources.0.freeze(resources.1, &pool).unwrap(),
                FrameInfo {
                    logical_size: [640., 420.],
                    physical_size: [640, 420],
                },
                BuildLimits { max_bytes: 8 << 20 },
                &pool,
            )
            .unwrap();
            while !build
                .step(StepBudget {
                    max_commands: 64,
                    max_units: 8192,
                    max_bytes: 64 << 10,
                })
                .unwrap()
                .complete
            {}
            let frame = build.finish().unwrap();
            if std::env::var_os("REEL_SESSION_FONT").is_some() {
                assert!(frame.commands().iter().any(|command| matches!(&command.op,
                    FrameOp::Draw(FrameDraw::GlyphRun(run)) if run.font_size == 40. && !run.glyphs.is_empty()
                )), "the Chinese title must produce real glyphs from the supplied font");
            }
            assert!(frame.commands().iter().any(|command| matches!(&command.op, FrameOp::PushGroup(group) if group.composite.alpha < 1.)));
            assert_eq!(
                bridge.resolve(PresentationNotice::new(
                    key,
                    SurfaceEpoch(1),
                    PresentationOutcome::Accepted(frame.frame_id())
                )),
                NoticeResult::Accepted
            );
            driver.presented(driver.projection_stamp()).unwrap();
            let root_widget = bridge
                .root()
                .get_widget(root)
                .unwrap()
                .downcast::<SessionWidget>()
                .unwrap();
            let mut output = Vec::new();
            labels(
                bridge.root().get_widget(root_widget.stage.id()).unwrap(),
                &mut output,
            );
            output
        }
        fn advance(
            driver: &mut AsideSessionUi,
            bridge: &mut PresentationBridge,
            root: WidgetId,
            frame: &mut u64,
            resources: (&ResourceRegistry, &[ResourceHandle]),
        ) {
            for _ in 0..2 {
                let widget = bridge
                    .root()
                    .get_widget(root)
                    .unwrap()
                    .downcast::<SessionWidget>()
                    .unwrap();
                let point = widget.dialogue_rect.center() - Vec2::new(0., widget.scroll.get());
                for kind in ["move", "down", "up"] {
                    driver
                        .handle_pointer(bridge, mouse(kind, (point.x, point.y)))
                        .unwrap();
                }
                *frame += 1;
                present(driver, bridge, root, *frame, resources);
            }
        }
        fn run_to_wait(driver: &mut AsideSessionUi) {
            for _ in 0..10_000 {
                match driver.tick(SliceBudget::default(), 256, || 0).unwrap() {
                    StoryProgress::Interaction(_) | StoryProgress::Finished => return,
                    StoryProgress::HostFailed(stage) => {
                        panic!("host failed: {stage:?}: {:?}", driver.host_failure())
                    }
                    StoryProgress::Faulted(_) => {
                        panic!("script fault: {:?}", driver.fault().unwrap())
                    }
                    _ => {}
                }
            }
            panic!("author example did not yield");
        }
        let mut driver = AsideSessionUi::from_source(
            session(),
            include_str!("../../../examples/author_stage.aside"),
            "main",
            1,
            "main",
            "zh-CN",
            VmLimits::default(),
        )
        .unwrap();
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 32,
                max_bytes: 128 << 20,
            },
        );
        let mut registry = ResourceRegistry::new(16, 1 << 20).unwrap();
        let mut handles = Vec::new();
        let font = std::env::var_os("REEL_SESSION_FONT").map(|path| {
            let bytes = std::fs::read(path).unwrap();
            let ticket = pool.reserve(bytes.len() as u64).unwrap();
            let handle = registry
                .register_font(
                    ResourceVersion {
                        key: AssetKey("author-scene/font".into()),
                        version: ContentVersion([1; 32]),
                    },
                    OwnedBytes::new(bytes, ticket).unwrap(),
                )
                .unwrap();
            handles.push(handle);
            let lease = registry.acquire(handle).unwrap();
            let ResourceData::Font(font) = lease.data() else {
                unreachable!()
            };
            font.clone()
        });
        let resources = (&registry, handles.as_slice());
        let (mut bridge, root) = mount_with_font(driver.ui(), font);
        let mut frame = 1;
        run_to_wait(&mut driver);
        let before = present(&mut driver, &mut bridge, root, frame, resources);
        let title_id = before
            .iter()
            .find(|(_, text)| text == "画面，由你定义")
            .unwrap()
            .0;
        advance(&mut driver, &mut bridge, root, &mut frame, resources);
        run_to_wait(&mut driver);
        frame += 1;
        let after = present(&mut driver, &mut bridge, root, frame, resources);
        assert!(
            after
                .iter()
                .any(|(id, text)| *id == title_id && text == "同一幅画面，新的语气")
        );
        assert_eq!(
            before.iter().map(|(id, _)| id).collect::<Vec<_>>(),
            after.iter().map(|(id, _)| id).collect::<Vec<_>>()
        );
        advance(&mut driver, &mut bridge, root, &mut frame, resources);
        run_to_wait(&mut driver);
        assert_eq!(driver.state(), StoryState::Finished);
        frame += 1;
        assert_eq!(
            present(&mut driver, &mut bridge, root, frame, resources),
            after
        );
    }

    #[test]
    fn presented_dialogue_reveals_then_advances_once_and_old_frame_cannot_start_successor() {
        let mut session = session();
        say(&mut session, "A full unchanged line");
        let mut ui = SessionUi::new(session).unwrap();
        let (mut bridge, id) = mount(&ui);
        click(&mut ui, &mut bridge, (100., 300.));
        assert_eq!(visible(&ui), 0);
        assert!(ui.session.take_completion().is_none());
        accept(&mut ui, &mut bridge, 1);
        click(&mut ui, &mut bridge, (100., 300.));
        ui.sync(&mut bridge, id).unwrap();
        assert_eq!(visible(&ui), "A full unchanged line".len());
        assert_eq!(ui.session.view().history.len(), 1);
        assert!(ui.session.take_completion().is_none());
        // Accepted frame 1 still depicts the previous reveal projection.
        click(&mut ui, &mut bridge, (100., 300.));
        assert!(ui.session.take_completion().is_none());
        accept(&mut ui, &mut bridge, 2);
        click(&mut ui, &mut bridge, (100., 300.));
        let completion = ui.session.take_completion().unwrap();
        assert_eq!(completion.outcome(), &CompletionOutcome::Advanced);
        assert!(ui.session.take_completion().is_none());
        say(&mut ui.session, "The unseen successor");
        ui.sync(&mut bridge, id).unwrap();
        click(&mut ui, &mut bridge, (100., 300.));
        assert_eq!(visible(&ui), 0);
        assert_eq!(ui.session.view().history.len(), 2);
        accept(&mut ui, &mut bridge, 3);
        click(&mut ui, &mut bridge, (100., 300.));
        ui.sync(&mut bridge, id).unwrap();
        assert_eq!(visible(&ui), "The unseen successor".len());
    }
    #[test]
    fn down_cannot_be_reinterpreted_by_a_new_accepted_interaction() {
        let mut session = session();
        let first = say(&mut session, "First");
        session
            .reveal_more(session.root_owner(), first, usize::MAX)
            .unwrap();
        let mut ui = SessionUi::new(session).unwrap();
        let (mut bridge, id) = mount(&ui);
        accept(&mut ui, &mut bridge, 1);
        ui.handle_pointer(&mut bridge, mouse("move", (100., 300.)))
            .unwrap();
        ui.handle_pointer(&mut bridge, mouse("down", (100., 300.)))
            .unwrap();
        ui.session
            .handle_input(InputEvent {
                source_owner: ui.session.root_owner(),
                sequence: InputSequence(1),
                action: InputAction::Advance(first),
            })
            .unwrap();
        ui.session.take_completion().unwrap();
        say(&mut ui.session, "Second");
        ui.sync(&mut bridge, id).unwrap();
        accept(&mut ui, &mut bridge, 2);
        ui.handle_pointer(&mut bridge, mouse("up", (100., 300.)))
            .unwrap();
        assert_eq!(visible(&ui), 0);
        assert!(ui.session.take_completion().is_none());
        click(&mut ui, &mut bridge, (100., 300.));
        ui.sync(&mut bridge, id).unwrap();
        assert_eq!(visible(&ui), "Second".len());
    }
    #[test]
    fn actual_button_choice_id_settles_once_consumes_click_and_revokes_removed_node_owner() {
        let mut session = session();
        session
            .begin_choice(
                session.root_owner(),
                &[
                    ChoiceInput {
                        key: "door",
                        label: "Door",
                    },
                    ChoiceInput {
                        key: "window",
                        label: "Window",
                    },
                ],
            )
            .unwrap();
        let mut ui = SessionUi::new(session).unwrap();
        let (mut bridge, root) = mount(&ui);
        let old_owners = ui.choice_owners.clone();
        accept(&mut ui, &mut bridge, 1);
        let buttons = bridge
            .root()
            .get_widget(root)
            .unwrap()
            .downcast::<SessionWidget>()
            .unwrap();
        let button_id = buttons.choices[1].button.id();
        let reel_ui::app::InputGeometrySource::Presented(Some(geometry)) =
            bridge.root().input_geometry_source()
        else {
            panic!()
        };
        assert_eq!(geometry.hit_test(Point::new(100., 320.)), Some(button_id));
        click(&mut ui, &mut bridge, (100., 320.));
        let completion = ui.session.take_completion().unwrap();
        assert_eq!(
            completion.outcome(),
            &CompletionOutcome::Selected("window".into())
        );
        say(&mut ui.session, "After choice");
        ui.sync(&mut bridge, root).unwrap();
        assert_eq!(
            visible(&ui),
            0,
            "choice Up cannot advance the replacement dialogue"
        );
        assert!(
            old_owners
                .iter()
                .all(|owner| !ui.session.is_owner_live(*owner))
        );
        click(&mut ui, &mut bridge, (100., 320.));
        assert_eq!(visible(&ui), 0);
        assert!(ui.session.take_completion().is_none());
    }
    #[test]
    fn cancelled_session_and_cleared_surface_have_no_input_permission() {
        let mut session = session();
        say(&mut session, "Cancel this");
        let mut ui = SessionUi::new(session).unwrap();
        let (mut bridge, root) = mount(&ui);
        accept(&mut ui, &mut bridge, 1);
        bridge.clear();
        ui.clear_presented();
        click(&mut ui, &mut bridge, (100., 300.));
        assert_eq!(visible(&ui), 0);
        assert!(ui.cancel());
        assert!(!ui.cancel());
        ui.sync(&mut bridge, root).unwrap();
        click(&mut ui, &mut bridge, (100., 300.));
        assert!(matches!(
            ui.session.take_completion().unwrap().outcome(),
            CompletionOutcome::Cancelled(_)
        ));
        assert!(ui.session.take_completion().is_none());
        assert_eq!(ui.session.view().history.len(), 1);
    }
    #[test]
    fn revoked_source_owner_input_is_filtered_without_a_window_error() {
        let mut session = session();
        say(&mut session, "A live main interaction");
        let mut ui = SessionUi::new(session).unwrap();
        let (mut bridge, _) = mount(&ui);
        accept(&mut ui, &mut bridge, 1);
        ui.session.revoke_owner(ui.source_owner).unwrap();
        click(&mut ui, &mut bridge, (100., 300.));
        assert_eq!(visible(&ui), 0);
        assert!(ui.session.take_completion().is_none());
        assert_eq!(ui.session.status(), reel_engine::SessionStatus::Running);
        assert_eq!(ui.session.view().history.len(), 1);
    }

    #[test]
    fn first_ui_click_after_session_handoff_uses_the_consumed_input_watermark() {
        let mut session = session();
        let interaction = say(&mut session, "Already revealed before UI attachment");
        assert_eq!(
            session.handle_input(InputEvent {
                source_owner: session.root_owner(),
                sequence: InputSequence(100),
                action: InputAction::Advance(interaction),
            }),
            Ok(reel_engine::InputOutcome::Revealed)
        );
        let mut ui = SessionUi::new(session).unwrap();
        let (mut bridge, root) = mount(&ui);
        accept(&mut ui, &mut bridge, 1);
        click(&mut ui, &mut bridge, (100., 300.));
        assert_eq!(
            ui.session_mut().take_completion().unwrap().outcome(),
            &CompletionOutcome::Advanced
        );
        assert!(ui.session_mut().take_completion().is_none());
        assert_eq!(ui.session().view().history.len(), 1);
        ui.sync(&mut bridge, root).unwrap();
        click(&mut ui, &mut bridge, (100., 300.));
        assert!(ui.session_mut().take_completion().is_none());
    }

    #[test]
    fn inputs_through_session_mut_and_ui_share_one_sequence_source() {
        let mut session = session();
        let first = say(&mut session, "First");
        let mut ui = SessionUi::new(session).unwrap();
        let (mut bridge, root) = mount(&ui);
        accept(&mut ui, &mut bridge, 1);
        click(&mut ui, &mut bridge, (100., 300.));
        ui.sync(&mut bridge, root).unwrap();
        assert_eq!(visible(&ui), "First".len());
        let source_owner = ui.session().root_owner();
        let mut foreign = Session::new(SessionId(99), Epoch(3)).unwrap();
        let foreign_interaction = say(&mut foreign, "Foreign interaction");
        // A live source's invalid action still consumes the global watermark.
        assert_eq!(
            ui.session_mut().handle_input(InputEvent {
                source_owner,
                sequence: InputSequence(100),
                action: InputAction::Advance(foreign_interaction),
            }),
            Err(reel_engine::InputRejection::StaleInteraction)
        );
        accept(&mut ui, &mut bridge, 2);
        click(&mut ui, &mut bridge, (100., 300.));
        let completion = ui.session_mut().take_completion().unwrap();
        assert_eq!(completion.interaction_id(), first);
        assert_eq!(completion.outcome(), &CompletionOutcome::Advanced);
        assert!(ui.session_mut().take_completion().is_none());
        say(ui.session_mut(), "After external input");
        ui.sync(&mut bridge, root).unwrap();
        assert_eq!(
            ui.session_mut().handle_input(InputEvent {
                source_owner,
                sequence: InputSequence(1_000),
                action: InputAction::Advance(first),
            }),
            Err(reel_engine::InputRejection::StaleInteraction)
        );
        accept(&mut ui, &mut bridge, 3);
        click(&mut ui, &mut bridge, (100., 300.));
        ui.sync(&mut bridge, root).unwrap();
        assert_eq!(visible(&ui), "After external input".len());
        assert!(ui.session_mut().take_completion().is_none());
        accept(&mut ui, &mut bridge, 4);
        click(&mut ui, &mut bridge, (100., 300.));
        assert_eq!(
            ui.session_mut().take_completion().unwrap().outcome(),
            &CompletionOutcome::Advanced
        );
        assert_eq!(ui.session().view().history.len(), 2);
    }

    #[test]
    fn zero_line_dialogue_advances_through_the_presented_pointer_path_once() {
        let mut session = session();
        let interaction = session.begin_say(session.root_owner(), &[], "en").unwrap();
        let mut ui = SessionUi::new(session).unwrap();
        let (mut bridge, root) = mount(&ui);
        click(&mut ui, &mut bridge, (100., 300.));
        assert!(ui.session_mut().take_completion().is_none());
        accept(&mut ui, &mut bridge, 1);
        click(&mut ui, &mut bridge, (100., 300.));
        let completion = ui.session_mut().take_completion().unwrap();
        assert_eq!(completion.interaction_id(), interaction);
        assert_eq!(completion.outcome(), &CompletionOutcome::Advanced);
        assert_eq!(ui.session().view().history.len(), 1);
        assert!(ui.session().view().history[0].lines().is_empty());
        assert!(ui.session_mut().take_completion().is_none());
        say(ui.session_mut(), "Unseen successor");
        ui.sync(&mut bridge, root).unwrap();
        click(&mut ui, &mut bridge, (100., 300.));
        assert_eq!(visible(&ui), 0);
        accept(&mut ui, &mut bridge, 2);
        click(&mut ui, &mut bridge, (100., 300.));
        ui.sync(&mut bridge, root).unwrap();
        assert_eq!(visible(&ui), "Unseen successor".len());
        assert_eq!(ui.session().view().history.len(), 2);
    }

    fn scroll(ui: &mut SessionUi, bridge: &mut PresentationBridge, delta: f32) {
        let PointerEvent::Down(event) = mouse("down", (100., 300.)) else {
            unreachable!()
        };
        ui.handle_pointer(
            bridge,
            PointerEvent::Scroll(PointerScrollEvent {
                pointer: event.pointer,
                state: event.state,
                delta: ScrollDelta::LineDelta(0., delta),
            }),
        )
        .unwrap();
    }

    #[test]
    fn many_dialogue_rows_retain_full_text_and_scroll_without_negative_layout() {
        let mut ui = SessionUi::new(session()).unwrap();
        let (mut bridge, root) = mount(&ui);
        accept(&mut ui, &mut bridge, 1);
        let text = "A complete long dialogue line. ".repeat(40);
        let line = DialogueLineInput {
            speaker_id: None,
            speaker_name: "",
            content_id: "large-group",
            text: &text,
        };
        ui.session
            .begin_say(ui.session.root_owner(), &[line; 6], "en")
            .unwrap();
        ui.sync(&mut bridge, root).unwrap();
        accept(&mut ui, &mut bridge, 2);
        assert_eq!(ui.projection.lines.len(), 6);
        assert!(ui.projection.lines.iter().all(|row| row.text == text));
        scroll(&mut ui, &mut bridge, 100_000.);
        accept(&mut ui, &mut bridge, 3);
        assert!(ui.scroll.get() > 0.);
        click(&mut ui, &mut bridge, (100., 300.));
        ui.sync(&mut bridge, root).unwrap();
        assert!(
            ui.projection
                .lines
                .iter()
                .all(|row| row.visible.end == text.len())
        );
        accept(&mut ui, &mut bridge, 4);
        click(&mut ui, &mut bridge, (100., 300.));
        assert_eq!(
            ui.session.take_completion().unwrap().outcome(),
            &CompletionOutcome::Advanced
        );
        assert_eq!(ui.session.view().history[0].lines().len(), 6);
    }

    #[test]
    fn all_choices_are_reachable_and_scroll_requires_new_presented_geometry() {
        let mut session = session();
        let count = 1025;
        let keys: Vec<_> = (0..count).map(|i| format!("choice-{i}")).collect();
        let rows: Vec<_> = keys
            .iter()
            .map(|key| ChoiceInput { key, label: key })
            .collect();
        session.begin_choice(session.root_owner(), &rows).unwrap();
        let mut ui = SessionUi::new(session).unwrap();
        assert_eq!(
            ui.choice_owners.len(),
            1,
            "one projection lifetime, not one owner per option"
        );
        let (mut bridge, root) = mount(&ui);
        accept(&mut ui, &mut bridge, 1);
        ui.handle_pointer(&mut bridge, mouse("down", (100., 270.)))
            .unwrap();
        scroll(&mut ui, &mut bridge, 100_000.);
        assert!(ui.binding.borrow().accepted.is_none());
        click(&mut ui, &mut bridge, (100., 350.));
        assert!(ui.session.take_completion().is_none());
        accept(&mut ui, &mut bridge, 2);
        ui.handle_pointer(&mut bridge, mouse("up", (100., 350.)))
            .unwrap();
        assert!(ui.session.take_completion().is_none());
        let widget = bridge
            .root()
            .get_widget(root)
            .unwrap()
            .downcast::<SessionWidget>()
            .unwrap();
        let last = widget.choices.last().unwrap().button.id();
        let reel_ui::app::InputGeometrySource::Presented(Some(geometry)) =
            bridge.root().input_geometry_source()
        else {
            panic!()
        };
        assert_eq!(geometry.hit_test(Point::new(100., 350.)), Some(last));
        click(&mut ui, &mut bridge, (100., 350.));
        assert_eq!(
            ui.session.take_completion().unwrap().outcome(),
            &CompletionOutcome::Selected(keys.last().unwrap().clone())
        );
        ui.sync(&mut bridge, root).unwrap();
        assert_eq!(ui.scroll.get(), 0.);
    }

    #[test]
    fn gamepad_focus_scrolls_choices_into_view_and_selects_the_presented_last_item() {
        use reel_ui::input::{ActionPhase as P, NavigationAction as A, NavigationEvent};
        let nav = |action, phase| {
            TextEvent::Navigation(NavigationEvent {
                action,
                phase,
                value: if phase == P::Released { 0. } else { 1. },
            })
        };
        let mut session = session();
        let keys: Vec<_> = (0..24).map(|i| format!("choice-{i}")).collect();
        let rows: Vec<_> = keys
            .iter()
            .map(|key| ChoiceInput { key, label: key })
            .collect();
        session.begin_choice(session.root_owner(), &rows).unwrap();
        let mut ui = SessionUi::new(session).unwrap();
        let (mut bridge, root) = mount(&ui);
        accept(&mut ui, &mut bridge, 1);
        for i in 0..keys.len() {
            ui.handle_text(&mut bridge, nav(A::Down, P::Pressed))
                .unwrap();
            ui.handle_text(&mut bridge, nav(A::Down, P::Released))
                .unwrap();
            ui.sync(&mut bridge, root).unwrap();
            accept(&mut ui, &mut bridge, i as u64 + 2);
            let focused = bridge.root().focused_widget().unwrap();
            let reel_ui::app::InputGeometrySource::Presented(Some(geometry)) =
                bridge.root().input_geometry_source()
            else {
                panic!("accepted choices")
            };
            let record = geometry.get(focused).unwrap();
            let point = record.window_transform * record.size.to_rect().center();
            assert_eq!(geometry.hit_test(point), Some(focused));
            assert!(
                (0. ..=420.).contains(&point.y),
                "choice {i} must be visible after focus scrolling"
            );
        }
        assert!(ui.scroll.get() > 0.);
        ui.handle_text(&mut bridge, nav(A::Confirm, P::Pressed))
            .unwrap();
        ui.handle_text(&mut bridge, nav(A::Confirm, P::Released))
            .unwrap();
        assert_eq!(
            ui.session.take_completion().unwrap().outcome(),
            &CompletionOutcome::Selected(keys.last().unwrap().clone())
        );
    }

    #[test]
    fn existing_owner_count_does_not_block_choice_projection() {
        let mut session = Session::new(SessionId(42), Epoch(3)).unwrap();
        for _ in 0..100 {
            session.create_owner().unwrap();
        }
        let mut ui = SessionUi::new(session).unwrap();
        let (mut bridge, root) = mount(&ui);
        accept(&mut ui, &mut bridge, 1);
        ui.session
            .begin_choice(
                ui.session.root_owner(),
                &[
                    ChoiceInput {
                        key: "a",
                        label: "A",
                    },
                    ChoiceInput {
                        key: "b",
                        label: "B",
                    },
                ],
            )
            .unwrap();
        ui.sync(&mut bridge, root).unwrap();
        assert_eq!(ui.projection.choices.len(), 2);
        assert_ne!(ui.binding.borrow().accepted, Some(ui.projection_stamp()));
        accept(&mut ui, &mut bridge, 2);
        assert_eq!(ui.binding.borrow().accepted, Some(ui.projection_stamp()));
        assert!(ui.session.create_owner().is_ok());
        assert_eq!(ui.choice_owners.len(), 1);
        assert!(ui.session.is_owner_live(ui.source_owner));
    }
}
