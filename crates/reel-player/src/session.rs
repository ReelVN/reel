//! Real Rust session projection into Reel's maintained UI and presented input path.
//!
//! This layer never executes Aside. The optional native diagnostic below drives
//! explicit host requests and must not be described as a source story runner.
use reel_engine::{
    ChoiceId, InputAction, InputRejection, InteractionId, InteractionView, OwnerHandle, Session,
};
#[cfg(any(test, feature = "window"))]
use reel_engine::{
    ChoiceInput, CompletionOutcome, DialogueLineInput, Epoch, SessionId, SessionLimits,
};
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
    interaction: Option<InteractionId>,
    scroll_bits: u64,
}
#[derive(Default)]
struct InputBinding {
    accepted: Option<ProjectionStamp>,
    down: Option<ProjectionStamp>,
}
impl InputBinding {
    fn permits(&self, current: ProjectionStamp) -> bool {
        self.accepted == Some(current) && self.down == Some(current)
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
    status: String,
}
impl Projection {
    fn from_session(session: &Session) -> Result<Self, Error> {
        let view = session.view();
        let mut projection = Self {
            interaction: None,
            lines: vec![],
            choices: vec![],
            history_count: view.history.len(),
            status: format!("{:?}", view.status),
        };
        match view.interaction {
            Some(InteractionView::Dialogue(dialogue)) => {
                projection.interaction = Some(dialogue.id());
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
struct UiInput {
    owner: OwnerHandle,
    action: InputAction,
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
    title: WidgetPod<Label>,
    status: WidgetPod<Label>,
    lines: Vec<LineWidgets>,
    choices: Vec<ChoiceWidget>,
    background: Color,
    scroll: Rc<Cell<f64>>,
    content_height: f64,
    viewport: Size,
    dialogue_rect: Rect,
}
impl SessionWidget {
    fn new(ui: &SessionUi) -> Self {
        let mut widget = Self {
            projection: ui.projection.clone(),
            stamp: ui.stamp,
            source_owner: ui.source_owner,
            binding: ui.binding.clone(),
            inputs: ui.inputs.clone(),
            title: WidgetPod::new(Label::new("Reel")),
            status: WidgetPod::new(Label::new(ui.status_text())),
            lines: vec![],
            choices: vec![],
            background: ui.background,
            scroll: ui.scroll.clone(),
            content_height: 420.,
            viewport: Size::new(640., 420.),
            dialogue_rect: Rect::new(30., 245., 610., 380.),
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
        let same_interaction = this.widget.projection.interaction == ui.projection.interaction;
        if !same_interaction {
            this.widget.scroll.set(0.);
        }
        if same_interaction && this.widget.lines.len() == ui.projection.lines.len() {
            for (widgets, row) in this.widget.lines.iter_mut().zip(&ui.projection.lines) {
                if let Some(mut label) = this.ctx.get_mut(&mut widgets.text) {
                    // Reveal never substitutes a prefix for the original full text.
                    Label::set_visible_bytes(&mut label, row.visible.clone());
                }
            }
        } else {
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
        if let Some(mut status) = this.ctx.get_mut(&mut this.widget.status) {
            Label::set_text(&mut status, ui.status_text());
        }
        this.widget.projection = ui.projection.clone();
        this.widget.stamp = ui.stamp;
        this.widget.background = ui.background;
        this.ctx.request_layout();
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
                    *self.binding.borrow_mut() = InputBinding::default();
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
                if ctx.target() == ctx.widget_id()
                    && self.dialogue_rect.contains(
                        ctx.local_position(e.state.position) + Vec2::new(0., self.scroll.get()),
                    ) =>
            {
                ctx.capture_pointer();
                ctx.set_handled();
            }
            PointerEvent::Up(e) if ctx.is_active() => {
                if self.binding.borrow().permits(self.current_stamp())
                    && self.dialogue_rect.contains(
                        ctx.local_position(e.state.position) + Vec2::new(0., self.scroll.get()),
                    )
                {
                    self.inputs.borrow_mut().get_or_insert(UiInput {
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
    fn on_action(
        &mut self,
        ctx: &mut ActionCtx<'_>,
        _: &mut PropertiesMut<'_>,
        action: &ErasedAction,
        source: WidgetId,
    ) {
        if action.downcast_ref::<ButtonPress>().is_some()
            && let Some(choice) = self.choices.iter().find(|row| row.button.id() == source)
        {
            if self.binding.borrow().permits(self.current_stamp()) {
                self.inputs.borrow_mut().get_or_insert(UiInput {
                    owner: choice.owner,
                    action: InputAction::Select(choice.id.clone()),
                });
            }
            ctx.set_handled();
        }
    }
    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
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
        let text_width = (size.width - 90.).max(0.);
        let button_width = (size.width - 140.).max(0.);
        let preferred = |width| SizeDef::new(LenDef::Fixed(Length::px(width)), LenDef::MaxContent);
        let mut y = 250.;
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
        let bottom = y.max(380.);
        self.dialogue_rect = Rect::new(30., 245., (size.width - 30.).max(30.), bottom);
        self.content_height = bottom + 40.;
        let offset = self
            .scroll
            .get()
            .clamp(0., (self.content_height - size.height).max(0.));
        self.scroll.set(offset);
        ctx.run_layout(&mut self.title, Size::new((size.width - 60.).max(0.), 36.));
        ctx.place_child(&mut self.title, Point::new(30., 20. - offset));
        ctx.run_layout(&mut self.status, Size::new((size.width - 60.).max(0.), 30.));
        ctx.place_child(&mut self.status, Point::new(30., bottom + 5. - offset));
        for (line, &(y, height)) in self.lines.iter_mut().zip(&positions) {
            ctx.place_child(&mut line.speaker, Point::new(45., y - offset));
            ctx.place_child(&mut line.text, Point::new(45., y + height - offset));
        }
        for (choice, &(y, _)) in self.choices.iter_mut().zip(&positions[self.lines.len()..]) {
            ctx.place_child(&mut choice.button, Point::new(70., y - offset));
        }
    }
    fn paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, painter: &mut Painter<'_>) {
        painter.fill_rect(self.viewport.to_rect(), self.background);
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
        let mut ids = ChildrenIds::from_slice(&[self.title.id(), self.status.id()]);
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
    background: Color,
    scroll: Rc<Cell<f64>>,
}
impl SessionUi {
    pub fn new(mut session: Session) -> Result<Self, Error> {
        let source_owner = session.create_owner()?;
        let projection = Projection::from_session(&session)?;
        let choice_owners = allocate_choice_owners(&mut session, projection.choices.len())?;
        let stamp = ProjectionStamp {
            revision: 1,
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
            background: Color::from_rgb8(35, 54, 74),
            scroll: Rc::new(Cell::new(0.)),
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
    pub fn projection_stamp(&self) -> ProjectionStamp {
        ProjectionStamp {
            scroll_bits: self.scroll.get().to_bits(),
            ..self.stamp
        }
    }
    /// Call only after PresentationBridge accepted the matching actual frame.
    pub fn presented(&mut self, stamp: ProjectionStamp) {
        self.binding.borrow_mut().accepted = Some(stamp);
    }
    #[cfg(feature = "window")]
    pub(crate) fn gesture_active(&self) -> bool {
        self.binding.borrow().down.is_some()
    }
    pub fn clear_presented(&mut self) {
        *self.binding.borrow_mut() = InputBinding::default();
    }
    fn status_text(&self) -> String {
        format!(
            "{} | history: {} | scroll for more; click dialogue twice; Escape cancels",
            self.projection.status, self.projection.history_count
        )
    }
    pub fn sync(
        &mut self,
        bridge: &mut PresentationBridge,
        root_id: WidgetId,
    ) -> Result<(), Error> {
        let projection = match Projection::from_session(&self.session) {
            Ok(projection) => projection,
            Err(error) => {
                self.clear_presented();
                return Err(error);
            }
        };
        if projection != self.projection {
            if projection.interaction != self.projection.interaction {
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
            }
            self.stamp.revision = self
                .stamp
                .revision
                .checked_add(1)
                .ok_or("UI projection revision exhausted")?;
            self.stamp.interaction = projection.interaction;
            self.projection = projection;
            bridge.edit_widget(root_id, |mut widget| {
                SessionWidget::sync(&mut widget.downcast::<SessionWidget>(), self)
            });
        }
        Ok(())
    }
    pub fn handle_pointer(
        &mut self,
        bridge: &mut PresentationBridge,
        event: PointerEvent,
    ) -> Result<Handled, Error> {
        match &event {
            PointerEvent::Down(_) => {
                let accepted = self.binding.borrow().accepted;
                self.binding.borrow_mut().down = accepted;
            }
            PointerEvent::Cancel(_) => self.binding.borrow_mut().down = None,
            _ => {}
        }
        let handled = bridge.handle_pointer_event(event.clone());
        if matches!(event, PointerEvent::Up(_) | PointerEvent::Cancel(_)) {
            self.binding.borrow_mut().down = None;
        }
        let input = self.inputs.borrow_mut().take();
        if let Some(input) = input {
            // Owner/session cancellation may race the final UI dispatch. Host
            // rejections filter that event; they are not window failures.
            // Assign the new event at final dispatch, after any external input
            // through session_mut. Already-sequenced replays use handle_input.
            if self.session.handle_local_input(input.owner, input.action)
                == Err(InputRejection::SequenceExhausted)
            {
                return Err("input sequence exhausted".into());
            }
        }
        Ok(handled)
    }
    pub fn cancel(&mut self) -> bool {
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
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn diagnostic_step(&self) -> u8 {
        self.step
    }
    pub(crate) fn new() -> Result<Self, Error> {
        let mut session = Session::new(SessionId(1), Epoch(1), SessionLimits::default())?;
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

    fn session() -> Session {
        Session::new(SessionId(42), Epoch(3), SessionLimits::default()).unwrap()
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
    fn mount(ui: &SessionUi) -> (PresentationBridge, WidgetId) {
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
                test_font: None,
            },
        );
        (
            PresentationBridge::new(
                root,
                SurfaceEpoch(1),
                BridgeLimits {
                    geometry: reel_ui::app::GeometryLimits {
                        max_nodes: 256,
                        max_bytes: 1 << 20,
                        max_path: 32,
                    },
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
    fn mouse(kind: &str, point: (f64, f64)) -> PointerEvent {
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
        let mut foreign = Session::new(SessionId(99), Epoch(3), SessionLimits::default()).unwrap();
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
    fn all_configured_choices_are_reachable_and_scroll_requires_new_presented_geometry() {
        let mut session = session();
        let count = session.limits().max_choices;
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
    fn failed_choice_owner_allocation_rolls_back_without_destroying_old_view() {
        let session = Session::new(
            SessionId(42),
            Epoch(3),
            SessionLimits {
                max_owners: 2,
                ..SessionLimits::default()
            },
        )
        .unwrap();
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
        assert!(ui.sync(&mut bridge, root).is_err());
        assert!(ui.projection.choices.is_empty());
        assert!(ui.binding.borrow().accepted.is_none());
        assert!(ui.session.create_owner().is_err());
        assert!(ui.choice_owners.is_empty());
        assert!(ui.session.is_owner_live(ui.source_owner));
    }
}
