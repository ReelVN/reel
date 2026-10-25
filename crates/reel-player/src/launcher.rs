//! Native library using the common stage, virtual list and accepted input geometry.
use crate::{assets::Assets, session::SessionUi};
use reel_engine::{Epoch, Session, SessionId, aside::GameSettings};
use reel_ui::{
    bridge::PresentationBridge,
    core::{Handled, PointerEvent, TextEvent, WidgetId},
    input::{ActionPhase, Control, GamepadButton, InputEvent, NavigationAction},
    stage::{
        InteractionState, NodeEvent, NodeId, NodeKind, Stage, VirtualListNavigation,
        VirtualListView, VirtualRow,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

type Error = Box<dyn std::error::Error>;
const ROW_HEIGHT: f64 = 90.;
const LIST_HEIGHT: f64 = 355.;
const PAGE_ROWS: usize = 4;
const PREFERENCES_BYTES: u64 = 64 << 10;
mod catalog;
#[cfg(test)]
mod tests;
mod view;
use crate::assets::worker::Worker;
use catalog::{Reply, Request};
use reel_core::{budget::QueueLimits, identity::Priority, wake::WakeSignal};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Game {
    title: String,
    package: PathBuf,
    bytes: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Launch(PathBuf),
    Exit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Screen {
    Library,
    Settings,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Picker {
    Theme,
    Language,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Language {
    #[default]
    Chinese,
    English,
    Japanese,
}
impl Language {
    fn pick<'a>(self, zh: &'a str, en: &'a str, ja: &'a str) -> &'a str {
        match self {
            Self::Chinese => zh,
            Self::English => en,
            Self::Japanese => ja,
        }
    }
    fn code(self) -> &'static str {
        self.pick("zh", "en", "ja")
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Launch(usize),
    Play,
    Previous,
    Next,
    Refresh,
    About,
    Settings,
    Picker(Picker),
    Theme(bool),
    Language(Language),
    Back,
    Exit,
    ConfirmExit,
}
#[derive(Clone, Default)]
struct Preferences {
    light: bool,
    language: Language,
    selected: Option<String>,
}
struct Button {
    action: Action,
    row: bool,
    primary: bool,
    interaction: InteractionState,
}
struct List {
    node: NodeId,
    scroll: NodeId,
    rows: BTreeMap<usize, NodeId>,
    viewport: f64,
    navigation: Option<VirtualListNavigation>,
}
pub(crate) struct Launcher {
    pub(crate) ui: SessionUi,
    pub(crate) settings: Arc<GameSettings>,
    root: PathBuf,
    games: Vec<Game>,
    selected: usize,
    scroll_offset: f64,
    preferences: Preferences,
    screen: Screen,
    error: Option<String>,
    confirm_exit: bool,
    about: bool,
    picker: Option<Picker>,
    loading: Option<Game>,
    loading_presented: bool,
    worker: Worker<Request, Reply>,
    scanning: bool,
    saving: bool,
    pending_save: Option<Preferences>,
    next_request: u64,
    stage: Stage,
    pages: BTreeMap<Screen, NodeId>,
    overlay: Option<NodeId>,
    page_focus: BTreeMap<Screen, NodeId>,
    buttons: BTreeMap<NodeId, Button>,
    list: Option<List>,
    pending_focus: Option<NodeId>,
    root_widget: Option<WidgetId>,
    outcome: Option<Outcome>,
}
impl Launcher {
    pub(crate) fn new(
        root: &Path,
        assets: Assets,
        error: Option<String>,
        wake: WakeSignal,
    ) -> Result<Self, Error> {
        let directory = root.to_path_buf();
        let worker = Worker::new(
            QueueLimits {
                max_requests: 2,
                max_bytes: PREFERENCES_BYTES * 2,
            },
            wake,
            move |request, cancelled| match request {
                Request::Scan => Reply::Scanned(
                    catalog::scan(&directory, cancelled)
                        .map_err(|e| format!("{}: {e}", directory.display())),
                ),
                Request::Save(preferences) => Reply::Saved(
                    catalog::save_preferences(&directory, &preferences)
                        .map_err(|e| format!("launcher.tsv: {e}")),
                ),
            },
            |_| {},
        );
        let mut launcher = Self {
            ui: SessionUi::new(Session::new(SessionId(1), Epoch(1))?)?.with_assets(assets),
            settings: Arc::new(GameSettings {
                title: "Reel".into(),
                canvas_size: Some([960, 544]),
                fonts: vec!["fonts/body.ttf".into()],
                ..Default::default()
            }),
            root: root.into(),
            games: Vec::new(),
            selected: 0,
            scroll_offset: 0.,
            preferences: Preferences::default(),
            screen: Screen::Library,
            error,
            confirm_exit: false,
            about: false,
            picker: None,
            loading: None,
            loading_presented: false,
            worker,
            scanning: false,
            saving: false,
            pending_save: None,
            next_request: 0,
            stage: Stage::default(),
            pages: BTreeMap::new(),
            overlay: None,
            page_focus: BTreeMap::new(),
            buttons: BTreeMap::new(),
            list: None,
            pending_focus: None,
            root_widget: None,
            outcome: None,
        };
        match catalog::read_preferences(root) {
            Ok(preferences) => launcher.preferences = preferences,
            Err(error) => launcher.report(format!("读取启动器设置失败：\n{error}")),
        }
        launcher.request(Request::Scan)?;
        launcher.scanning = true;
        launcher.rebuild()?;
        Ok(launcher)
    }
    fn text<'a>(&self, zh: &'a str, en: &'a str, ja: &'a str) -> &'a str {
        self.preferences.language.pick(zh, en, ja)
    }
    fn report(&mut self, message: String) {
        if let Some(error) = &mut self.error {
            error.push_str("\n\n");
            error.push_str(&message);
        } else {
            self.error = Some(message);
        }
    }
    fn request(&mut self, request: Request) -> Result<(), Error> {
        self.next_request += 1;
        self.worker
            .accept(
                self.next_request,
                request,
                PREFERENCES_BYTES,
                Priority::Visible,
            )
            .map_err(|e| e.reason.message.to_string())?;
        Ok(())
    }
    fn save_preferences(&mut self) -> Result<(), Error> {
        if self.saving {
            self.pending_save = Some(self.preferences.clone());
        } else {
            self.request(Request::Save(self.preferences.clone()))?;
            self.saving = true;
        }
        Ok(())
    }
    pub(crate) fn close_complete(&self) -> bool {
        self.worker.is_idle()
    }
    pub(crate) fn needs_tick(&self) -> bool {
        !self.worker.is_idle() || !self.ui.current_presentation_ready()
    }
    pub(crate) fn poll(&mut self) -> Result<(), Error> {
        let mut events = Vec::new();
        self.worker.drain(&mut events, 2);
        for event in events {
            match event {
                Reply::Scanned(result) => {
                    self.scanning = false;
                    match result {
                        Ok(games) => {
                            let selected = self
                                .games
                                .get(self.selected)
                                .map(|g| &g.title)
                                .or(self.preferences.selected.as_ref());
                            self.selected = selected
                                .and_then(|name| games.iter().position(|g| &g.title == name))
                                .unwrap_or(0);
                            self.games = games;
                            self.scroll_to_selected();
                        }
                        Err(error) => self.report(error),
                    }
                    self.rebuild()?;
                }
                Reply::Saved(result) => {
                    self.saving = false;
                    if let Err(error) = result {
                        self.loading = None;
                        self.report(error);
                        self.update_overlay()?;
                    }
                    if let Some(preferences) = self.pending_save.take() {
                        self.request(Request::Save(preferences))?;
                        self.saving = true;
                    }
                }
            }
        }
        if self.loading_presented && self.error.is_none() && self.worker.is_idle() {
            if let Some(game) = &self.loading {
                self.outcome = Some(Outcome::Launch(game.package.clone()));
            }
        }
        Ok(())
    }
    pub(crate) fn presented(
        &mut self,
        bridge: &PresentationBridge,
        frame: &crate::session::RecordedProjection,
    ) {
        if self.ui.presented(bridge, frame) && self.loading.is_some() {
            self.loading_presented = true;
            if self.worker.is_idle() && self.error.is_none() {
                self.outcome = Some(Outcome::Launch(
                    self.loading.as_ref().unwrap().package.clone(),
                ));
            }
        }
    }
    pub(crate) fn take_outcome(&mut self) -> Option<Outcome> {
        self.outcome.take()
    }
    pub(crate) fn exit_requested(&self) -> bool {
        self.outcome.is_some()
    }
    pub(crate) fn handle_pointer(
        &mut self,
        bridge: &mut PresentationBridge,
        event: PointerEvent,
    ) -> Result<Handled, Error> {
        if self.loading.is_some() {
            return Ok(Handled::Yes);
        }
        let dispatch = self.ui.handle_pointer(bridge, event)?;
        self.events(dispatch.events)?;
        Ok(dispatch.handled)
    }
    pub(crate) fn handle_text(
        &mut self,
        bridge: &mut PresentationBridge,
        event: TextEvent,
    ) -> Result<Handled, Error> {
        self.focus(bridge);
        if self.loading.is_some() {
            return Ok(Handled::Yes);
        }
        let shortcut = if self.ui.current_presentation_ready() {
            match &event {
                TextEvent::Navigation(event)
                    if event.action == NavigationAction::Back
                        && event.phase == ActionPhase::Released =>
                {
                    Some(Action::Back)
                }
                TextEvent::Input(InputEvent::Value {
                    control: Control::Button(button),
                    value,
                    ..
                }) if *value > 0.5 && !self.modal() => match button {
                    GamepadButton::Start if self.screen == Screen::Library => Some(Action::Refresh),
                    GamepadButton::Select if self.screen == Screen::Library => {
                        Some(Action::Settings)
                    }
                    GamepadButton::North if self.screen == Screen::Settings => Some(Action::About),
                    GamepadButton::LeftShoulder if self.screen == Screen::Library => {
                        Some(Action::Previous)
                    }
                    GamepadButton::RightShoulder if self.screen == Screen::Library => {
                        Some(Action::Next)
                    }
                    _ => None,
                },
                _ => None,
            }
        } else {
            None
        };
        if let Some(action) = shortcut {
            self.perform(action)?;
            return Ok(Handled::Yes);
        }
        let dispatch = self.ui.handle_text(bridge, event)?;
        self.events(dispatch.events)?;
        Ok(dispatch.handled)
    }
    pub(crate) fn sync(
        &mut self,
        bridge: &mut PresentationBridge,
        root: WidgetId,
    ) -> Result<(), Error> {
        self.root_widget = Some(root);
        // Layout can request a virtual row; mount it before recording this frame.
        for _ in 0..3 {
            self.ui.sync(bridge, root)?;
            self.focus(bridge);
            let before = self.stage.revision();
            let events = self.ui.take_events();
            self.events(events)?;
            if before == self.stage.revision() {
                return Ok(());
            }
        }
        self.ui.sync(bridge, root)
    }
    fn focus(&mut self, bridge: &mut PresentationBridge) {
        if let (Some(root), Some(node)) = (self.root_widget, self.pending_focus)
            && let Some(widget) = SessionUi::projected_node(bridge, root, node)
            && bridge.focus_on(Some(widget))
        {
            self.pending_focus = None;
        }
    }
    fn modal(&self) -> bool {
        self.error.is_some()
            || self.confirm_exit
            || self.about
            || self.picker.is_some()
            || self.loading.is_some()
    }
    fn events(&mut self, events: Vec<NodeEvent>) -> Result<(), Error> {
        let before = self.stage.revision();
        for event in events {
            match event {
                NodeEvent::PopupDismiss(_) => self.perform(Action::Back)?,
                NodeEvent::Click(click) => {
                    if let Some(button) = self.buttons.get(&click.node) {
                        self.perform(button.action)?;
                    }
                }
                NodeEvent::Interaction(event) => {
                    if let Some(button) = self.buttons.get_mut(&event.node) {
                        button.interaction = event.state;
                        if event.state.focused
                            && let Action::Launch(index) = button.action
                        {
                            self.selected = index;
                        }
                        self.feedback(event.node)?;
                    }
                }
                NodeEvent::Scroll(event)
                    if self
                        .list
                        .as_ref()
                        .is_some_and(|list| list.scroll == event.node) =>
                {
                    self.scroll_offset = event.offset.y;
                    self.list.as_mut().unwrap().viewport = event.viewport.height;
                    self.reconcile(&[])?;
                }
                NodeEvent::VirtualList(event)
                    if self
                        .list
                        .as_ref()
                        .is_some_and(|list| list.node == event.node) =>
                {
                    self.scroll_offset = event.offset.y;
                    self.list.as_mut().unwrap().viewport = event.viewport.height;
                    let navigation = event.navigation.map(|mut request| {
                        self.selected = request.index.min(self.games.len().saturating_sub(1));
                        request.ready = true;
                        request.resolved_index = self.selected;
                        request
                    });
                    self.list.as_mut().unwrap().navigation = navigation;
                    if event.navigation.is_some() {
                        self.scroll_to_selected();
                    }
                    self.reconcile(&event.retained)?;
                }
                _ => {}
            }
        }
        if before != self.stage.revision() {
            self.ui.set_feedback_stage(&self.stage);
        }
        Ok(())
    }
    fn perform(&mut self, action: Action) -> Result<(), Error> {
        if let Err(error) = self.act(action) {
            self.error = Some(error.to_string());
            self.loading = None;
            self.update_overlay()?;
        }
        Ok(())
    }
    fn act(&mut self, action: Action) -> Result<(), Error> {
        match action {
            Action::Play if !self.games.is_empty() => {
                return self.act(Action::Launch(self.selected));
            }
            Action::Launch(index) => {
                let game = self.games[index].clone();
                self.preferences.selected = Some(game.title.clone());
                self.save_preferences()?;
                self.selected = index;
                self.loading = Some(game);
                self.loading_presented = false;
            }
            Action::Previous | Action::Next if !self.games.is_empty() => {
                self.selected = if action == Action::Previous {
                    self.selected.saturating_sub(PAGE_ROWS)
                } else {
                    (self.selected + PAGE_ROWS).min(self.games.len() - 1)
                };
                self.scroll_to_selected();
                self.reconcile(&[])?;
                // Paging changes the intended activation target even when an
                // overscan row already exists in the accepted list projection.
                self.ui.clear_presented();
                self.pending_focus = self
                    .list
                    .as_ref()
                    .and_then(|list| list.rows.get(&self.selected).copied());
                return Ok(());
            }
            Action::Refresh if !self.scanning => {
                self.request(Request::Scan)?;
                self.scanning = true;
                return self.rebuild();
            }
            Action::About => self.about = true,
            Action::Settings => return self.navigate(Screen::Settings),
            Action::Picker(picker) => self.picker = Some(picker),
            Action::Theme(light) => {
                self.preferences.light = light;
                self.picker = None;
                self.save_preferences()?;
                return self.rebuild();
            }
            Action::Language(language) => {
                self.preferences.language = language;
                self.picker = None;
                self.save_preferences()?;
                return self.rebuild();
            }
            Action::Back if self.modal() => {
                self.error = None;
                self.confirm_exit = false;
                self.about = false;
                self.picker = None;
                self.loading = None;
            }
            Action::Back if self.screen != Screen::Library => {
                return self.navigate(Screen::Library);
            }
            Action::Back | Action::Exit => self.confirm_exit = true,
            Action::ConfirmExit => {
                self.outcome = Some(Outcome::Exit);
                return Ok(());
            }
            _ => return Ok(()),
        }
        self.update_overlay()
    }
    fn scroll_to_selected(&mut self) {
        let viewport = self
            .list
            .as_ref()
            .map_or(LIST_HEIGHT, |list| list.viewport.max(1.));
        let start = self.selected as f64 * ROW_HEIGHT;
        self.scroll_offset = self
            .scroll_offset
            .min(start)
            .max(start + ROW_HEIGHT - viewport)
            .clamp(
                0.,
                (self.games.len() as f64 * ROW_HEIGHT - viewport).max(0.),
            );
    }
    fn reconcile(&mut self, retained: &[NodeId]) -> Result<(), Error> {
        let Some(mut list) = self.list.take() else {
            return Ok(());
        };
        let first = (self.scroll_offset / ROW_HEIGHT).floor() as usize;
        let last = ((self.scroll_offset + list.viewport) / ROW_HEIGHT).ceil() as usize;
        let mut desired =
            (first.saturating_sub(1)..(last + 1).min(self.games.len())).collect::<BTreeSet<_>>();
        for (index, node) in &list.rows {
            if retained.contains(node) {
                desired.insert(*index);
            }
        }
        if let Some(request) = list.navigation {
            desired.insert(request.resolved_index);
        }
        for index in list
            .rows
            .keys()
            .copied()
            .filter(|index| !desired.contains(index))
            .collect::<Vec<_>>()
        {
            let node = list.rows.remove(&index).unwrap();
            self.stage.remove(node)?;
            self.buttons.remove(&node);
        }
        for index in desired {
            if !list.rows.contains_key(&index) {
                let node = self.game_row(index)?;
                self.stage.add(list.node, node, list.rows.len())?;
                list.rows.insert(index, node);
            }
        }
        let node = self.stage.node(list.node).unwrap();
        self.stage.update(
            list.node,
            NodeKind::VirtualList(VirtualListView {
                revision: 1,
                total_height: self.games.len() as f64 * ROW_HEIGHT,
                total_rows: self.games.len(),
                rows: list
                    .rows
                    .iter()
                    .map(|(&index, &node)| VirtualRow {
                        node,
                        index,
                        offset: index as f64 * ROW_HEIGHT,
                    })
                    .collect(),
                navigation: list.navigation,
            }),
            node.props.clone(),
        )?;
        let node = self.stage.node(list.scroll).unwrap();
        let NodeKind::Scroll {
            axis,
            follow_end,
            align,
            ..
        } = node.kind
        else {
            unreachable!()
        };
        self.stage.update(
            list.scroll,
            NodeKind::Scroll {
                axis,
                follow_end,
                align,
                offset: (0., self.scroll_offset).into(),
            },
            node.props.clone(),
        )?;
        self.list = Some(list);
        self.ui.set_stage(&self.stage);
        Ok(())
    }
}
