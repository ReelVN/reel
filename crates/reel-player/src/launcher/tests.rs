use super::*;
use crate::player::session::PlayerSession;
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{BridgeLimits, NoticeResult, PresentationNotice, PresentationOutcome},
    core::{
        DefaultProperties, PointerButtonEvent, PointerId, PointerInfo, PointerState, PointerType,
        Widget,
    },
    dpi::PhysicalSize,
    input::NavigationEvent,
};

struct Library(PathBuf);
impl Library {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/launcher-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn game(&self, name: &str) -> PathBuf {
        let root = self.0.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let package = root.join("game.asb");
        std::fs::write(&package, []).unwrap();
        package
    }
}
impl Drop for Library {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

struct View {
    session: PlayerSession,
    bridge: PresentationBridge,
    root: WidgetId,
    frame: u64,
}
impl View {
    fn new(library: &Library, error: Option<String>) -> Self {
        let launcher = Launcher::new(
            &library.0,
            Assets::directory(Default::default()),
            error,
            WakeSignal::new(|| {}),
        )
        .unwrap();
        let mut launcher = launcher;
        settle_io(&mut launcher);
        let session = PlayerSession::Launcher(Box::new(launcher));
        let widget = session.ui().widget().prepare();
        let root = widget.id();
        let render = RenderRoot::new(
            widget,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(960, 544),
                scale_factor: 1.,
                default_font: Some(reel_ui::peniko::Blob::from(
                    include_bytes!("../../tests/fixtures/font-weight/400.ttf").to_vec(),
                )),
            },
        );
        let bridge = PresentationBridge::new(
            render,
            SurfaceEpoch(1),
            BridgeLimits {
                geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
                max_candidates: 4,
                max_bridge_bytes: 64 << 10,
                max_snapshot_bytes: 8 << 20,
            },
        )
        .unwrap();
        Self {
            session,
            bridge,
            root,
            frame: 0,
        }
    }
    fn launcher(&mut self) -> &mut Launcher {
        let PlayerSession::Launcher(launcher) = &mut self.session else {
            unreachable!()
        };
        launcher
    }
    fn present(&mut self) {
        self.session.sync(&mut self.bridge, self.root).unwrap();
        self.frame += 1;
        let (source, frame) = self
            .session
            .record(&mut self.bridge, self.root, FrameId(self.frame))
            .unwrap()
            .unwrap();
        assert_eq!(
            self.bridge.resolve(PresentationNotice::new(
                source.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(FrameId(self.frame))
            )),
            NoticeResult::Accepted
        );
        let PlayerSession::Launcher(launcher) = &mut self.session else {
            unreachable!()
        };
        launcher.presented(&self.bridge, &frame);
    }
    fn touch(&mut self, position: [f64; 2]) {
        for down in [true, false] {
            let event = PointerButtonEvent {
                pointer: PointerInfo {
                    pointer_id: PointerId::new(2),
                    persistent_device_id: None,
                    pointer_type: PointerType::Touch,
                },
                state: PointerState {
                    position: (position[0], position[1]).into(),
                    ..Default::default()
                },
                button: None,
            };
            self.session
                .handle_pointer(
                    &mut self.bridge,
                    if down {
                        PointerEvent::Down(event)
                    } else {
                        PointerEvent::Up(event)
                    },
                )
                .unwrap();
            self.session
                .after_input(&mut self.bridge, self.root)
                .unwrap();
        }
    }
    fn navigation(&mut self, action: NavigationAction, phase: ActionPhase) {
        self.session
            .handle_text(
                &mut self.bridge,
                TextEvent::Navigation(NavigationEvent {
                    action,
                    phase,
                    value: if phase == ActionPhase::Released {
                        0.
                    } else {
                        1.
                    },
                }),
            )
            .unwrap();
        self.session
            .after_input(&mut self.bridge, self.root)
            .unwrap();
    }
    fn button(&mut self, button: GamepadButton) {
        for value in [1., 0.] {
            self.session
                .handle_text(
                    &mut self.bridge,
                    TextEvent::Input(InputEvent::Value {
                        device: reel_ui::input::InputDeviceId::Gamepad(1),
                        control: Control::Button(button),
                        value,
                    }),
                )
                .unwrap();
            self.session
                .after_input(&mut self.bridge, self.root)
                .unwrap();
        }
    }
}

