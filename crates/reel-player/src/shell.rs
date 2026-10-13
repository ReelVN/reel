use crate::aside_session::AsideSessionUi;
use crate::window::{PointerKind, State};
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{VmLimits, source::CompiledStory},
};
use reel_render::gpu::GpuError;
use reel_ui::core::TextEvent;
use std::path::PathBuf;
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

/// Native window options; logical_size is the window size, not a fixed stage canvas.
pub struct StoryWindowOptions {
    pub title: String,
    pub font: PathBuf,
    pub locale: String,
    pub logical_size: [u32; 2],
}
impl StoryWindowOptions {
    pub fn new(font: impl Into<PathBuf>) -> Self {
        Self {
            title: "Reel".into(),
            font: font.into(),
            locale: "en".into(),
            logical_size: [960, 540],
        }
    }
}

/// Run a prepared story using the production window and presented-input path.
/// On close, VM task and root retirement finishes before the native window exits.
pub fn run_story(story: CompiledStory, options: StoryWindowOptions) -> Result<(), Error> {
    let session = Session::new(SessionId(1), Epoch(1))?;
    let driver =
        AsideSessionUi::from_compiled(session, story, &options.locale, VmLimits::default())?;
    run_mode(Mode::Story(Box::new((driver, options))))
}

enum Mode {
    Diagnostic,
    SessionDiagnostic,
    Story(Box<(AsideSessionUi, StoryWindowOptions)>),
}

fn run_mode(mode: Mode) -> Result<(), Error> {
    let event_loop = EventLoop::new()?;
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
            Ok(state) => {
                state.window.request_redraw();
                self.state = Some(state);
            }
            Err(e) => self.fail(event_loop, e),
        }
    }
    fn suspended(&mut self, _: &ActiveEventLoop) {
        if let Some(state) = self.state.as_mut() {
            state.suspend();
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
                    && event.physical_key
                        == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::Escape) =>
            {
                self.close(event_loop);
                return;
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && state.is_diagnostic()
                    && event.physical_key
                        == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::F5) =>
            {
                state.renderer.gpu().device.destroy();
                state.invalidate();
                Ok(())
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && state.is_diagnostic()
                    && event.physical_key
                        == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::F6) =>
            {
                state.suspend();
                state.resume()
            }
            WindowEvent::CloseRequested => {
                self.close(event_loop);
                return;
            }
            WindowEvent::Resized(size) => state.resize(size),
            WindowEvent::ScaleFactorChanged { .. } => state.resize(state.window.inner_size()),
            WindowEvent::Focused(focus) => {
                state
                    .bridge
                    .handle_text_event(TextEvent::WindowFocusChange(focus));
                if !focus {
                    state.pointer_event(PointerKind::Cancel)
                } else {
                    Ok(())
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                state.pointer = reel_ui::dpi::PhysicalPosition::new(position.x, position.y);
                state.pointer_event(PointerKind::Move)
            }
            WindowEvent::MouseInput {
                state: button,
                button: MouseButton::Left,
                ..
            } => state.pointer_event(if button == ElementState::Pressed {
                PointerKind::Down
            } else {
                PointerKind::Up
            }),
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
                state.bridge.edit_widget(state.root_id, |mut widget| {
                    widget.set_transform(reel_ui::kurbo::Affine::scale(zoom))
                });
                state.invalidate();
                Ok(())
            }
            WindowEvent::RedrawRequested => state.redraw_requested(),
            _ => Ok(()),
        };
        if let Err(e) = result {
            if matches!(e.downcast_ref::<GpuError>(), Some(GpuError::DeviceLost)) {
                if let Err(e) = self.state.as_mut().unwrap().recover_device() {
                    self.fail(event_loop, e);
                }
            } else {
                self.fail(event_loop, e);
            }
        }
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(state) = &mut self.state {
            if self.closing {
                if let Err(error) = state.tick_story() {
                    self.error.get_or_insert_with(|| error.to_string());
                    self.state = None;
                    event_loop.exit();
                } else if state.close_complete() {
                    self.state = None;
                    event_loop.exit();
                } else {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(
                        Instant::now() + Duration::from_millis(1),
                    ));
                }
                return;
            }
            if state.is_suspended() {
                event_loop.set_control_flow(ControlFlow::Wait);
                return;
            }
            if let Err(error) = state.tick_story() {
                self.fail(event_loop, error);
                return;
            }
            if let Err(e) = state.tick_reveal() {
                self.fail(event_loop, e);
                return;
            }
            if let Err(e) = state.poll_completions() {
                if matches!(e, GpuError::DeviceLost) {
                    if let Err(e) = state.recover_device() {
                        self.fail(event_loop, e);
                    }
                } else {
                    self.fail(event_loop, e);
                }
                return;
            }
            if state.demand.has_work() {
                state.window.request_redraw();
            }
            // Completion wake-up is bounded; no continuous redraw when idle.
            let completion_deadline = (state.demand.has_work()
                || state.renderer.usage().targets != 0)
                .then(|| Instant::now() + Duration::from_millis(8));
            let deadline = match (completion_deadline, state.reveal_deadline()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let deadline = match (deadline, state.story_deadline()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            event_loop.set_control_flow(
                deadline
                    .map(ControlFlow::WaitUntil)
                    .unwrap_or(ControlFlow::Wait),
            );
        }
    }
}
