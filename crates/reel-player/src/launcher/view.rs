//! Library pages, shared chrome, controller hints and modal stage content.
use super::*;
use reel_ui::{
    stage::NodeProps,
    widgets::{ScrollAlign, ScrollAxis},
};

#[derive(Clone, Copy)]
struct Palette {
    background: [u8; 4],
    surface: [u8; 4],
    text: [u8; 4],
    muted: [u8; 4],
    separator: [u8; 4],
    highlight: [u8; 4],
    focus: [u8; 4],
    accent: [u8; 4],
    on_accent: [u8; 4],
}
impl Launcher {
    fn palette(&self) -> Palette {
        if self.preferences.light {
            Palette {
                background: [244, 245, 247, 255],
                surface: [255, 255, 255, 255],
                text: [37, 42, 50, 255],
                muted: [109, 117, 128, 255],
                separator: [218, 222, 229, 255],
                highlight: [246, 249, 255, 255],
                focus: [63, 118, 205, 255],
                accent: [35, 101, 187, 255],
                on_accent: [255, 255, 255, 255],
            }
        } else {
            Palette {
                background: [31, 35, 43, 255],
                surface: [48, 54, 65, 255],
                text: [246, 248, 252, 255],
                muted: [166, 177, 190, 255],
                separator: [72, 80, 93, 255],
                highlight: [53, 64, 83, 255],
                focus: [104, 165, 241, 255],
                accent: [111, 169, 245, 255],
                on_accent: [24, 34, 49, 255],
            }
        }
    }
    fn label(
        &mut self,
        text: impl Into<String>,
        rect: [f64; 4],
        size: f64,
        muted: bool,
    ) -> Result<NodeId, Error> {
        let [x, y, width, height] = rect;
        let palette = self.palette();
        Ok(self.stage.create(
            NodeKind::Text(text.into().into()),
            NodeProps {
                x,
                y,
                width: Some(width),
                height: Some(height),
                font_size: size,
                color: if muted { palette.muted } else { palette.text },
                text_wrap: true,
                clip: true,
                ..Default::default()
            },
            Vec::new(),
        )?)
    }
    fn line(&mut self, rect: [f64; 4]) -> Result<NodeId, Error> {
        let [x, y, width, height] = rect;
        Ok(self.stage.create(
            NodeKind::Stack,
            NodeProps {
                x,
                y,
                width: Some(width),
                height: Some(height),
                background: Some(self.palette().separator),
                ..Default::default()
            },
            Vec::new(),
        )?)
    }
    fn control(
        &mut self,
        rect: [f64; 4],
        children: Vec<NodeId>,
        action: Action,
        row: bool,
        primary: bool,
        enabled: bool,
    ) -> Result<NodeId, Error> {
        let [x, y, width, height] = rect;
        let node = self.stage.create(
            NodeKind::Button {
                label: None,
                listening: true,
                focus_on_pointer: true,
            },
            NodeProps {
                x,
                y,
                width: Some(width),
                height: Some(height),
                enabled,
                decorated: false,
                clip: true,
                corner_radius: 6.,
                ..Default::default()
            },
            children,
        )?;
        self.buttons.insert(
            node,
            Button {
                action,
                row,
                primary,
                interaction: InteractionState::default(),
            },
        );
        self.feedback(node)?;
        Ok(node)
    }
    fn button(
        &mut self,
        text: &str,
        rect: [f64; 4],
        action: Action,
        primary: bool,
    ) -> Result<NodeId, Error> {
        let child = self.label(
            text,
            [16., (rect[3] - 32.) / 2., rect[2] - 32., 32.],
            22.,
            false,
        )?;
        let node = self.stage.node(child).unwrap();
        let mut props = node.props.clone();
        props.text_align = reel_ui::TextAlign::Center;
        if primary {
            props.color = self.palette().on_accent;
        }
        self.stage.update(child, node.kind.clone(), props)?;
        self.control(rect, vec![child], action, false, primary, true)
    }
    fn hint(
        &mut self,
        badge: &str,
        label: &str,
        x: f64,
        width: f64,
        action: Option<Action>,
        enabled: bool,
    ) -> Result<NodeId, Error> {
        let badge_width = if badge.chars().count() > 1 {
            if badge.len() > 3 { 65. } else { 57. }
        } else {
            30.
        };
        let badge_node = self.label(
            badge,
            [8., 10., badge_width, 25.],
            if badge.chars().count() > 1 { 14. } else { 22. },
            false,
        )?;
        let node = self.stage.node(badge_node).unwrap();
        let mut props = node.props.clone();
        props.text_align = reel_ui::TextAlign::Center;
        props.text_wrap = false;
        props.border_width = if badge.is_ascii() { 1. } else { 0. };
        props.border_color = self.palette().muted;
        props.corner_radius = 4.;
        self.stage.update(badge_node, node.kind.clone(), props)?;
        let mut children = vec![badge_node];
        if !label.is_empty() {
            children.push(self.label(
                label,
                [badge_width + 18., 10., width - badge_width - 20., 26.],
                18.,
                false,
            )?);
        }
        if let Some(action) = action {
            self.control(
                [x, 486., width, 44.],
                children,
                action,
                true,
                false,
                enabled,
            )
        } else {
            Ok(self.stage.create(
                NodeKind::Stack,
                NodeProps {
                    x,
                    y: 486.,
                    width: Some(width),
                    height: Some(44.),
                    ..Default::default()
                },
                children,
            )?)
        }
    }
    pub(super) fn feedback(&mut self, node: NodeId) -> Result<(), Error> {
        let button = &self.buttons[&node];
        let state = button.interaction;
        let palette = self.palette();
        let view = self.stage.node(node).unwrap();
        let mut props = view.props.clone();
        props.background = if state.pressed || state.hover || state.focused {
            Some(palette.highlight)
        } else if button.primary {
            Some(palette.accent)
        } else if button.row {
            None
        } else {
            Some(palette.surface)
        };
        if button.primary {
            props.background = Some(palette.accent);
        }
        props.border_width = if state.focused { 3. } else { 0. };
        props.border_color = palette.focus;
        props.opacity = if props.enabled { 1. } else { 0.4 };
        self.stage.update(node, view.kind.clone(), props)?;
        self.ui.set_feedback_stage(&self.stage);
        Ok(())
    }
    pub(super) fn game_row(&mut self, index: usize) -> Result<NodeId, Error> {
        let title = self.games[index].title.clone();
        let text = self.label(title, [16., 27., 800., 38.], 28., false)?;
        let node = self.stage.node(text).unwrap();
        let mut props = node.props.clone();
        props.text_wrap = false;
        self.stage.update(text, node.kind.clone(), props)?;
        let separator = self.line([16., 89., 800., 1.])?;
        self.control(
            [0., 0., 832., ROW_HEIGHT],
            vec![text, separator],
            Action::Launch(index),
            true,
            false,
            true,
        )
    }
    pub(super) fn rebuild(&mut self) -> Result<(), Error> {
        self.buttons.clear();
        self.list = None;
        self.pending_focus = None;
        let mut nodes = Vec::new();
        let title = match self.screen {
            Screen::Library => "Reel",
            Screen::Information => "作品信息",
            Screen::Settings => "设置",
        };
        nodes.push(self.label(title, [65., 26., 400., 40.], 28., false)?);
        if self.screen == Screen::Library {
            nodes.push(self.label(
                format!("{} 部作品", self.games.len()),
                [143., 37., 350., 26.],
                17.,
                true,
            )?);
        }
        let status = self.label(
            concat!("v", env!("CARGO_PKG_VERSION")),
            [695., 35., 200., 26.],
            17.,
            true,
        )?;
        let node = self.stage.node(status).unwrap();
        let mut props = node.props.clone();
        props.text_align = reel_ui::TextAlign::End;
        self.stage.update(status, node.kind.clone(), props)?;
        nodes.push(status);
        nodes.push(self.line([48., 87., 864., 1.])?);
        nodes.push(self.line([48., 471., 864., 1.])?);
        match self.screen {
            Screen::Library => self.library(&mut nodes)?,
            Screen::Information => self.information(&mut nodes)?,
            Screen::Settings => self.options(&mut nodes)?,
        }
        if self.modal() {
            nodes.push(self.dialog()?);
        }
        let root = self.stage.create(
            NodeKind::Stack,
            NodeProps {
                width: Some(960.),
                height: Some(544.),
                background: Some(self.palette().background),
                ..Default::default()
            },
            nodes,
        )?;
        self.stage.replace(root)?;
        if self.list.is_some() {
            self.reconcile(&[])?;
            if !self.modal() {
                self.pending_focus = self
                    .list
                    .as_ref()
                    .and_then(|list| list.rows.get(&self.selected).copied());
            }
        }
        self.ui.set_stage(&self.stage);
        Ok(())
    }
    fn library(&mut self, nodes: &mut Vec<NodeId>) -> Result<(), Error> {
        if self.games.is_empty() {
            for node in [
                self.label("未找到作品", [64., 175., 832., 44.], 28., false)?,
                self.label(
                    self.root.display().to_string(),
                    [64., 231., 832., 60.],
                    20.,
                    true,
                )?,
                self.label(
                    "每个作品目录放入 game.asb，复制完成后刷新。",
                    [64., 301., 832., 32.],
                    20.,
                    true,
                )?,
            ] {
                let view = self.stage.node(node).unwrap();
                let mut props = view.props.clone();
                props.text_align = reel_ui::TextAlign::Center;
                self.stage.update(node, view.kind.clone(), props)?;
                nodes.push(node);
            }
            let refresh = self.button("刷新", [360., 363., 240., 56.], Action::Refresh, true)?;
            self.pending_focus = Some(refresh);
            nodes.push(refresh);
        } else {
            let node = self.stage.create(
                NodeKind::VirtualList(VirtualListView {
                    revision: 1,
                    total_height: self.games.len() as f64 * ROW_HEIGHT,
                    total_rows: self.games.len(),
                    rows: Arc::from([]),
                    navigation: None,
                }),
                NodeProps {
                    width: Some(832.),
                    ..Default::default()
                },
                Vec::new(),
            )?;
            let scroll = self.stage.create(
                NodeKind::Scroll {
                    axis: ScrollAxis::Vertical,
                    follow_end: false,
                    align: ScrollAlign::Start,
                    offset: (0., self.scroll_offset).into(),
                },
                NodeProps {
                    x: 64.,
                    y: 100.,
                    width: Some(832.),
                    height: Some(LIST_HEIGHT),
                    clip: true,
                    ..Default::default()
                },
                vec![node],
            )?;
            nodes.push(scroll);
            self.list = Some(List {
                node,
                scroll,
                rows: BTreeMap::new(),
                viewport: LIST_HEIGHT,
                navigation: None,
            });
        }
        nodes.push(self.hint("SELECT", "设置", 55., 145., Some(Action::Settings), true)?);
        if !self.games.is_empty() {
            nodes.push(self.hint("L", "", 206., 46., Some(Action::Previous), true)?);
            nodes.push(self.hint("R", "翻页", 254., 102., Some(Action::Next), true)?);
        }
        nodes.push(self.hint("START", "刷新", 368., 152., Some(Action::Refresh), true)?);
        if !self.games.is_empty() {
            nodes.push(self.hint("△", "详情", 665., 111., Some(Action::Information), true)?);
            nodes.push(self.hint("×", "启动", 790., 115., Some(Action::Play), true)?);
        }
        Ok(())
    }
    fn subpage(
        &mut self,
        nodes: &mut Vec<NodeId>,
        title: &str,
        description: &str,
    ) -> Result<(), Error> {
        nodes.push(self.label(title, [64., 112., 190., 110.], 28., false)?);
        nodes.push(self.label(description, [64., 236., 190., 168.], 18., true)?);
        nodes.push(self.line([282., 100., 1., 355.])?);
        nodes.push(self.hint("○", "返回", 665., 111., Some(Action::Back), true)?);
        nodes.push(self.hint("×", "选择", 790., 115., None, true)?);
        Ok(())
    }
    fn information(&mut self, nodes: &mut Vec<NodeId>) -> Result<(), Error> {
        let game = self.games[self.selected].clone();
        self.subpage(nodes, &game.title, "作品保留各自的资源与存档。")?;
        let directory = game.package.parent().unwrap();
        nodes.push(self.label("作品目录", [311., 111., 585., 27.], 17., true)?);
        nodes.push(self.label(
            directory.display().to_string(),
            [311., 148., 585., 71.],
            18.,
            false,
        )?);
        nodes.push(self.label("存档目录", [311., 239., 585., 27.], 17., true)?);
        nodes.push(self.label(
            directory.join("saves").display().to_string(),
            [311., 276., 585., 71.],
            18.,
            false,
        )?);
        let play = self.button("启动作品", [311., 383., 280., 56.], Action::Play, true)?;
        self.pending_focus = Some(play);
        nodes.push(play);
        Ok(())
    }
    fn options(&mut self, nodes: &mut Vec<NodeId>) -> Result<(), Error> {
        self.subpage(nodes, "启动器设置", "主题与上次启动的作品会自动保存。")?;
        let theme = self.button(
            if self.preferences.light {
                "主题：浅色"
            } else {
                "主题：深色"
            },
            [311., 112., 585., 56.],
            Action::Theme,
            false,
        )?;
        self.pending_focus = Some(theme);
        nodes.push(theme);
        nodes.push(self.label("操作", [311., 204., 585., 32.], 22., false)?);
        nodes.push(self.label("方向键 / 左摇杆选择，× 确认，○ 返回。\n触摸作品启动，上下拖动列表滚动。\n游戏中按 SELECT + START 返回作品列表。",[311.,248.,585.,103.],18.,true)?);
        nodes.push(self.button("退出启动器", [311., 383., 280., 56.], Action::Exit, false)?);
        Ok(())
    }
    fn dialog(&mut self) -> Result<NodeId, Error> {
        let mut nodes = Vec::new();
        let error = self.error.clone();
        let (rect, title, message) = if let Some(error) = error {
            ([96., 90., 768., 365.], "启动失败", error)
        } else {
            (
                [200., 150., 560., 244.],
                "退出启动器",
                "确定退出 Reel？".into(),
            )
        };
        let [x, y, width, height] = rect;
        let card = self.stage.create(
            NodeKind::Stack,
            NodeProps {
                x,
                y,
                width: Some(width),
                height: Some(height),
                background: Some(self.palette().surface),
                border_width: 1.,
                border_color: self.palette().separator,
                corner_radius: 8.,
                ..Default::default()
            },
            Vec::new(),
        )?;
        nodes.push(card);
        nodes.push(self.label(title, [x + 32., y + 25., width - 64., 42.], 28., false)?);
        nodes.push(self.line([x + 32., y + 85., width - 64., 1.])?);
        let text = self.label(message, [0., 0., width - 64., 0.], 22., false)?;
        let node = self.stage.node(text).unwrap();
        let mut props = node.props.clone();
        props.height = None;
        props.clip = false;
        self.stage.update(text, node.kind.clone(), props)?;
        nodes.push(self.stage.create(
            NodeKind::Scroll {
                axis: ScrollAxis::Vertical,
                follow_end: false,
                align: ScrollAlign::Start,
                offset: Default::default(),
            },
            NodeProps {
                x: x + 32.,
                y: y + 105.,
                width: Some(width - 64.),
                height: Some(height - 190.),
                clip: true,
                ..Default::default()
            },
            vec![text],
        )?);
        let back = self.button(
            if self.error.is_some() {
                "返回"
            } else {
                "取消"
            },
            [x + 32., y + height - 72., 224., 48.],
            Action::Back,
            false,
        )?;
        nodes.push(back);
        self.pending_focus = Some(back);
        if self.error.is_none() {
            nodes.push(self.button(
                "退出",
                [x + width - 256., y + height - 72., 224., 48.],
                Action::ConfirmExit,
                true,
            )?);
        }
        Ok(self.stage.create(
            NodeKind::Stack,
            NodeProps {
                width: Some(960.),
                height: Some(544.),
                modal: true,
                background: Some([0, 0, 0, 140]),
                ..Default::default()
            },
            nodes,
        )?)
    }
}