#[test]
fn library_scans_sorted_immediate_game_directories() {
    let library = Library::new();
    let b = library.game("B");
    let a = library.game("A");
    library.game("nested/ignored");
    std::fs::write(library.0.join("game.asb"), []).unwrap();
    let games = catalog::scan(&library.0, &std::sync::atomic::AtomicBool::new(false)).unwrap();
    assert_eq!(
        games,
        vec![
            Game {
                title: "A".into(),
                package: a,
                bytes: 0
            },
            Game {
                title: "B".into(),
                package: b,
                bytes: 0
            }
        ]
    );
}

#[test]
fn touch_launch_requires_an_accepted_frame() {
    let library = Library::new();
    let package = library.game("作品");
    let mut view = View::new(&library, None);
    view.touch([80., 120.]);
    assert_eq!(view.launcher().take_outcome(), None);
    view.present();
    view.touch([80., 120.]);
    assert_eq!(
        view.launcher().take_outcome(),
        None,
        "the loading frame must be accepted first"
    );
    view.present();
    settle_io(view.launcher());
    assert_eq!(
        view.launcher().take_outcome(),
        Some(Outcome::Launch(package))
    );
}

#[test]
fn controller_selects_and_launches_a_work() {
    let library = Library::new();
    library.game("A");
    let package = library.game("B");
    let mut view = View::new(&library, None);
    view.present();
    view.navigation(NavigationAction::Down, ActionPhase::Pressed);
    view.present();
    view.navigation(NavigationAction::Confirm, ActionPhase::Pressed);
    view.navigation(NavigationAction::Confirm, ActionPhase::Released);
    view.present();
    settle_io(view.launcher());
    assert_eq!(
        view.launcher().take_outcome(),
        Some(Outcome::Launch(package))
    );
}

#[test]
fn paging_mounts_bounded_rows_and_requires_new_frame_acceptance() {
    let library = Library::new();
    for index in 0..100 {
        library.game(&format!("game-{index:03}"));
    }
    let mut view = View::new(&library, None);
    view.present();
    view.button(GamepadButton::RightShoulder);
    assert_eq!(view.launcher().selected, 4);
    assert!(view.launcher().list.as_ref().unwrap().rows.len() <= 7);
    view.navigation(NavigationAction::Confirm, ActionPhase::Pressed);
    view.navigation(NavigationAction::Confirm, ActionPhase::Released);
    assert_eq!(view.launcher().take_outcome(), None);
    view.present();
    view.navigation(NavigationAction::Confirm, ActionPhase::Pressed);
    view.navigation(NavigationAction::Confirm, ActionPhase::Released);
    view.present();
    settle_io(view.launcher());
    assert_eq!(
        view.launcher().take_outcome(),
        Some(Outcome::Launch(library.0.join("game-004/game.asb")))
    );
}

#[test]
fn refresh_discovers_copied_games_and_error_back_returns_to_library() {
    let library = Library::new();
    let mut view = View::new(&library, Some("package failure".into()));
    view.present();
    view.navigation(NavigationAction::Back, ActionPhase::Released);
    assert!(view.launcher().error.is_none());
    assert!(view.launcher().games.is_empty());
    let package = library.game("new");
    view.present();
    view.button(GamepadButton::Start);
    settle_io(view.launcher());
    assert_eq!(view.launcher().games[0].package, package);
    view.present();
    view.button(GamepadButton::Select);
    assert_eq!(view.launcher().screen, Screen::Settings);
    view.present();
    view.touch([710., 390.]);
    assert!(view.launcher().confirm_exit);
    assert_eq!(view.launcher().take_outcome(), None);
    view.present();
    view.touch([600., 380.]);
    assert_eq!(view.launcher().take_outcome(), Some(Outcome::Exit));
    view.session.close().unwrap();
    assert!(view.session.close_complete());
}

#[test]
fn reference_frame_keeps_large_rows_between_shared_header_and_footer() {
    let library = Library::new();
    library.game("A");
    let mut view = View::new(&library, None);
    view.present();
    let launcher = view.launcher();
    let list = launcher.list.as_ref().unwrap();
    let viewport = &launcher.stage.node(list.scroll).unwrap().props;
    assert_eq!(
        (viewport.x, viewport.y, viewport.width, viewport.height),
        (64., 100., Some(832.), Some(355.))
    );
    let row = launcher.stage.node(list.rows[&0]).unwrap();
    assert_eq!(row.props.height, Some(90.));
    assert!(row.children.iter().any(|node| {
        let node = launcher.stage.node(*node).unwrap();
        matches!(node.kind, NodeKind::Text(_)) && node.props.font_size == 28.
    }));
    let root = launcher.stage.node(launcher.stage.root().unwrap()).unwrap();
    assert_eq!(root.props.background, Some([31, 35, 43, 255]));
    let separators = launcher
        .stage
        .nodes()
        .filter(|node| node.props.width == Some(864.) && node.props.height == Some(1.))
        .map(|node| node.props.y)
        .collect::<Vec<_>>();
    assert_eq!(separators, vec![87., 471., 87., 471.]);
}

