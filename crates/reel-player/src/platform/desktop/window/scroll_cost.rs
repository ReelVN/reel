//! Opt-in measurement through the production native window, VM deadline and GPU path.
use super::*;
use std::time::Duration;
#[cfg(target_os = "windows")]
use winit::platform::windows::EventLoopBuilderExtWindows;
#[cfg(target_os = "linux")]
use winit::platform::x11::EventLoopBuilderExtX11;
use winit::{application::ApplicationHandler, event_loop::EventLoop};

#[test]
#[ignore = "requires REEL_SCROLL_ASB, REEL_SCROLL_ASSETS, REEL_SESSION_FONT and a native GPU/window"]
fn external_gallery_native_frame_cost() {
    struct Harness;
    impl ApplicationHandler for Harness {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            exercise(event_loop);
            event_loop.exit();
        }
        fn window_event(
            &mut self,
            _: &ActiveEventLoop,
            _: winit::window::WindowId,
            _: winit::event::WindowEvent,
        ) {
        }
    }
    #[cfg(target_os = "windows")]
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    #[cfg(target_os = "linux")]
    let event_loop = EventLoop::builder()
        .with_x11()
        .with_any_thread(true)
        .build()
        .unwrap();
    event_loop.run_app(&mut Harness).unwrap();
}

