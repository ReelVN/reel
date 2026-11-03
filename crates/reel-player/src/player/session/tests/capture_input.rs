use super::*;
use crate::saves::{DirectorySaves, RuntimeSaves, SaveService, SaveStorage};
use reel_core::{
    budget::QueueLimits,
    identity::{FrameId, SurfaceEpoch},
    wake::WakeSignal,
};
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{BridgeLimits, NoticeResult, PresentationNotice, PresentationOutcome},
    core::{Ime, Widget, keyboard},
    dpi::PhysicalSize,
    input::{Control, GamepadAxis, InputDeviceId, InputEvent},
    stage::NodeKind,
};
use std::sync::Arc;

fn axis(axis: GamepadAxis, value: f32) -> TextEvent {
    TextEvent::Input(InputEvent::Value {
        device: InputDeviceId::Gamepad(1),
        control: Control::Axis(axis),
        value,
    })
}

#[test]
fn capture_routes_noop_input_and_preserves_authored_action_thresholds() {
    for (name, held, change) in [
        ("noise", false, None),
        ("held", true, None),
        ("mapped", false, Some(axis(GamepadAxis::LeftX, 0.8))),
        ("sensitive", false, Some(axis(GamepadAxis::LeftY, 0.001))),
        (
            "editing",
            false,
            Some(TextEvent::Ime(Ime::Commit("edited".into()))),
        ),
    ] {
        let driver = AsideSessionUi::from_source(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            r#"
            let state = model({events: 0, result: "waiting", name: ""});
            fn main() {
                stage.replace(input(width: 400, height: 200, actions: {
                    horizontal: {bindings: [binding("LeftX", dead_zone: 0.2)],
                        on_action: |event| { state.events += 1; }},
                    precise: {bindings: [binding("LeftY", dead_zone: 0)],
                        on_action: |event| { state.events += 1; }}
                }) {
                    text_input(value: bind(state.name), width: 160, height: 40,
                        on_change: |event| { state.events += 1; });
                    text(derive(|| f"{state.result}:{state.events}:{state.name}"));
                });
                await run(delay(1));
                try {
                    await saves.save("capture");
                    state.result = "saved";
                } catch error {
                    state.result = f"{error}";
                }
            }
            "#,
            "capture_input",
            1,
            "main",
            "en",
            VmLimits::default(),
        )
        .unwrap();
        let widget = driver.widget().prepare();
        let root = widget.id();
        let tree = RenderRoot::new(
            widget,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(Default::default()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(400, 200),
                scale_factor: 1.,
                default_font: Some(reel_ui::peniko::Blob::new(Arc::new(
                    include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/fixtures/font-weight/400.ttf"
                    ))
                    .to_vec(),
                ))),
            },
        );
        let mut bridge = PresentationBridge::new(
            tree,
            SurfaceEpoch(1),
            BridgeLimits {
                geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
                max_candidates: 1,
                max_bridge_bytes: 1 << 20,
                max_snapshot_bytes: 8 << 20,
            },
        )
        .unwrap();
        let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/save-tests")
            .join(format!("input-{name}-{}", std::process::id()));
        let storage = Arc::new(DirectorySaves::new(&folder, 16 << 20));
        let wake = WakeSignal::new(|| {});
        let service = SaveService::new(
            storage.clone(),
            QueueLimits {
                max_requests: 1,
                max_bytes: 16 << 20,
            },
            wake.clone(),
        );
        let runtime = StoryRuntime::new(driver)
            .with_saves(RuntimeSaves::new(service, 16 << 20))
            .with_media_factory(move || {
                Box::new(crate::media::MediaPlayer::new(
                    crate::assets::Assets::directory(Default::default()),
                    Default::default(),
                    wake.clone(),
                    |_| Err("test has no sound output".into()),
                ))
            });
        let mut window = PlayerSession::Aside {
            story: Box::new(runtime),
            hover: None,
        };
        // Accept the input scope through CPU geometry; no pixels are captured.
        for _ in 0..1000 {
            window
                .tick(
                    SliceBudget::default(),
                    crate::story_runtime::FrameProduction::Ready,
                    || 0,
                )
                .unwrap();
            window.sync(&mut bridge, root).unwrap();
            if !window.needs_tick() {
                break;
            }
        }
        let (recording, receipt) = window
            .record(&mut bridge, root, FrameId(1))
            .unwrap()
            .unwrap();
        assert_eq!(
            bridge.resolve(PresentationNotice::new(
                recording.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(FrameId(1)),
            )),
            NoticeResult::Accepted
        );
        window
            .story_mut()
            .unwrap()
            .driver_mut()
            .unwrap()
            .presented(&bridge, &receipt)
            .unwrap();
        if held {
            window
                .handle_text(&mut bridge, axis(GamepadAxis::LeftX, 0.6))
                .unwrap();
            window.after_input(&mut bridge, root).unwrap();
        }
        if name == "editing" {
            for _ in 0..2 {
                window
                    .handle_text(
                        &mut bridge,
                        TextEvent::Keyboard(keyboard::KeyboardEvent {
                            state: keyboard::KeyState::Down,
                            key: keyboard::Key::Named(keyboard::NamedKey::Tab),
                            code: keyboard::Code::Tab,
                            location: keyboard::Location::Standard,
                            modifiers: Default::default(),
                            repeat: false,
                            is_composing: false,
                        }),
                    )
                    .unwrap();
                window.after_input(&mut bridge, root).unwrap();
            }
            assert!(
                bridge
                    .root()
                    .get_widget(bridge.root().focused_widget().unwrap())
                    .unwrap()
                    .downcast::<reel_ui::widgets::TextInput>()
                    .is_some()
            );
        }
        for _ in 0..1000 {
            window
                .tick(
                    SliceBudget::default(),
                    crate::story_runtime::FrameProduction::Ready,
                    || 1_000_000_000,
                )
                .unwrap();
            window.sync(&mut bridge, root).unwrap();
            if window.story_mut().unwrap().capturing() {
                break;
            }
        }
        assert!(window.story_mut().unwrap().capturing(), "{name}");
        let stamp = window.ui().projection_stamp();
        for value in [-0.01, 0.003, 0., 0.19] {
            assert_eq!(
                window
                    .handle_text(&mut bridge, axis(GamepadAxis::RightX, value))
                    .unwrap(),
                Handled::No
            );
            window.after_input(&mut bridge, root).unwrap();
            assert!(window.story_mut().unwrap().capturing(), "unbound {name}");
            assert_eq!(
                window
                    .handle_text(
                        &mut bridge,
                        axis(GamepadAxis::LeftX, if held { 0.6 } else { value })
                    )
                    .unwrap(),
                Handled::Yes
            );
            window.after_input(&mut bridge, root).unwrap();
            assert!(
                window.story_mut().unwrap().capturing(),
                "mapped unchanged {name}"
            );
        }
        for event in [
            TextEvent::Ime(Ime::Enabled),
            TextEvent::Keyboard(keyboard::KeyboardEvent {
                state: keyboard::KeyState::Down,
                key: keyboard::Key::Named(keyboard::NamedKey::F1),
                code: keyboard::Code::F1,
                location: keyboard::Location::Standard,
                modifiers: Default::default(),
                repeat: false,
                is_composing: false,
            }),
        ] {
            window.handle_text(&mut bridge, event).unwrap();
            window.after_input(&mut bridge, root).unwrap();
            assert!(
                window.story_mut().unwrap().capturing(),
                "unused text {name}"
            );
        }
        assert_eq!(window.ui().projection_stamp(), stamp);
        if let Some(event) = &change {
            assert_eq!(
                window.handle_text(&mut bridge, event.clone()).unwrap(),
                Handled::Yes,
                "{name}"
            );
            window.after_input(&mut bridge, root).unwrap();
            assert!(
                !window.story_mut().unwrap().capturing(),
                "real action {name}"
            );
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while window.needs_tick() {
            assert!(
                std::time::Instant::now() < deadline,
                "save did not settle: {name}"
            );
            window
                .tick(
                    SliceBudget::default(),
                    crate::story_runtime::FrameProduction::Ready,
                    || 1_000_000_000,
                )
                .unwrap();
            window.sync(&mut bridge, root).unwrap();
            std::thread::yield_now();
        }
        let caption = window
            .story_mut()
            .unwrap()
            .driver()
            .adapter()
            .stage()
            .nodes()
            .find_map(|node| match &node.kind {
                NodeKind::Text(value) => Some(value.to_string()),
                _ => None,
            })
            .unwrap();
        if change.is_some() {
            assert!(
                caption.contains("CaptureBusy: input changed while capturing"),
                "{caption}"
            );
            let suffix = if name == "editing" {
                ":1:edited"
            } else {
                ":1:"
            };
            assert!(
                caption.ends_with(suffix),
                "the real action must execute once: {caption}"
            );
            assert!(storage.list().unwrap().is_empty());
        } else {
            assert_eq!(caption, format!("saved:{}:", usize::from(held)));
            assert_eq!(storage.list().unwrap().len(), 1);
            storage.delete("capture").unwrap();
        }
        window.close().unwrap();
        if folder.exists() {
            std::fs::remove_dir(folder).unwrap();
        }
    }
}
