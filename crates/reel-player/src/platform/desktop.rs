use crate::aside_session::AsideSessionUi;
use crate::hal::PlatformHost;
mod diagnostic;
#[cfg(feature = "performance")]
mod performance;
#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
mod tests;
mod window;
use crate::player::{PlayerOptions, PointerKind};
use reel_core::wake::WakeSignal;
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{GameSettings, VmLimits, source::CompiledStory},
};
use reel_render::RenderBackend;
use reel_render::gpu::GpuError;
use reel_ui::core::TextEvent;
use std::time::{Duration, Instant};
use window::State;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
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

/// Run a prepared story using the production window and presented-input path.
/// On close, VM task and root retirement finishes before the native window exits.
pub fn run_story(story: CompiledStory, mut options: PlayerOptions) -> Result<(), Error> {
    #[cfg(feature = "performance")]
    let mut capture = performance::Capture::from_environment(&mut options)?;
    #[cfg(not(feature = "performance"))]
    if std::env::var_os("REEL_PERFORMANCE_CONFIG").is_some() {
        return Err("REEL_PERFORMANCE_CONFIG requires reel-player/performance".into());
    }
    let prepared = (|| {
        let session = Session::new(SessionId(1), Epoch(1))?;
        let assets = options
            .open_assets()
            .map_err(|error| format!("cannot open story resources: {error:?}"))?;
        let mut driver =
            AsideSessionUi::from_compiled(session, story, &options.locale, VmLimits::default())?
                .with_asset_context(assets, options.catalogs.clone());
        driver
            .adapter_mut()
            .inherit_game_settings(std::sync::Arc::new(GameSettings {
                title: options.title.clone(),
                ..GameSettings::default()
            }));
        Ok::<_, Error>(Mode::Story(Box::new((driver, options))))
    })();
    let mode = match prepared {
        Ok(mode) => mode,
        Err(error) => {
            #[cfg(feature = "performance")]
            if let Some(capture) = &mut capture {
                capture.finish(&error.to_string())?;
            }
            return Err(error);
        }
    };
    run_mode_recorded(
        mode,
        #[cfg(feature = "performance")]
        capture,
    )
}

enum Mode {
    Diagnostic,
    SessionDiagnostic,
    Story(Box<(AsideSessionUi, PlayerOptions)>),
}

fn run_mode(mode: Mode) -> Result<(), Error> {
    run_mode_recorded(
        mode,
        #[cfg(feature = "performance")]
        None,
    )
}

fn run_mode_recorded(
    mode: Mode,
    #[cfg(feature = "performance")] mut capture: Option<performance::Capture>,
) -> Result<(), Error> {
    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            #[cfg(feature = "performance")]
            if let Some(capture) = &mut capture {
                capture.finish(&error.to_string())?;
            }
            return Err(error.into());
        }
    };
    let mut app = App {
        wake: event_wake(event_loop.create_proxy()),
        state: None,
        error: None,
        next_generation: 1,
        mode: Some(mode),
        closing: false,
        #[cfg(feature = "performance")]
        capture,
        #[cfg(feature = "performance")]
        capture_started: None,
        #[cfg(feature = "performance")]
        capture_wait: None,
    };
    let run = event_loop.run_app(&mut app);
    #[cfg(feature = "performance")]
    drop(app.capture_wait.take());
    #[cfg(feature = "performance")]
    if let Some(capture) = &mut app.capture {
        let outcome = app
            .error
            .as_deref()
            .map(str::to_owned)
            .or_else(|| run.as_ref().err().map(ToString::to_string))
            .unwrap_or_else(|| {
                if app.closing && app.state.is_none() {
                    "closed"
                } else {
                    "event loop exited before close completed"
                }
                .into()
            });
        capture.finish(&outcome)?;
    }
    run?;
    if let Some(error) = app.error {
        return Err(error.into());
    }
    Ok(())
}

