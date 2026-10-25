//! Native library view built from the same stage, buttons and presented input as stories.
use crate::{assets::Assets, session::SessionUi};
use reel_engine::{Epoch, Session, SessionId, aside::GameSettings};
use reel_ui::{
    bridge::PresentationBridge,
    core::{Handled, PointerEvent, TextEvent, WidgetId},
    input::{ActionPhase, NavigationAction},
    stage::{NodeEvent, NodeId, NodeKind, NodeProps, Stage},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

type Error = Box<dyn std::error::Error>;
const ROWS: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Game {
    title: String,
    package: PathBuf,
}

#[cfg(test)]
mod tests;

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
    games.sort_by(|left, right| left.title.cmp(&right.title));
    Ok(games)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Launch(PathBuf),
    Exit,
}

#[derive(Clone)]
enum Action {
    Launch(PathBuf),
    Previous,
    Next,
    Refresh,
    Back,
    Exit,
}

pub(crate) struct Launcher {
    pub(crate) ui: SessionUi,
    pub(crate) settings: Arc<GameSettings>,
    root: PathBuf,
    games: Vec<Game>,
    page: usize,
    error: Option<String>,
    stage: Stage,
    buttons: Vec<(NodeId, Action)>,
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
            page: 0,
            error,
            stage: Stage::default(),
            buttons: Vec::new(),
            outcome: None,
        };
        match scan(root) {
            Ok(games) => launcher.games = games,
            Err(error) => launcher.error = Some(format!("读取作品目录失败：\n{error}")),
        }
        launcher.rebuild()?;
        Ok(launcher)
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
        let back = self.error.is_some()
            && matches!(&event, TextEvent::Navigation(event)
                if event.action == NavigationAction::Back && event.phase == ActionPhase::Released);
        let dispatch = self.ui.handle_text(bridge, event)?;
        self.events(dispatch.events)?;
        if back {
            self.error = None;
            self.rebuild()?;
            return Ok(Handled::Yes);
        }
        Ok(dispatch.handled)
    }

    pub(crate) fn sync(
        &mut self,
        bridge: &mut PresentationBridge,
        root: WidgetId,
    ) -> Result<(), Error> {
        self.ui.sync(bridge, root)
    }

    fn events(&mut self, events: Vec<NodeEvent>) -> Result<(), Error> {
        for event in events {
            let NodeEvent::Click(click) = event else {
                continue;
            };
            let Some(action) = self
                .buttons
                .iter()
                .find_map(|(node, action)| (*node == click.node).then(|| action.clone()))
            else {
                continue;
            };
            match action {
                Action::Launch(path) => self.outcome = Some(Outcome::Launch(path)),
                Action::Exit => self.outcome = Some(Outcome::Exit),
                Action::Previous => {
                    self.page -= 1;
                    self.rebuild()?;
                }
                Action::Next => {
                    self.page += 1;
                    self.rebuild()?;
                }
                Action::Refresh => {
                    match scan(&self.root) {
                        Ok(games) => {
                            self.games = games;
                            self.error = None;
                            self.page = self.page.min(self.pages() - 1);
                        }
                        Err(error) => self.error = Some(format!("读取作品目录失败：\n{error}")),
                    }
                    self.rebuild()?;
                }
                Action::Back => {
                    self.error = None;
                    self.rebuild()?;
                }
            }
        }
        Ok(())
    }

    fn pages(&self) -> usize {
        self.games.len().div_ceil(ROWS).max(1)
    }

    fn text(&mut self, value: String, rect: [f64; 4], size: f64) -> Result<NodeId, Error> {
        let [x, y, width, height] = rect;
        Ok(self.stage.create(
            NodeKind::Text(value.into()),
            NodeProps {
                x,
                y,
                width: Some(width),
                height: Some(height),
                font_size: size,
                clip: true,
                ..Default::default()
            },
            Vec::new(),
        )?)
    }

    fn button(
        &mut self,
        label: String,
        rect: [f64; 4],
        enabled: bool,
        action: Action,
    ) -> Result<NodeId, Error> {
        let [x, y, width, height] = rect;
        let node = self.stage.create(
            NodeKind::Button {
                label: Some(label),
                listening: true,
                focus_on_pointer: true,
            },
            NodeProps {
                x,
                y,
                width: Some(width),
                height: Some(height),
                enabled,
                font_size: 24.,
                clip: true,
                padding: 10.,
                corner_radius: 6.,
                ..Default::default()
            },
            Vec::new(),
        )?;
        self.buttons.push((node, action));
        Ok(node)
    }

    fn rebuild(&mut self) -> Result<(), Error> {
        self.buttons.clear();
        let mut nodes = Vec::new();
        if let Some(error) = self.error.clone() {
            nodes.push(self.text("启动失败".into(), [32., 24., 896., 48.], 32.)?);
            nodes.push(self.text(error, [32., 94., 896., 308.], 22.)?);
            nodes.push(self.button(
                "返回作品列表".into(),
                [32., 442., 256., 48.],
                true,
                Action::Back,
            )?);
            nodes.push(self.button(
                "刷新".into(),
                [600., 442., 150., 48.],
                true,
                Action::Refresh,
            )?);
            nodes.push(self.button("退出".into(), [770., 442., 158., 48.], true, Action::Exit)?);
        } else {
            nodes.push(self.text("作品列表".into(), [32., 20., 896., 44.], 32.)?);
            nodes.push(self.text(self.root.display().to_string(), [32., 66., 896., 24.], 18.)?);
            if self.games.is_empty() {
                nodes.push(
                    self.text(
                        "将作品目录放入此目录，每个作品目录包含 game.asb。\n复制完成后选择“刷新”。"
                            .into(),
                        [32., 130., 896., 220.],
                        26.,
                    )?,
                );
            }
            let start = self.page * ROWS;
            for (index, game) in self
                .games
                .iter()
                .skip(start)
                .take(ROWS)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .enumerate()
            {
                nodes.push(self.button(
                    game.title,
                    [32., 98. + index as f64 * 54., 896., 48.],
                    true,
                    Action::Launch(game.package),
                )?);
            }
            nodes.push(self.button(
                "上一页".into(),
                [32., 442., 140., 48.],
                self.page > 0,
                Action::Previous,
            )?);
            nodes.push(self.button(
                "下一页".into(),
                [184., 442., 140., 48.],
                self.page + 1 < self.pages(),
                Action::Next,
            )?);
            nodes.push(self.text(
                format!(
                    "{}/{} 页 · {} 个作品",
                    self.page + 1,
                    self.pages(),
                    self.games.len()
                ),
                [344., 450., 240., 36.],
                20.,
            )?);
            nodes.push(self.button(
                "刷新".into(),
                [600., 442., 150., 48.],
                true,
                Action::Refresh,
            )?);
            nodes.push(self.button("退出".into(), [770., 442., 158., 48.], true, Action::Exit)?);
        }
        nodes.push(self.text(
            "方向键 / 左摇杆选择　× 确认　触摸启动　游戏中 SELECT + START 返回".into(),
            [32., 510., 896., 26.],
            18.,
        )?);
        let root = self.stage.create(
            NodeKind::Stack,
            NodeProps {
                width: Some(960.),
                height: Some(544.),
                background: Some([25, 34, 49, 255]),
                ..Default::default()
            },
            nodes,
        )?;
        self.stage.replace(root)?;
        self.ui.set_stage(&self.stage);
        Ok(())
    }
}
