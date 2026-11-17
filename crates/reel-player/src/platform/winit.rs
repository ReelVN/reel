//! Winit host commands, usable with any RenderBackend.
use crate::hal::{DisplayMetrics, Host, PlatformHost, WindowControl};
#[path = "winit_display.rs"]
mod display;
#[path = "winit_text.rs"]
mod text;
#[path = "winit_touch.rs"]
mod touch;
use reel_core::{
    clock::{Clock, MonoTime, UnixMillis},
    wake::WakeSignal,
};
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub use text::ime;
pub use touch::TouchInput;
use winit::{
    event_loop::{ActiveEventLoop, ControlFlow},
    window::Window,
};

pub struct WinitHost {
    window: Arc<Window>,
    origin: Instant,
    deadline: Option<MonoTime>,
    exiting: bool,
    text: text::TextInputHost,
    display: display::WindowDisplay,
    event_wake: Option<WakeSignal>,
}
impl WinitHost {
    pub fn new(window: Arc<Window>) -> Self {
        let display = display::WindowDisplay::new(&window);
        Self {
            window,
            origin: Instant::now(),
            deadline: None,
            exiting: false,
            text: text::TextInputHost::default(),
            display,
            event_wake: None,
        }
    }
    #[cfg(feature = "window")]
    pub(crate) fn with_event_wake(mut self, wake: WakeSignal) -> Self {
        self.event_wake = Some(wake);
        self
    }
    pub fn window(&self) -> &Window {
        &self.window
    }
    pub fn wait_until(&mut self, deadline: Option<Instant>) {
        self.set_wake_deadline(deadline.map(|when| {
            MonoTime(
                u64::try_from(when.saturating_duration_since(self.origin).as_nanos())
                    .unwrap_or(u64::MAX),
            )
        }));
    }
    /// Apply pending commands before returning control to the native event loop.
    pub fn apply(&self, event_loop: &ActiveEventLoop) {
        if self.exiting {
            // The Windows event loop can wait once more before observing exit.
            event_loop.set_control_flow(ControlFlow::Poll);
            event_loop.exit();
            return;
        }
        event_loop.set_control_flow(
            self.deadline
                .and_then(|time| self.origin.checked_add(Duration::from_nanos(time.0)))
                .map(ControlFlow::WaitUntil)
                .unwrap_or(ControlFlow::Wait),
        );
    }
}
impl WindowControl for WinitHost {
    fn window_info(&mut self) -> reel_core::window::WindowInfo {
        self.display.info(&self.window)
    }
    fn configure_window(
        &mut self,
        configuration: reel_core::window::WindowConfiguration,
    ) -> Result<bool, String> {
        self.display.configure(&self.window, configuration)
    }
    fn window_resized(&mut self) -> bool {
        self.display.resized(&self.window)
    }
    fn move_pointer(&mut self, position: [f64; 2]) -> Result<bool, String> {
        if !self.display.info(&self.window).capabilities.pointer_move {
            return Ok(false);
        }
        self.window
            .set_cursor_position(winit::dpi::LogicalPosition::new(position[0], position[1]))
            .map(|()| true)
            .map_err(|error| error.to_string())
    }
}
impl Host for WinitHost {
    fn display_metrics(&self) -> DisplayMetrics {
        let size = self.window.inner_size();
        DisplayMetrics {
            physical_size: [size.width, size.height],
            scale_factor: self.window.scale_factor(),
        }
    }
    fn set_title(&mut self, title: &str) {
        self.window.set_title(title);
    }
    fn set_visible(&mut self, visible: bool) {
        self.window.set_visible(visible);
    }
    fn wake_callback(&self) -> Arc<dyn Fn() + Send + Sync> {
        if let Some(wake) = &self.event_wake {
            let wake = wake.clone();
            return Arc::new(move || wake.wake());
        }
        let window = Arc::downgrade(&self.window);
        Arc::new(move || {
            if let Some(window) = window.upgrade() {
                window.request_redraw();
            }
        })
    }
    fn ui_signal_handler(&self) -> impl FnMut(reel_ui::app::RenderRootSignal) + 'static {
        WinitHost::ui_signal_handler(self)
    }
}
impl Clock for WinitHost {
    fn monotonic_now(&self) -> MonoTime {
        MonoTime(u64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(u64::MAX))
    }
    fn wall_time(&self) -> Option<UnixMillis> {
        Some(UnixMillis(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_millis()
                .try_into()
                .ok()?,
        ))
    }
}
impl PlatformHost for WinitHost {
    fn request_redraw(&mut self) {
        self.window.request_redraw();
    }
    fn before_present(&mut self) {
        self.window.pre_present_notify();
    }
    fn set_wake_deadline(&mut self, when: Option<MonoTime>) {
        self.deadline = when;
    }
    fn request_exit(&mut self) {
        self.exiting = true;
    }
}
