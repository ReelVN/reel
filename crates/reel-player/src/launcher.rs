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
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

type Error = Box<dyn std::error::Error>;
const ROW_HEIGHT: f64 = 90.;
const LIST_HEIGHT: f64 = 355.;
const PAGE_ROWS: usize = 4;
const PREFERENCES_BYTES: u64 = 64 << 10;
#[cfg(test)]
mod tests;
mod view;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Game {
    title: String,
    package: PathBuf,
}
fn scan(root: &Path) -> Result<Vec<Game>, Error> {
    let mut games = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let package = entry.path().join("game.asb");
            if package.is_file() {
                games.push(Game {
                    title: entry.file_name().to_string_lossy().into_owned(),
                    package,
                });
            }
        }
    }
    games.sort_by(|a, b| a.title.cmp(&b.title));
    Ok(games)
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Launch(PathBuf),
    Exit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Library,
    Information,
    Settings,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Launch(usize),
    Play,
    Previous,
    Next,
    Refresh,
    Information,
    Settings,
    Theme,
    Back,
    Exit,
    ConfirmExit,
}
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct Preferences {
    light: bool,
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
    stage: Stage,
    buttons: BTreeMap<NodeId, Button>,
    list: Option<List>,
    pending_focus: Option<NodeId>,
    root_widget: Option<WidgetId>,
    outcome: Option<Outcome>,
}
impl Launcher {
    pub(crate) fn new(root: &Path, assets: Assets, error: Option<String>) -> Result<Self, Error> {
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
            stage: Stage::default(),
            buttons: BTreeMap::new(),
            list: None,
            pending_focus: None,
            root_widget: None,
            outcome: None,
        };
        match launcher.read_preferences() {
            Ok(preferences) => launcher.preferences = preferences,
            Err(error) => launcher.report(format!("读取启动器设置失败：\n{error}")),
        }
        match scan(root) {
            Ok(games) => launcher.games = games,
            Err(error) => launcher.report(format!("读取作品目录失败：\n{error}")),
        }
        launcher.selected = launcher
            .preferences
            .selected
            .as_ref()
            .and_then(|name| launcher.games.iter().position(|game| &game.title == name))
            .unwrap_or(0);
        launcher.scroll_to_selected();
        launcher.rebuild()?;
        Ok(launcher)
    }
    fn read_preferences(&self) -> Result<Preferences, Error> {
        let file = match reel_storage::open(&self.root.join("launcher.preferences")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Preferences::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(PREFERENCES_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > PREFERENCES_BYTES {
            return Err("启动器设置超过 64 KiB".into());
        }
        Ok(postcard::from_bytes(&bytes)?)
    }
    fn report(&mut self, message: String) {
        if let Some(error) = &mut self.error {
            error.push_str("\n\n");
            error.push_str(&message);
        } else {
            self.error = Some(message);
        }
    }
    fn save_preferences(&self, preferences: &Preferences) -> Result<(), Error> {
        let bytes = postcard::to_stdvec(preferences)?;
        let result =
            reel_storage::publish(&self.root.join("launcher.preferences"), true, |writer| {
                writer.write_all(&bytes)
            })?;
        if let Some(warning) = result.cleanup_warning {
            eprintln!("launcher settings: {warning}");
        }
        Ok(())
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
                    GamepadButton::North
                        if self.screen == Screen::Library && !self.games.is_empty() =>
                    {
                        Some(Action::Information)
                    }
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
                break;
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
        self.error.is_some() || self.confirm_exit
    }
    fn events(&mut self, events: Vec<NodeEvent>) -> Result<(), Error> {
        for event in events {
            match event {
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
        Ok(())
    }
    fn perform(&mut self, action: Action) -> Result<(), Error> {
        if let Err(error) = self.act(action) {
            self.error = Some(error.to_string());
            self.rebuild()?;
        }
        Ok(())
    }
    fn act(&mut self, action: Action) -> Result<(), Error> {
        match action {
            Action::Play if !self.games.is_empty() => {
                return self.act(Action::Launch(self.selected));
            }
            Action::Launch(index) => {
                let game = &self.games[index];
                let preferences = Preferences {
                    selected: Some(game.title.clone()),
                    ..self.preferences.clone()
                };
                if let Err(error) = self.save_preferences(&preferences) {
                    eprintln!("launcher selection: {error}");
                }
                self.preferences = preferences;
                self.selected = index;
                self.outcome = Some(Outcome::Launch(game.package.clone()));
                return Ok(());
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
            Action::Refresh => {
                let selected = self.games.get(self.selected).map(|game| game.title.clone());
                self.games = scan(&self.root)?;
                self.selected = selected
                    .and_then(|name| self.games.iter().position(|game| game.title == name))
                    .unwrap_or(0);
                self.scroll_to_selected();
            }
            Action::Information if !self.games.is_empty() => self.screen = Screen::Information,
            Action::Settings => self.screen = Screen::Settings,
            Action::Theme => {
                let preferences = Preferences {
                    light: !self.preferences.light,
                    ..self.preferences.clone()
                };
                self.save_preferences(&preferences)?;
                self.preferences = preferences;
            }
            Action::Back if self.modal() => {
                self.error = None;
                self.confirm_exit = false;
            }
            Action::Back if self.screen != Screen::Library => self.screen = Screen::Library,
            Action::Back | Action::Exit => self.confirm_exit = true,
            Action::ConfirmExit => {
                self.outcome = Some(Outcome::Exit);
                return Ok(());
            }
            _ => return Ok(()),
        }
        self.rebuild()
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
