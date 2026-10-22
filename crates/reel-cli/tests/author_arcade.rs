//! The bundled Arcade project loads its own font and reaches its game menu on a CPU host.
#[path = "support/author.rs"]
#[allow(dead_code)]
mod support;

use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_engine::{
    Epoch, InteractionView, Session, SessionId,
    aside::{SliceBudget, VmLimits},
};
use reel_player::{
    aside_session::{AsideSessionUi, StoryState},
    assets::{AssetKind, AssetOptions},
    story_runtime::StoryRuntime,
};
use reel_ui::{
    bridge::{NoticeResult, PresentationBridge, PresentationNotice, PresentationOutcome},
    core::{TextEvent, WidgetId, WindowEvent, keyboard},
    dpi::PhysicalSize,
    imaging::record::{Command, Draw},
    peniko::Blob,
    resources::ResourceData,
    stage::NodeKind,
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::Duration,
};

fn present(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    font: &Blob<u8>,
    frame: u64,
) -> usize {
    driver.sync(bridge, root).unwrap();
    let (source, receipt) = driver
        .record(bridge, root, FrameId(frame))
        .unwrap()
        .expect("the story must have a complete frame");
    let scene = source.scene();
    let mut glyphs = 0;
    for command in scene.commands() {
        if let Command::Draw(id) = command
            && let Draw::GlyphRun(run) = scene.draw_op(*id)
        {
            assert_eq!(run.font.data.id(), font.id());
            assert!(run.glyphs.iter().all(|glyph| glyph.id != 0));
            glyphs += run.glyphs.len();
        }
    }
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            source.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(frame)),
        )),
        NoticeResult::Accepted
    );
    driver.presented(bridge, &receipt).unwrap();
    glyphs
}

fn tick(runtime: &mut StoryRuntime, now: u64) {
    for _ in 0..10_000 {
        runtime.tick(SliceBudget::default(), || now).unwrap();
        if !runtime.driver().has_runnable_tasks() {
            return;
        }
    }
    panic!("story did not reach its interaction");
}

#[test]
fn source_and_asb_load_bundled_font_and_reach_arcade_menu() {
    let path = std::env::var_os("REEL_ARCADE_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/author_arcade")
        });
    let project =
        reel_cli::project::load(&path).unwrap_or_else(|error| panic!("{:?}", error.failure));
    let checked = project.check().unwrap();
    let artifact = reel_cli::build(&checked, Some(reel_cli::project::ENTRY)).unwrap();
    for (kind, story) in [
        (
            "source",
            reel_cli::into_story(checked, reel_cli::project::ENTRY).unwrap(),
        ),
        ("ASB", reel_cli::decode_story(&artifact.bytes).unwrap()),
    ] {
        let driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap()
        .with_asset_options(AssetOptions {
            root: project.assets(),
            ..Default::default()
        });
        let mut runtime = StoryRuntime::new(driver);
        tick(&mut runtime, 0);
        let driver = runtime.driver();
        assert!(matches!(
            driver.ui().session().view().interaction,
            Some(InteractionView::Dialogue(_))
        ));
        assert_eq!(
            driver.adapter().game_settings().fonts,
            ["fonts/AtkinsonHyperlegible-Regular.ttf"]
        );

        let assets = driver.ui().assets();
        let (send, receive) = mpsc::channel();
        assets.set_waker(Arc::new(move || {
            let _ = send.send(());
        }));
        let request = assets.request(
            driver.adapter().game_settings().fonts[0].clone(),
            AssetKind::Font,
        );
        let lease = loop {
            if let Some(lease) = assets
                .poll(request)
                .expect("load the project's configured font")
            {
                break lease;
            }
            receive
                .recv_timeout(Duration::from_secs(10))
                .expect("font worker wake");
        };
        let ResourceData::Font(font) = lease.data() else {
            panic!("configured font must decode as font data")
        };
        let (mut bridge, root) = support::bridge(driver);
        bridge.handle_window_event(WindowEvent::Resize(PhysicalSize::new(960, 640)));
        assert!(bridge.set_default_fonts(std::slice::from_ref(font)));
        // The first accepted frame opens the dialogue's reveal clock, as in the native shell.
        present(runtime.driver_mut().unwrap(), &mut bridge, root, font, 1);
        tick(&mut runtime, 16_666_667);
        assert!(present(runtime.driver_mut().unwrap(), &mut bridge, root, font, 2) > 0);

        for event in [
            TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Enter)),
            TextEvent::key_up(keyboard::Key::Named(keyboard::NamedKey::Enter)),
        ] {
            runtime
                .driver_mut()
                .unwrap()
                .handle_text(&mut bridge, event)
                .unwrap();
        }
        runtime.wake_input();
        tick(&mut runtime, 33_333_334);
        let driver = runtime.driver();
        assert_eq!(driver.state(), StoryState::Running);
        let labels = driver
            .adapter()
            .stage()
            .nodes()
            .filter_map(|node| match &node.kind {
                NodeKind::Button {
                    label: Some(label),
                    listening: true,
                } => Some(label.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(labels, ["Pong", "Bricks", "Leave"]);
        assert!(present(runtime.driver_mut().unwrap(), &mut bridge, root, font, 3) > 0);
        eprintln!(
            "{kind}: {} loaded its font, accepted dialogue advance, and presented the Pong/Bricks/Leave menu",
            path.display()
        );
    }
}