fn exercise(event_loop: &ActiveEventLoop) {
    use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};
    let bytes = std::fs::read(std::env::var_os("REEL_SCROLL_ASB").unwrap()).unwrap();
    let compiled = crate::distribution::decode_story(&bytes).unwrap();
    let assets = crate::assets::AssetOptions {
        root: std::env::var_os("REEL_SCROLL_ASSETS").unwrap().into(),
        ..Default::default()
    };
    let driver = AsideSessionUi::from_compiled(
        Session::new(SessionId(1), Epoch(1)).unwrap(),
        compiled,
        "en",
        VmLimits::default(),
    )
    .unwrap()
    .with_asset_options(assets.clone());
    let options = PlayerOptions {
        assets,
        logical_size: Some([1280, 720]),
        save_directory: Some(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/reel-artifacts/native-scroll-cost-saves"),
        ),
        ..Default::default()
    };
    let mut state = State::new_story(event_loop, 1, driver, options).unwrap();
    let start = Instant::now();
    let mut warm = 0;
    while warm < 5 {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "native gallery warmup timed out"
        );
        state.tick_story().unwrap();
        state.player.presentation.exposed();
        state.render().unwrap();
        if state.player.presentation.bridge.accepted_key().is_some()
            && !state.player.session.as_ref().unwrap().ui().images_pending()
            && !state.player.session.as_ref().unwrap().needs_tick()
        {
            warm += 1;
        } else {
            warm = 0;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    if std::env::var("REEL_SCROLL_VSYNC").is_ok_and(|s| s == "0") {
        state.player.backend.set_vsync(false).unwrap();
    }
    let interval = Duration::from_secs_f64(
        std::env::var("REEL_SCROLL_FRAME_MS")
            .ok()
            .map(|v| v.parse::<f64>().unwrap())
            .unwrap_or(1000. / 60.)
            / 1000.,
    );
    let began = Instant::now();
    let mut next_input = began;
    let mut sent = 0;
    let warm_window = std::env::var_os("REEL_SCROLL_WARM").is_some();
    let mut last_accepted = began;
    let mut accepted = 0;
    let mut current = 0;
    let mut times = Vec::new();
    let mut max_tick = Duration::ZERO;
    let mut max_render = Duration::ZERO;
    let mut max_gap = Duration::ZERO;
    let mut last_motion = began;
    let mut position = accepted_position(&state);
    let mut max_motion_gap = Duration::ZERO;
    let mut max_displacement = 0f64;
    let mut motion_frames = 0;
    let mut motion_stalls = 0;
    let mut rejected = std::collections::BTreeMap::<&str, usize>::new();
    state.player.pointer = reel_ui::dpi::PhysicalPosition::new(
        100. * state.window.scale_factor(),
        100. * state.window.scale_factor(),
    );
    while sent < 180 || began.elapsed() < interval * 180 + Duration::from_millis(500) {
        if sent < 180 && Instant::now() >= next_input {
            state
                .pointer_event(PointerKind::Scroll(ScrollDelta::PixelDelta(
                    reel_ui::dpi::PhysicalPosition::new(
                        0.,
                        if warm_window {
                            if sent < 90 { 0.5 } else { -0.5 }
                        } else if sent < 90 {
                            40.25
                        } else {
                            -37.25
                        },
                    ),
                )))
                .unwrap();
            sent += 1;
            next_input = began + interval * sent;
        }
        let tick = Instant::now();
        state.tick_story().unwrap();
        let tick_time = tick.elapsed();
        max_tick = max_tick.max(tick_time);
        let old = state
            .player
            .presentation
            .bridge
            .accepted_key()
            .map(|k| k.frame_id());
        let draw = Instant::now();
        state.render().unwrap();
        let render_time = draw.elapsed();
        max_render = max_render.max(render_time);
        if state
            .player
            .presentation
            .bridge
            .accepted_key()
            .map(|k| k.frame_id())
            != old
        {
            let now = Instant::now();
            let gap = now - last_accepted;
            last_accepted = now;
            max_gap = max_gap.max(gap);
            accepted += 1;
            current += usize::from(
                state
                    .player
                    .recorded_projection
                    .as_ref()
                    .is_some_and(|p| p.current),
            );
            let next_position = accepted_position(&state);
            if (next_position - position).abs() > 1e-6 {
                let motion_gap = now - last_motion;
                last_motion = now;
                max_motion_gap = max_motion_gap.max(motion_gap);
                max_displacement = max_displacement.max((next_position - position).abs());
                motion_stalls += usize::from(motion_gap > Duration::from_millis(34));
                motion_frames += 1;
                position = next_position;
            }
            if gap > Duration::from_millis(12)
                || tick_time > Duration::from_millis(3)
                || render_time > Duration::from_millis(8)
            {
                times.push((
                    sent,
                    gap,
                    tick_time,
                    render_time,
                    state
                        .player
                        .recorded_projection
                        .as_ref()
                        .is_some_and(|p| p.current),
                    state.player.session.as_ref().unwrap().ui().images_pending(),
                ));
            }
        } else if std::env::var_os("REEL_SCROLL_DIAG").is_some() {
            let PlayerSession::Aside { story, .. } = state.player.session.as_ref().unwrap() else {
                unreachable!()
            };
            let d = story.driver();
            let reason = if !d.adapter().stage_ready() {
                if !d.adapter().virtual_feedback_boundary() {
                    "active_or_ordinary_derive"
                } else if !d.adapter().virtual_feedback_ready() {
                    "unsynced_properties"
                } else if !d.ui().permits_virtual_feedback(d.adapter().stage()) {
                    "feedback_identity"
                } else if !d
                    .ui()
                    .virtual_feedback_visible(
                        &mut state.player.presentation.bridge,
                        state.player.root_id,
                    )
                    .unwrap()
                {
                    "viewport_coverage"
                } else if d.ui().images_block_presentation() {
                    "images_block"
                } else {
                    "other"
                }
            } else if d.ui().images_block_presentation() {
                "images_block"
            } else {
                "unchanged_or_other"
            };
            *rejected.entry(reason).or_default() += 1;
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    for (input, gap, tick, render, current, images) in times {
        eprintln!(
            "native input={input} gap={gap:?} tick_sync={tick:?} render={render:?} current={current} images={images}"
        );
    }
    eprintln!("native rejected={rejected:?}");
    eprintln!(
        "native summary warm_window={warm_window} inputs={sent} accepted={accepted} current={current} motion_frames={motion_frames} motion_stalls_over_34ms={motion_stalls} max_motion_gap={max_motion_gap:?} max_displacement={max_displacement:.4} duration={:?} max_gap={max_gap:?} max_tick_sync={max_tick:?} max_render={max_render:?}",
        began.elapsed()
    );
    state.close().unwrap();
    let end = Instant::now();
    while !state.close_complete() {
        state.tick_story().unwrap();
        assert!(end.elapsed() < Duration::from_secs(15));
    }
}

fn accepted_position(state: &State) -> f64 {
    let PlayerSession::Aside { story, .. } = state.player.session.as_ref().unwrap() else {
        unreachable!()
    };
    let list = story
        .driver()
        .adapter()
        .stage()
        .nodes()
        .find(|n| matches!(n.kind, reel_ui::stage::NodeKind::VirtualList(_)))
        .unwrap();
    let widget = crate::session::SessionUi::stage_widget(
        &state.player.presentation.bridge,
        state.player.root_id,
    )
    .unwrap();
    let stage = state
        .player
        .presentation
        .bridge
        .root()
        .get_widget(widget)
        .unwrap()
        .downcast::<crate::stage_widget::StageWidget>()
        .unwrap();
    let geometry = state
        .player
        .presentation
        .bridge
        .accepted_geometry(state.player.presentation.bridge.accepted_key().unwrap())
        .unwrap();
    let list = geometry.get(stage.node_widget(list.id).unwrap()).unwrap();
    list.window_transform.translation().y
}
