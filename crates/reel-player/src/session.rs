//! Real Rust session projection into Reel's maintained UI and presented input path.
//!
//! This layer never executes Aside. The optional native diagnostic below drives
//! explicit host requests and must not be described as a source story runner.
use reel_engine::{
    ChoiceId, InputAction, InputEvent, InputSequence, InteractionId, InteractionView, OwnerHandle,
    Session,
};
#[cfg(any(test, feature = "window"))]
use reel_engine::{
    ChoiceInput, CompletionOutcome, DialogueLineInput, Epoch, SessionId, SessionLimits,
};
use reel_ui::{
    accesskit::{Node, Role},
    bridge::PresentationBridge,
    core::*,
    imaging::Painter,
    kurbo::{Axis, Point, Rect, Size},
    layout::{LenReq, Length},
    peniko::Color,
    widgets::{Button, ButtonPress, Label},
};
use std::{cell::RefCell, ops::Range, rc::Rc};

type Error = Box<dyn std::error::Error>;

/// An immutable binding to the content recorded in one frame. Hover and capture
/// paints preserve this revision; changing interaction or reveal increments it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectionStamp {
    revision: u64,
    interaction: Option<InteractionId>,
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
        match view.interaction {
            Some(InteractionView::Dialogue(dialogue)) if dialogue.lines().len() > 2 => {
                return Err("the compact session view supports at most two dialogue rows".into());
            }
            Some(InteractionView::Choice(choice)) if choice.options().len() > 2 => {
                return Err("the compact session view supports at most two choice buttons".into());
            }
            _ => {}
        }
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
}
impl SessionWidget {
    fn new(ui: &SessionUi) -> Self {
        let mut widget = Self {
            projection: ui.projection.clone(),
            stamp: ui.stamp,
            source_owner: ui.source_owner,
            binding: ui.binding.clone(),
            inputs: ui.inputs.clone(),
            title: WidgetPod::new(Label::new(
                "Rust session host diagnostic — no Aside execution",
            )),
            status: WidgetPod::new(Label::new(ui.status_text())),
            lines: vec![],
            choices: vec![],
            background: ui.background,
        };
        widget.make_children(&ui.choice_owners);
        widget
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
            .zip(owners)
            .map(|(row, owner)| ChoiceWidget {
                id: row.id.clone(),
                owner: *owner,
                button: WidgetPod::new(Button::with_text(row.label.clone())),
            })
            .collect();
    }
    fn sync(this: &mut WidgetMut<'_, Self>, ui: &SessionUi) {
        let same_interaction = this.widget.projection.interaction == ui.projection.interaction;
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
        this.ctx.request_paint_only();
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
        // Choice controls own their click. It cannot fall through into a dialogue
        // that is installed after this dispatch completes.
        if !self.choices.is_empty() {
            return;
        }
        let Some(interaction) = self.projection.interaction else {
            return;
        };
        if self.lines.is_empty() {
            return;
        }
        match event {
            PointerEvent::Down(e)
                if ctx.target() == ctx.widget_id()
                    && Rect::new(30., 245., 610., 380.)
                        .contains(ctx.local_position(e.state.position)) =>
            {
                ctx.capture_pointer();
                ctx.set_handled();
            }
            PointerEvent::Up(e) if ctx.is_active() => {
                if self.binding.borrow().permits(self.stamp)
                    && Rect::new(30., 245., 610., 380.)
                        .contains(ctx.local_position(e.state.position))
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
            if self.binding.borrow().permits(self.stamp) {
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
        _: LenReq,
        _: Option<Length>,
    ) -> Length {
        Length::px(if axis == Axis::Horizontal { 640. } else { 420. })
    }
    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, _: Size) {
        ctx.run_layout(&mut self.title, Size::new(580., 36.));
        ctx.place_child(&mut self.title, Point::new(30., 20.));
        ctx.run_layout(&mut self.status, Size::new(580., 30.));
        ctx.place_child(&mut self.status, Point::new(30., 385.));
        let height = 125. / self.lines.len().max(1) as f64;
        for (index, line) in self.lines.iter_mut().enumerate() {
            let y = 250. + index as f64 * height;
            ctx.run_layout(&mut line.speaker, Size::new(540., 24.));
            ctx.place_child(&mut line.speaker, Point::new(45., y));
            ctx.run_layout(&mut line.text, Size::new(540., height - 24.));
            ctx.place_child(&mut line.text, Point::new(45., y + 24.));
        }
        for (index, choice) in self.choices.iter_mut().enumerate() {
            ctx.run_layout(&mut choice.button, Size::new(500., 45.));
            ctx.place_child(
                &mut choice.button,
                Point::new(70., 250. + index as f64 * 55.),
            );
        }
    }
    fn paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, painter: &mut Painter<'_>) {
        painter.fill_rect(Rect::new(0., 0., 640., 420.), self.background);
        painter.fill_rect(
            Rect::new(30., 70., 610., 235.),
            Color::from_rgb8(122, 151, 157),
        );
        painter.fill_rect(
            Rect::new(390., 85., 580., 225.),
            Color::from_rgb8(187, 222, 225),
        );
        painter.fill_rect(Rect::new(30., 245., 610., 380.), Color::WHITE);
        for index in 0..self.choices.len() {
            painter.fill_rect(
                Rect::new(
                    70.,
                    250. + index as f64 * 55.,
                    570.,
                    295. + index as f64 * 55.,
                ),
                Color::from_rgb8(219, 230, 238),
            );
        }
    }
    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, node: &mut Node) {
        node.set_label("Rust session host diagnostic; click dialogue to reveal then advance, or select a choice");
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
/// This compact 640×420 view accepts up to two dialogue rows or choice buttons;
/// larger host interactions are rejected before replacing the displayed view.
pub struct SessionUi {
    session: Session,
    projection: Projection,
    stamp: ProjectionStamp,
    source_owner: OwnerHandle,
    choice_owners: Vec<OwnerHandle>,
    binding: Rc<RefCell<InputBinding>>,
    inputs: Rc<RefCell<Option<UiInput>>>,
    sequence: u64,
    background: Color,
}
impl SessionUi {
    pub fn new(mut session: Session) -> Result<Self, Error> {
        let source_owner = session.create_owner()?;
        let projection = Projection::from_session(&session)?;
        let choice_owners = allocate_choice_owners(&mut session, projection.choices.len())?;
        let stamp = ProjectionStamp {
            revision: 1,
            interaction: projection.interaction,
        };
        Ok(Self {
            session,
            projection,
            stamp,
            source_owner,
            choice_owners,
            binding: Rc::new(RefCell::new(InputBinding::default())),
            inputs: Rc::new(RefCell::new(None)),
            sequence: 0,
            background: Color::from_rgb8(35, 54, 74),
        })
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }
    pub fn widget(&self) -> SessionWidget {
        SessionWidget::new(self)
    }
    pub fn projection_stamp(&self) -> ProjectionStamp {
        self.stamp
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
            "{} | history: {} | click dialogue twice; Escape cancels",
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
            self.sequence = self
                .sequence
                .checked_add(1)
                .ok_or("input sequence exhausted")?;
            // Owner/session cancellation may race the final UI dispatch. Host
            // rejections filter that event; they are not window failures.
            let _ = self.session.handle_input(InputEvent {
                source_owner: input.owner,
                sequence: InputSequence(self.sequence),
                action: input.action,
            });
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
    #[cfg(test)]
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
        ui.sequence = 1;
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
    fn oversized_host_interactions_are_rejected_before_compact_view_layout() {
        let mut ui = SessionUi::new(session()).unwrap();
        let (mut bridge, root) = mount(&ui);
        accept(&mut ui, &mut bridge, 1);
        let line = DialogueLineInput {
            speaker_id: None,
            speaker_name: "",
            content_id: "large-group",
            text: "A real host row",
        };
        ui.session
            .begin_say(ui.session.root_owner(), &[line; 6], "en")
            .unwrap();
        assert!(ui.sync(&mut bridge, root).is_err());
        assert!(ui.projection.lines.is_empty());
        assert!(ui.binding.borrow().accepted.is_none());
        let InteractionView::Dialogue(dialogue) = ui.session.view().interaction.unwrap() else {
            panic!("the host group must remain intact");
        };
        assert_eq!(dialogue.lines().len(), 6);
        assert_eq!(ui.session.view().history.len(), 1);

        let mut choice_session = session();
        choice_session
            .begin_choice(
                choice_session.root_owner(),
                &[
                    ChoiceInput {
                        key: "a",
                        label: "A",
                    },
                    ChoiceInput {
                        key: "b",
                        label: "B",
                    },
                    ChoiceInput {
                        key: "c",
                        label: "C",
                    },
                ],
            )
            .unwrap();
        assert!(SessionUi::new(choice_session).is_err());
    }

    #[test]
    fn failed_choice_owner_allocation_rolls_back_without_destroying_old_view() {
        let session = Session::new(
            SessionId(42),
            Epoch(3),
            SessionLimits {
                max_owners: 3,
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
        let available = ui
            .session
            .create_owner()
            .expect("the partial new button owner was revoked");
        ui.session.revoke_owner(available).unwrap();
        assert!(ui.session.is_owner_live(ui.source_owner));
    }
}
