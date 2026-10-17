use crate::aside_session::AsideSessionUi;
use crate::platform::PlatformHost;
use crate::window::{PointerKind, State};
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{VmLimits, source::CompiledStory},
};
use reel_render::RenderBackend;
use reel_render::gpu::GpuError;
use reel_ui::core::TextEvent;
use std::time::{Duration, Instant};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::WindowId,
};
type Error = Box<dyn std::error::Error>;
pub fn run() -> Result<(), Error> {
    run_mode(Mode::Diagnostic)
}
/// Open the real Rust host/session diagnostic through the existing native renderer.
pub fn run_session_host() -> Result<(), Error> {
    run_mode(Mode::SessionDiagnostic)
}

/// Native window options. An explicit logical_size overrides the first story canvas.
pub struct StoryWindowOptions {
    pub assets: crate::assets::AssetOptions,
    /// Some selects archive resolution; None retains ordinary directory assets.
    pub asset_packages: Option<Vec<std::path::PathBuf>>,
    /// Explicit loose overrides for an archive-backed work.
    pub asset_overlay: Option<std::path::PathBuf>,
    pub media: crate::media::MediaOptions,
    pub title: String,
    pub locale: String,
    pub logical_size: Option<[u32; 2]>,
    pub canvas_size: Option<[u32; 2]>,
    /// Host-selected user data, independent of read-only game assets.
    pub save_directory: Option<std::path::PathBuf>,
    pub save_bytes: u64,
}
impl Default for StoryWindowOptions {
    fn default() -> Self {
        Self {
            assets: Default::default(),
            asset_packages: None,
            asset_overlay: None,
            media: Default::default(),
            title: "Reel".into(),
            locale: "en".into(),
            logical_size: None,
            canvas_size: None,
            save_directory: None,
            save_bytes: 256 << 20,
        }
    }
}

/// Run a prepared story using the production window and presented-input path.
/// On close, VM task and root retirement finishes before the native window exits.
pub fn run_story(story: CompiledStory, options: StoryWindowOptions) -> Result<(), Error> {
    let session = Session::new(SessionId(1), Epoch(1))?;
    let assets = match &options.asset_packages {
        Some(paths) => crate::assets::Assets::packaged(options.assets.clone(), paths.clone(), options.asset_overlay.clone())
            .map_err(|error| format!("cannot open resource packages: {error:?}"))?,
        None => crate::assets::Assets::directory(options.assets.clone()),
    };
    let driver =
        AsideSessionUi::from_compiled(session, story, &options.locale, VmLimits::default())?
            .with_assets(assets);
    run_mode(Mode::Story(Box::new((driver, options))))
}

enum Mode {
    Diagnostic,
    SessionDiagnostic,
    Story(Box<(AsideSessionUi, StoryWindowOptions)>),
}