fn event_wake(proxy: EventLoopProxy<()>) -> WakeSignal {
    WakeSignal::new(move || {
        let _ = proxy.send_event(());
    })
}

struct App {
    wake: WakeSignal,
    state: Option<State>,
    error: Option<String>,
    next_generation: u64,
    mode: Option<Mode>,
    closing: bool,
    #[cfg(feature = "performance")]
    capture: Option<performance::Capture>,
    #[cfg(feature = "performance")]
    capture_started: Option<Instant>,
    #[cfg(feature = "performance")]
    capture_wait: Option<reel_core::profile::Scope>,
}
impl App {
    #[cfg(feature = "performance")]
    fn start_performance(&mut self) {
        self.capture_started = self.capture.as_ref().map(|_| Instant::now());
    }
    #[cfg(feature = "performance")]
    fn record_performance(&mut self) {
        if let Some(state) = &self.state {
            performance::record(&mut self.capture, state, &mut self.capture_started);
        }
    }
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
            let closed = state.close();
            #[cfg(feature = "performance")]
            performance::record(&mut self.capture, state, &mut self.capture_started);
            if let Err(error) = closed {
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
    #[cfg(feature = "performance")]
    fn new_events(&mut self, _: &ActiveEventLoop, cause: winit::event::StartCause) {
        if let Some(capture) = &mut self.capture {
            let resumed_at = Instant::now();
            drop(self.capture_wait.take());
            capture.wake(resumed_at, cause);
        }
    }
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(feature = "performance")]
        self.start_performance();
        if let Some(state) = &mut self.state {
            if let Err(e) = state.resume() {
                self.fail(event_loop, e);
            }
            #[cfg(feature = "performance")]
            self.record_performance();
            return;
        }
        let Some(mode) = self.mode.take() else {
            return;
        };
        let created = match mode {
            Mode::SessionDiagnostic => {
                State::new_session(event_loop, self.wake.clone(), self.next_generation)
            }
            Mode::Diagnostic => State::new(event_loop, self.wake.clone(), self.next_generation),
            Mode::Story(story) => {
                let (driver, options) = *story;
                State::new_story(
                    event_loop,
                    self.wake.clone(),
                    self.next_generation,
                    driver,
                    options,
                )
            }
        };
        match created {
            Ok(mut state) => {
                #[cfg(feature = "performance")]
                if let Some(capture) = &self.capture {
                    capture.attach(&mut state);
                }
                state.host.request_redraw();
                self.state = Some(state);
            }
            Err(e) => self.fail(event_loop, e),
        }
        #[cfg(feature = "performance")]
        self.record_performance();
    }
    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(feature = "performance")]
        self.start_performance();
        if let Some(state) = self.state.as_mut()
            && let Err(error) = state.suspend()
        {
            self.fail(event_loop, error);
        }
        #[cfg(feature = "performance")]
        self.record_performance();
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::HostEvent);
        #[cfg(feature = "performance")]
        self.start_performance();
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
                state.player.backend.renderer().gpu().device.destroy();
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
            WindowEvent::ScaleFactorChanged {
                mut inner_size_writer,
                ..
            } => {
                let size = state.window.inner_size();
                if state.window.fullscreen().is_none() {
                    let _ = inner_size_writer.request_inner_size(size);
                }
                state.resize(size)
            }
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
                state.player.pointer = reel_ui::dpi::PhysicalPosition::new(position.x, position.y);
                state.pointer_event(PointerKind::Move)
            }
            WindowEvent::CursorLeft { .. } => state.pointer_event(PointerKind::Leave),
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
            WindowEvent::MouseWheel { delta, .. } if state.player.session.is_some() => {
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
            WindowEvent::MouseWheel { delta, .. } if state.player.session.is_none() => {
                let delta = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(y) * 0.1,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y * 0.002,
                };
                state.zoom = (state.zoom + delta).clamp(0.5, 1.5);
                let zoom = state.zoom;
                state
                    .player
                    .presentation
                    .bridge
                    .edit_widget(state.player.root_id, |mut widget| {
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
        #[cfg(feature = "performance")]
        self.record_performance();
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::HostEvent);
        // Clear before draining so a source that races the drain can wake the loop again.
        self.wake.acknowledge();
        #[cfg(feature = "performance")]
        self.start_performance();
        if let Some(state) = &mut self.state {
            if self.closing {
                if let Err(error) = state.tick_gamepads() {
                    self.fail(event_loop, error);
                    return;
                }
                let ticked = state.tick_story();
                #[cfg(feature = "performance")]
                performance::record(&mut self.capture, state, &mut self.capture_started);
                if let Err(error) = ticked {
                    self.error.get_or_insert_with(|| error.to_string());
                    self.state = None;
                    event_loop.exit();
                } else if state.close_complete() {
                    state.host.request_exit();
                    state.host.apply(event_loop);
                    self.state = None;
                } else {
                    let deadline = state
                        .story_deadline()
                        .map(|(deadline, _)| deadline)
                        .into_iter()
                        .chain(self.wake.is_pending().then(Instant::now))
                        .min();
                    state.host.wait_until(deadline);
                    state.host.apply(event_loop);
                    #[cfg(feature = "performance")]
                    if let Some(capture) = &mut self.capture {
                        capture.wait(deadline, performance::Reason::Shutdown);
                    }
                }
                return;
            }
            if state.is_suspended() {
                state.host.set_wake_deadline(None);
                state.host.apply(event_loop);
                #[cfg(feature = "performance")]
                if let Some(capture) = &mut self.capture {
                    capture.wait(None, performance::Reason::Suspended);
                }
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
            // This call can present the hidden startup window. Close its
            // measurement interval before polling or another event can present.
            #[cfg(feature = "performance")]
            performance::record(&mut self.capture, state, &mut self.capture_started);
            if let Err(e) = state.poll_completions() {
                self.recover_or_fail(event_loop, Box::new(e));
                return;
            }
            if state.player.needs_redraw() {
                state.host.request_redraw();
            }
            // Completion wake-up is bounded; no continuous redraw when idle.
            // Hidden startup windows may not deliver resource redraw wakes.
            let completion_interval = (state.player.first_frame_pending
                || state.player.needs_redraw()
                || state.player.backend.has_pending())
            .then(|| {
                if state.player.backend.has_pending_image_uploads() {
                    1
                } else {
                    8
                }
            });
            let completion_deadline =
                completion_interval.map(|ms| Instant::now() + Duration::from_millis(ms));
            let story_deadline = state.story_deadline();
            let deadlines = [
                completion_deadline,
                story_deadline.map(|(deadline, _)| deadline),
                state.input_deadline(),
                self.wake.is_pending().then(Instant::now),
            ];
            let deadline = deadlines.into_iter().flatten().min();
            state.host.wait_until(deadline);
            state.host.apply(event_loop);
            #[cfg(feature = "performance")]
            {
                performance::record(&mut self.capture, state, &mut self.capture_started);
                drop(_timing);
                if let Some(capture) = &mut self.capture {
                    use performance::Reason;
                    let reason = match (deadline, story_deadline) {
                        (None, _) => Reason::Idle,
                        (Some(_), _) if deadline == deadlines[0] => {
                            if completion_interval == Some(1) {
                                Reason::UploadPoll
                            } else {
                                Reason::CompletionPoll
                            }
                        }
                        (Some(at), Some((story, source))) if at == story => Reason::Story(source),
                        (Some(_), _) if deadline == deadlines[2] => Reason::Input,
                        (Some(_), _) => Reason::Notified,
                    };
                    capture.wait(deadline, reason);
                    self.capture_wait = Some(reel_core::profile::Scope::new(
                        reel_core::profile::Phase::HostWait,
                    ));
                }
            }
        }
    }
}
