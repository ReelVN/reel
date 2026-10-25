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
        let launcher =
            Launcher::new(&library.0, Assets::directory(Default::default()), error).unwrap();
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
        assert!(
            self.session
                .ui_mut()
                .unwrap()
                .presented(&self.bridge, &frame)
        );
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
}

#[test]
fn library_scans_sorted_immediate_game_directories() {
    let library = Library::new();
    let b = library.game("B");
    let a = library.game("A");
    library.game("nested/ignored");
    std::fs::write(library.0.join("game.asb"), []).unwrap();
    let games = scan(&library.0).unwrap();
    assert_eq!(
        games,
        vec![
            Game {
                title: "A".into(),
                package: a
            },
            Game {
                title: "B".into(),
                package: b
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
        Some(Outcome::Launch(package))
    );
}

#[test]
fn controller_selects_and_launches_a_work() {
    let library = Library::new();
    let package = library.game("作品");
    let mut view = View::new(&library, None);
    view.present();
    view.navigation(NavigationAction::Down, ActionPhase::Pressed);
    view.navigation(NavigationAction::Confirm, ActionPhase::Pressed);
    view.navigation(NavigationAction::Confirm, ActionPhase::Released);
    assert_eq!(
        view.launcher().take_outcome(),
        Some(Outcome::Launch(package))
    );
}

#[test]
fn paging_retires_old_buttons_and_requires_new_frame_acceptance() {
    let library = Library::new();
    for index in 0..7 {
        library.game(&format!("game-{index}"));
    }
    let mut view = View::new(&library, None);
    view.present();
    view.touch([220., 465.]);
    assert_eq!(view.launcher().page, 1);
    assert_eq!(
        view.launcher()
            .stage
            .nodes()
            .filter(|node| matches!(node.kind, NodeKind::Button { .. }))
            .count(),
        5
    );
    view.touch([80., 120.]);
    assert_eq!(view.launcher().take_outcome(), None);
    view.present();
    view.touch([80., 120.]);
    assert_eq!(
        view.launcher().take_outcome(),
        Some(Outcome::Launch(library.0.join("game-6/game.asb")))
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
    view.touch([650., 465.]);
    assert_eq!(view.launcher().games[0].package, package);
    view.present();
    view.touch([820., 465.]);
    assert_eq!(view.launcher().take_outcome(), Some(Outcome::Exit));
    view.session.close().unwrap();
    assert!(view.session.close_complete());
}