fn run_mode(mode: Mode) -> Result<(), Error> {
    let event_loop = EventLoop::new()?;
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    let mode = match mode {
        Mode::Story(story) => {
            let (driver, options) = *story;
            let proxy = event_loop.create_proxy();
            let wake = reel_core::wake::WakeSignal::new(move || {
                let _ = proxy.send_event(());
            });
            let assets = driver.ui().assets();
            let driver = driver.with_media(Box::new(crate::media::MediaPlayer::desktop(
                assets,
                options.media.clone(),
                wake,
            )))?;
            Mode::Story(Box::new((driver, options)))
        }
        mode => mode,
    };

    let mut app = App {
        state: None,
        error: None,
        next_generation: 1,
        mode: Some(mode),
        closing: false,
    };
    event_loop.run_app(&mut app)?;
    if let Some(error) = app.error {
        return Err(error.into());
    }
    Ok(())
}
struct App {
    state: Option<State>,
    error: Option<String>,
    next_generation: u64,
    mode: Option<Mode>,
    closing: bool,
}
impl App {
    fn recover_or_fail(&mut self, event_loop: &ActiveEventLoop, error: Error) {
        if matches!(
            error.downcast_ref::<GpuError>(),
            Some(GpuError::DeviceLost(_))
        ) {
            if let Some(state) = &mut self.state
                && let Err(error) = state.recover_device()
            {
                self.fail(event_loop, error);
            }
        } else {
            self.fail(event_loop, error);
        }
    }
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: impl std::fmt::Display) {
        self.error.get_or_insert_with(|| error.to_string());
        self.close(event_loop);
    }
    fn close(&mut self, event_loop: &ActiveEventLoop) {
        self.closing = true;
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(1),
        ));
        if let Some(state) = &mut self.state {
            if let Err(error) = state.close() {
                self.error.get_or_insert_with(|| error.to_string());
                self.state = None;
                event_loop.exit();
            }
        } else {
            event_loop.exit();
        }
    }
}
impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(state) = &mut self.state {
            if let Err(e) = state.resume() {
                self.fail(event_loop, e);
            }
            return;
        }
        let Some(mode) = self.mode.take() else {
            return;
        };
        let created = match mode {
            Mode::SessionDiagnostic => State::new_session(event_loop, self.next_generation),
            Mode::Diagnostic => State::new(event_loop, self.next_generation),
            Mode::Story(story) => {
                let (driver, options) = *story;
                State::new_story(event_loop, self.next_generation, driver, options)
            }
        };
        match created {
            Ok(mut state) => {
                state.host.request_redraw();
                self.state = Some(state);
            }
            Err(e) => self.fail(event_loop, e),
        }
    }
    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(state) = self.state.as_mut()
            && let Err(error) = state.suspend()
        {
            self.fail(event_loop, error);
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.closing {
            return;
        }
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.window.id() != id {
            return;
        }
        let result = match event {
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && state.is_diagnostic()
                    && event.physical_key
                        == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::F5) =>
            {
                state.backend.renderer().gpu().device.destroy();
                state.invalidate();
                Ok(())
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && state.is_diagnostic()
                    && event.physical_key
                        == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::F6) =>
            {
                state.suspend().and_then(|()| state.resume())
            }
            WindowEvent::CloseRequested => {
                self.close(event_loop);
                return;
            }
            WindowEvent::Resized(size) => state.resize(size),
            WindowEvent::Moved(_) => state.monitor_changed(),
            WindowEvent::ScaleFactorChanged { .. } => state.resize(state.window.inner_size()),
            WindowEvent::ModifiersChanged(modifiers) => {
                state.modifiers = modifiers.state();
                Ok(())
            }
            WindowEvent::KeyboardInput { event, .. } => state
                .text_event(TextEvent::Keyboard(
                    ui_events_winit::keyboard::from_winit_keyboard_event(event, state.modifiers),
                ))
                .map(|_| ()),
            WindowEvent::Ime(event) => state
                .text_event(crate::platform::winit::ime(event))
                .map(|_| ()),
            WindowEvent::Focused(focus) => {
                let result = state
                    .text_event(TextEvent::WindowFocusChange(focus))
                    .map(|_| ());
                if !focus {
                    state.modifiers = Default::default();
                    result.and_then(|()| state.pointer_event(PointerKind::Cancel))
                } else {
                    result
                }
            }
            WindowEvent::Touch(touch) => state.touch_event(touch),
            WindowEvent::CursorMoved { position, .. } => {
                state.pointer = reel_ui::dpi::PhysicalPosition::new(position.x, position.y);
                state.pointer_event(PointerKind::Move)
            }
            WindowEvent::MouseInput {
                state: pressed,
                button,
                ..
            } => {
                use reel_ui::core::PointerButton;
                let button = match button {
                    MouseButton::Left => PointerButton::Primary,
                    MouseButton::Right => PointerButton::Secondary,
                    MouseButton::Middle => PointerButton::Auxiliary,
                    MouseButton::Back => PointerButton::X1,
                    MouseButton::Forward => PointerButton::X2,
                    MouseButton::Other(_) => return,
                };
                state.pointer_event(if pressed == ElementState::Pressed {
                    PointerKind::Down(button)
                } else {
                    PointerKind::Up(button)
                })
            }
            WindowEvent::MouseWheel { delta, .. } if state.session.is_some() => {
                let delta = match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => {
                        reel_ui::core::ScrollDelta::LineDelta(-x, -y)
                    }
                    winit::event::MouseScrollDelta::PixelDelta(p) => {
                        reel_ui::core::ScrollDelta::PixelDelta(reel_ui::dpi::PhysicalPosition::new(
                            -p.x, -p.y,
                        ))
                    }
                };
                state.pointer_event(PointerKind::Scroll(delta))
            }
            WindowEvent::MouseWheel { delta, .. } if state.session.is_none() => {
                let delta = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(y) * 0.1,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y * 0.002,
                };
                state.zoom = (state.zoom + delta).clamp(0.5, 1.5);
                let zoom = state.zoom;
                state
                    .presentation
                    .bridge
                    .edit_widget(state.root_id, |mut widget| {
                        widget.set_transform(reel_ui::kurbo::Affine::scale(zoom))
                    });
                state.invalidate();
                Ok(())
            }
            WindowEvent::RedrawRequested => state.redraw_requested(),
            _ => Ok(()),
        };
        if let Err(e) = result {
            self.recover_or_fail(event_loop, e);
        }
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(state) = &mut self.state {
            if self.closing {
                if let Err(error) = state.tick_gamepads() {
                    self.fail(event_loop, error);
                    return;
                }
                if let Err(error) = state.tick_story() {
                    self.error.get_or_insert_with(|| error.to_string());
                    self.state = None;
                    event_loop.exit();
                } else if state.close_complete() {
                    state.host.request_exit();
                    state.host.apply(event_loop);
                    self.state = None;
                } else {
                    state
                        .host
                        .wait_until(Some(Instant::now() + Duration::from_millis(1)));
                    state.host.apply(event_loop);
                }
                return;
            }
            if state.is_suspended() {
                state.host.set_wake_deadline(None);
                state.host.apply(event_loop);
                return;
            }
            if let Err(error) = state.tick_gamepads() {
                self.fail(event_loop, error);
                return;
            }
            if let Err(error) = state.tick_story() {
                self.fail(event_loop, error);
                return;
            }
            if state.exit_requested() {
                self.close(event_loop);
                return;
            }
            if let Err(error) = state.prepare_initial_frame() {
                self.recover_or_fail(event_loop, error);
                return;
            }
            if let Err(e) = state.poll_completions() {
                self.recover_or_fail(event_loop, Box::new(e));
                return;
            }
            if state.presentation.needs_redraw() {
                state.host.request_redraw();
            }
            // Completion wake-up is bounded; no continuous redraw when idle.
            let completion_deadline = (state.presentation.needs_redraw()
                || state.backend.has_pending())
            .then(|| Instant::now() + Duration::from_millis(8));
            let deadline = match (completion_deadline, state.story_deadline()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let deadline = match (deadline, state.input_deadline()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            state.host.wait_until(deadline);
            state.host.apply(event_loop);
        }
    }
}
