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
                focusable: true,
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
        Ok(())
    }
    pub(super) fn game_row(&mut self, index: usize) -> Result<NodeId, Error> {
        let game = &self.games[index];
        let title = game.title.clone();
        let bytes = game.bytes;
        let size = if bytes >= 1 << 30 {
            format!("{:.2} GiB", bytes as f64 / (1u64 << 30) as f64)
        } else {
            format!("{:.1} MiB", bytes as f64 / (1u64 << 20) as f64)
        };
        let text = self.label(title, [16., 27., 654., 38.], 28., false)?;
        self.align(text, reel_ui::TextAlign::Start, false)?;
        let detail = self.label(size, [686., 34., 130., 28.], 17., true)?;
        self.align(detail, reel_ui::TextAlign::End, false)?;
        let separator = self.line([16., 89., 800., 1.])?;
        self.control(
            [0., 0., 832., ROW_HEIGHT],
            vec![text, detail, separator],
            Action::Launch(index),
            true,
            false,
            true,
        )
    }
    fn align(&mut self, node: NodeId, align: reel_ui::TextAlign, wrap: bool) -> Result<(), Error> {
        let view = self.stage.node(node).unwrap();
        let mut props = view.props.clone();
        props.text_align = align;
        props.text_wrap = wrap;
        self.stage.update(node, view.kind.clone(), props)?;
        Ok(())
    }
    pub(super) fn rebuild(&mut self) -> Result<(), Error> {
        self.buttons.clear();
        self.list = None;
        self.overlay = None;
        self.pages.clear();
        self.page_focus.clear();
        self.pending_focus = None;
        let mut pages = Vec::new();
        for screen in [Screen::Library, Screen::Settings] {
            let mut nodes = Vec::new();
            let title = if screen == Screen::Library {
                "Reel"
            } else {
                self.text("设置", "Settings", "設定")
            };
            nodes.push(self.label(title, [65., 26., 400., 40.], 28., false)?);
            if screen == Screen::Library {
                nodes.push(self.label(
                    format!(
                        "{} {}",
                        self.games.len(),
                        self.text("部作品", "games", "作品")
                    ),
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
            self.align(status, reel_ui::TextAlign::End, false)?;
            nodes.push(status);
            nodes.push(self.line([48., 87., 864., 1.])?);
            nodes.push(self.line([48., 471., 864., 1.])?);
            match screen {
                Screen::Library => self.library(&mut nodes)?,
                Screen::Settings => self.options(&mut nodes)?,
            }
            let page = self.stage.create(
                NodeKind::Stack,
                NodeProps {
                    width: Some(960.),
                    height: Some(544.),
                    visible: screen == self.screen,
                    ..Default::default()
                },
                nodes,
            )?;
            pages.push(page);
            self.pages.insert(screen, page);
        }
        let root = self.stage.create(
            NodeKind::Stack,
            NodeProps {
                width: Some(960.),
                height: Some(544.),
                background: Some(self.palette().background),
                ..Default::default()
            },
            pages,
        )?;
        self.stage.replace(root)?;
        self.reconcile(&[])?;
        self.restore_focus();
        self.update_overlay()
    }
    fn restore_focus(&mut self) {
        self.pending_focus = if self.screen == Screen::Library {
            self.list
                .as_ref()
                .and_then(|list| list.rows.get(&self.selected).copied())
                .or_else(|| self.page_focus.get(&Screen::Library).copied())
        } else {
            self.page_focus.get(&self.screen).copied()
        };
    }
    pub(super) fn navigate(&mut self, screen: Screen) -> Result<(), Error> {
        self.screen = screen;
        for (&page_screen, &node) in &self.pages {
            let page = self.stage.node(node).unwrap();
            let mut props = page.props.clone();
            props.visible = page_screen == screen;
            self.stage.update(node, page.kind.clone(), props)?;
        }
        self.restore_focus();
        self.ui.set_stage(&self.stage);
        Ok(())
    }
    pub(super) fn update_overlay(&mut self) -> Result<(), Error> {
        if let Some(node) = self.overlay.take() {
            self.stage.remove(node)?;
            self.buttons
                .retain(|node, _| self.stage.node(*node).is_some());
        }
        self.restore_focus();
        if self.modal() {
            let overlay = if self.loading.is_some() {
                self.loading_view()?
            } else if let Some(picker) = self.picker {
                self.dropdown(picker)?
            } else {
                self.dialog()?
            };
            let root = self.stage.root().unwrap();
            let position = self.stage.node(root).unwrap().children.len();
            self.stage.add(root, overlay, position)?;
            self.overlay = Some(overlay);
        }
        self.ui.set_stage(&self.stage);
        Ok(())
    }
    fn library(&mut self, nodes: &mut Vec<NodeId>) -> Result<(), Error> {
        if self.games.is_empty() {
            let message = if self.scanning {
                self.text("正在读取…", "Loading library…", "読み込み中…")
            } else {
                self.text("未找到作品", "No games found", "作品が見つかりません")
            };
            let title = self.label(message, [64., 194., 832., 44.], 28., false)?;
            self.align(title, reel_ui::TextAlign::Center, false)?;
            nodes.push(title);
            let path = self.label(
                self.root.display().to_string(),
                [64., 255., 832., 60.],
                20.,
                true,
            )?;
            self.align(path, reel_ui::TextAlign::Center, true)?;
            nodes.push(path);
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
        let settings = self.hint(
            "SELECT",
            self.text("设置", "Settings", "設定"),
            55.,
            175.,
            Some(Action::Settings),
            true,
        )?;
        self.page_focus.insert(Screen::Library, settings);
        nodes.push(settings);
        if !self.games.is_empty() {
            nodes.push(self.hint("L", "", 236., 46., Some(Action::Previous), true)?);
            nodes.push(self.hint(
                "R",
                self.text("翻页", "Page", "ページ"),
                284.,
                120.,
                Some(Action::Next),
                true,
            )?);
        }
        nodes.push(self.hint(
            "START",
            self.text("刷新", "Refresh", "更新"),
            410.,
            178.,
            Some(Action::Refresh),
            !self.scanning,
        )?);
        if !self.games.is_empty() {
            nodes.push(self.hint(
                "×",
                self.text("启动", "Play", "起動"),
                770.,
                135.,
                Some(Action::Play),
                true,
            )?);
        }
        Ok(())
    }
    fn selector(
        &mut self,
        title: &str,
        value: &str,
        y: f64,
        picker: Picker,
    ) -> Result<NodeId, Error> {
        let label = self.label(title, [18., 18., 310., 36.], 24., false)?;
        let value = self.label(value, [340., 20., 340., 32.], 22., true)?;
        self.align(value, reel_ui::TextAlign::End, false)?;
        let arrow = self.label("▾", [700., 18., 32., 36.], 24., true)?;
        let separator = self.line([16., 73., 720., 1.])?;
        self.control(
            [104., y, 752., 74.],
            vec![label, value, arrow, separator],
            Action::Picker(picker),
            true,
            false,
            true,
        )
    }
    fn options(&mut self, nodes: &mut Vec<NodeId>) -> Result<(), Error> {
        let language = match self.preferences.language {
            Language::Chinese => "简体中文",
            Language::English => "English",
            Language::Japanese => "日本語",
        };
        let first = self.selector(
            self.text("语言", "Language", "言語"),
            language,
            120.,
            Picker::Language,
        )?;
        nodes.push(first);
        self.page_focus.insert(Screen::Settings, first);
        let theme = if self.preferences.light {
            self.text("浅色", "Light", "ライト")
        } else {
            self.text("深色", "Dark", "ダーク")
        };
        nodes.push(self.selector(
            self.text("主题", "Theme", "テーマ"),
            theme,
            210.,
            Picker::Theme,
        )?);
        nodes.push(self.button(
            self.text("关于", "About", "情報"),
            [120., 360., 230., 56.],
            Action::About,
            false,
        )?);
        nodes.push(self.button(
            self.text("退出", "Exit", "終了"),
            [610., 360., 230., 56.],
            Action::Exit,
            false,
        )?);
        nodes.push(self.hint(
            "○",
            self.text("返回", "Back", "戻る"),
            620.,
            130.,
            Some(Action::Back),
            true,
        )?);
        nodes.push(self.hint(
            "×",
            self.text("选择", "Select", "選択"),
            770.,
            135.,
            None,
            true,
        )?);
        Ok(())
    }
    fn dropdown(&mut self, picker: Picker) -> Result<NodeId, Error> {
        let choices = match picker {
            Picker::Theme => vec![
                (
                    self.text("深色", "Dark", "ダーク"),
                    Action::Theme(false),
                    !self.preferences.light,
                ),
                (
                    self.text("浅色", "Light", "ライト"),
                    Action::Theme(true),
                    self.preferences.light,
                ),
            ],
            Picker::Language => vec![
                (
                    "简体中文",
                    Action::Language(Language::Chinese),
                    self.preferences.language == Language::Chinese,
                ),
                (
                    "English",
                    Action::Language(Language::English),
                    self.preferences.language == Language::English,
                ),
                (
                    "日本語",
                    Action::Language(Language::Japanese),
                    self.preferences.language == Language::Japanese,
                ),
            ],
        };
        let y = if picker == Picker::Language {
            120.
        } else {
            210.
        };
        let mut rows = Vec::new();
        for (index, (label, action, selected)) in choices.iter().enumerate() {
            let text = self.label(*label, [20., 14., 246., 34.], 22., false)?;
            let mut children = vec![text];
            if *selected {
                children.push(self.label("✓", [278., 14., 30., 34.], 22., false)?);
            }
            let row = self.control(
                [6., 6. + index as f64 * 60., 328., 60.],
                children,
                *action,
                true,
                false,
                true,
            )?;
            if *selected {
                self.pending_focus = Some(row);
            }
            rows.push(row);
        }
        let popup = self.stage.create(
            NodeKind::Popup(reel_ui::stage::PopupView {
                placement: reel_ui::widgets::PopupPlacement::BottomEnd,
                offset: 4.,
                listening: true,
            }),
            NodeProps {
                width: Some(340.),
                height: Some(12. + choices.len() as f64 * 60.),
                modal: true,
                background: Some(self.palette().surface),
                border_width: 1.,
                border_color: self.palette().separator,
                corner_radius: 10.,
                ..Default::default()
            },
            rows,
        )?;
        Ok(self.stage.create(
            NodeKind::Stack,
            NodeProps {
                x: 504.,
                y,
                width: Some(340.),
                height: Some(74.),
                ..Default::default()
            },
            vec![popup],
        )?)
    }
    fn loading_view(&mut self) -> Result<NodeId, Error> {
        let title = self.loading.as_ref().unwrap().title.clone();
        let text = self.label(
            self.text("正在加载…", "Loading…", "読み込み中…"),
            [80., 228., 800., 44.],
            28.,
            false,
        )?;
        self.align(text, reel_ui::TextAlign::Center, false)?;
        let game = self.label(title, [80., 292., 800., 36.], 22., true)?;
        self.align(game, reel_ui::TextAlign::Center, false)?;
        Ok(self.stage.create(
            NodeKind::Stack,
            NodeProps {
                width: Some(960.),
                height: Some(544.),
                modal: true,
                background: Some(self.palette().background),
                ..Default::default()
            },
            vec![text, game],
        )?)
    }
    fn dialog(&mut self) -> Result<NodeId, Error> {
        let (title, message, height) = if let Some(error) = &self.error {
            (
                self.text("发生错误", "Error", "エラー"),
                error.clone(),
                380.,
            )
        } else if self.about {
            ("Reel", format!("v{}", env!("CARGO_PKG_VERSION")), 260.)
        } else {
            (
                self.text("退出 Reel", "Exit Reel", "Reel を終了"),
                self.text("确定退出？", "Exit the launcher?", "終了しますか？")
                    .into(),
                280.,
            )
        };
        let width = 720.;
        let x = (960. - width) / 2.;
        let y = (544. - height) / 2.;
        let heading = self.label(title, [48., 32., width - 96., 42.], 28., false)?;
        self.align(heading, reel_ui::TextAlign::Center, false)?;
        let text = self.label(message, [0., 0., width - 112., 0.], 22., false)?;
        let node = self.stage.node(text).unwrap();
        let mut props = node.props.clone();
        props.height = None;
        props.clip = false;
        props.text_align = reel_ui::TextAlign::Center;
        self.stage.update(text, node.kind.clone(), props)?;
        let scroll = self.stage.create(
            NodeKind::Scroll {
                axis: ScrollAxis::Vertical,
                follow_end: false,
                align: ScrollAlign::Start,
                offset: Default::default(),
            },
            NodeProps {
                x: 56.,
                y: 88.,
                width: Some(width - 112.),
                height: Some(height - 188.),
                clip: true,
                ..Default::default()
            },
            vec![text],
        )?;
        let mut nodes = vec![heading, scroll, self.line([0., height - 68., width, 1.])?];
        let actions = if self.confirm_exit && self.error.is_none() {
            vec![
                (self.text("取消", "Cancel", "キャンセル"), Action::Back),
                (self.text("退出", "Exit", "終了"), Action::ConfirmExit),
            ]
        } else {
            vec![(self.text("确定", "OK", "確認"), Action::Back)]
        };
        let button_width = width / actions.len() as f64;
        for (index, (label, action)) in actions.into_iter().enumerate() {
            let text = self.label(label, [16., 18., button_width - 32., 34.], 22., false)?;
            let node = self.stage.node(text).unwrap();
            let mut props = node.props.clone();
            props.text_align = reel_ui::TextAlign::Center;
            props.color = self.palette().accent;
            self.stage.update(text, node.kind.clone(), props)?;
            let button = self.control(
                [index as f64 * button_width, height - 68., button_width, 68.],
                vec![text],
                action,
                true,
                false,
                true,
            )?;
            if index == 0 {
                self.pending_focus = Some(button);
            } else {
                nodes.push(self.line([index as f64 * button_width, height - 68., 1., 68.])?);
            }
            nodes.push(button);
        }
        let panel = self.stage.create(
            NodeKind::Stack,
            NodeProps {
                x,
                y,
                width: Some(width),
                height: Some(height),
                background: Some(self.palette().surface),
                corner_radius: 16.,
                clip: true,
                ..Default::default()
            },
            nodes,
        )?;
        Ok(self.stage.create(
            NodeKind::Stack,
            NodeProps {
                width: Some(960.),
                height: Some(544.),
                modal: true,
                background: Some([0, 0, 0, 144]),
                ..Default::default()
            },
            vec![panel],
        )?)
    }
}
