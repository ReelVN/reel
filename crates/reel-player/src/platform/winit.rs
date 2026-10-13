//! Winit host commands, usable with any RenderBackend.
use super::PlatformHost;
use reel_core::clock::{Clock, MonoTime, UnixMillis};
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use winit::{
    event_loop::{ActiveEventLoop, ControlFlow},
    window::Window,
};

pub struct WinitHost {
    window: Arc<Window>,
    origin: Instant,
    deadline: Option<MonoTime>,
    exiting: bool,
}
impl WinitHost {
    pub fn new(window: Arc<Window>) -> Self {
        Self {
            window,
            origin: Instant::now(),
            deadline: None,
            exiting: false,
        }
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
            event_loop.exit();
        }
        event_loop.set_control_flow(
            self.deadline
                .and_then(|time| self.origin.checked_add(Duration::from_nanos(time.0)))
                .map(ControlFlow::WaitUntil)
                .unwrap_or(ControlFlow::Wait),
        );
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