#[test]
fn dropdowns_persist_readable_preferences_and_restore_library_selection() {
    let library = Library::new();
    library.game("A");
    let package = library.game("B");
    let mut view = View::new(&library, None);
    view.present();
    view.navigation(NavigationAction::Down, ActionPhase::Pressed);
    view.present();
    assert_eq!(view.launcher().selected, 1);
    view.button(GamepadButton::Select);
    view.present();
    view.touch([650., 242.]);
    assert_eq!(view.launcher().picker, Some(Picker::Theme));
    view.present();
    view.navigation(NavigationAction::Down, ActionPhase::Pressed);
    view.present();
    view.navigation(NavigationAction::Confirm, ActionPhase::Pressed);
    view.navigation(NavigationAction::Confirm, ActionPhase::Released);
    assert!(view.launcher().preferences.light);
    view.present();
    view.touch([650., 150.]);
    assert_eq!(view.launcher().picker, Some(Picker::Language));
    view.present();
    view.navigation(NavigationAction::Down, ActionPhase::Pressed);
    view.present();
    view.navigation(NavigationAction::Confirm, ActionPhase::Pressed);
    view.navigation(NavigationAction::Confirm, ActionPhase::Released);
    assert_eq!(view.launcher().preferences.language, Language::English);
    view.present();
    view.navigation(NavigationAction::Back, ActionPhase::Released);
    view.present();
    assert_eq!(view.launcher().selected, 1);
    view.navigation(NavigationAction::Confirm, ActionPhase::Pressed);
    view.navigation(NavigationAction::Confirm, ActionPhase::Released);
    assert_eq!(view.launcher().take_outcome(), None);
    view.present();
    settle_io(view.launcher());
    assert_eq!(
        view.launcher().take_outcome(),
        Some(Outcome::Launch(package))
    );
    let saved = std::fs::read_to_string(library.0.join("launcher.tsv")).unwrap();
    assert!(saved.contains("theme\tlight\n") && saved.contains("language\ten\n"));
    assert!(!library.0.join(".reel-store-launcher.preferences").exists());
    let mut restored = View::new(&library, None);
    restored.present();
    assert!(restored.launcher().preferences.light);
    assert_eq!(restored.launcher().preferences.language, Language::English);
    assert_eq!(restored.launcher().selected, 1);
}

#[test]
fn page_switches_reuse_nodes_and_popup_dismissal_keeps_settings() {
    let library = Library::new();
    library.game("A");
    let mut view = View::new(&library, None);
    view.present();
    let pages = view.launcher().pages.clone();
    let count = view.launcher().stage.nodes().count();
    let started = std::time::Instant::now();
    for _ in 0..20 {
        view.button(GamepadButton::Select);
        view.present();
        view.navigation(NavigationAction::Back, ActionPhase::Released);
        view.present();
    }
    eprintln!("40 page switches: {:?}", started.elapsed());
    assert_eq!(view.launcher().pages, pages);
    assert_eq!(view.launcher().stage.nodes().count(), count);
    view.button(GamepadButton::Select);
    view.present();
    view.touch([650., 150.]);
    view.present();
    view.touch([100., 110.]);
    assert_eq!(view.launcher().picker, None);
    assert_eq!(view.launcher().screen, Screen::Settings);
}

#[test]
fn error_modal_blocks_underlying_touch_and_library_shortcuts() {
    let library = Library::new();
    library.game("A");
    let mut view = View::new(&library, Some("failure".into()));
    view.present();
    view.touch([80., 120.]);
    view.button(GamepadButton::Select);
    view.button(GamepadButton::Start);
    view.button(GamepadButton::North);
    assert_eq!(view.launcher().take_outcome(), None);
    assert_eq!(view.launcher().screen, Screen::Library);
    assert_eq!(view.launcher().error.as_deref(), Some("failure"));
    view.navigation(NavigationAction::Back, ActionPhase::Released);
    assert!(!view.launcher().modal());
}

fn settle_io(launcher: &mut Launcher) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !launcher.worker.is_idle() {
        launcher.poll().unwrap();
        assert!(
            std::time::Instant::now() < deadline,
            "launcher IO did not settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    launcher.poll().unwrap();
}
