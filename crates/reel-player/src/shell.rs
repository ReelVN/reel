use crate::window::{PointerKind, State};
use masonry_core::core::TextEvent;
use reel_render::gpu::GpuError;
use std::time::{Duration, Instant};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::WindowId,
};
type Error = Box<dyn std::error::Error>;
pub fn run() -> Result<(), Error> {
    let event_loop = EventLoop::new()?;
    let mut app = App {
        state: None,
        error: None,
        next_generation: 1,
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
}
impl App {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: impl std::fmt::Display) {
        self.error = Some(error.to_string());
        self.state = None;
        event_loop.exit();
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
        match State::new(event_loop, self.next_generation) {
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
                state.bridge.clear();
                self.state = None;
                event_loop.exit();
                return;
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key
                        == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::F5) =>
            {
                state.renderer.gpu().device.destroy();
                state.invalidate();
                Ok(())
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key
                        == winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::F6) =>
            {
                state.suspend();
                state.resume()
            }
            WindowEvent::CloseRequested => {
                state.bridge.clear();
                self.state = None;
                event_loop.exit();
                return;
            }
            WindowEvent::Resized(size) => state.resize(size),
            WindowEvent::ScaleFactorChanged { .. } => state.resize(state.window.inner_size()),
            WindowEvent::Focused(focus) => {
                state
                    .bridge
                    .handle_text_event(TextEvent::WindowFocusChange(focus));
                if !focus {
                    state.pointer_event(PointerKind::Cancel);
                }
                Ok(())
            }
            WindowEvent::CursorMoved { position, .. } => {
                state.pointer = masonry_core::dpi::PhysicalPosition::new(position.x, position.y);
                state.pointer_event(PointerKind::Move);
                Ok(())
            }
            WindowEvent::MouseInput {
                state: button,
                button: MouseButton::Left,
                ..
            } => {
                state.pointer_event(if button == ElementState::Pressed {
                    PointerKind::Down
                } else {
                    PointerKind::Up
                });
                Ok(())
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let delta = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(y) * 0.1,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y * 0.002,
                };
                state.zoom = (state.zoom + delta).clamp(0.5, 1.5);
                let zoom = state.zoom;
                state.bridge.edit_widget(state.root_id, |mut widget| {
                    widget.set_transform(masonry_core::kurbo::Affine::scale(zoom))
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
            if state.is_suspended() {
                event_loop.set_control_flow(ControlFlow::Wait);
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
            if state.demand.has_work() || state.renderer.usage().targets != 0 {
                event_loop.set_control_flow(ControlFlow::WaitUntil(
                    Instant::now() + Duration::from_millis(8),
                ));
            } else {
                event_loop.set_control_flow(ControlFlow::Wait);
            }
        }
    }
}
