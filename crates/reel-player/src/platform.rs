//! Commands implemented by the embedding event loop, independent of its window library.
use reel_core::clock::MonoTime;
use reel_core::window::{WindowConfiguration, WindowInfo};
#[cfg(feature = "winit")]
pub mod winit;

pub trait PlatformHost {
    fn request_redraw(&mut self);
    /// None permits sleeping until an external event. Deadlines use the host's Clock.
    fn set_wake_deadline(&mut self, when: Option<MonoTime>);
    /// Called after render submission, immediately before presenting its native surface.
    fn before_present(&mut self);
    fn request_exit(&mut self);
}

/// Optional client-window control. Rendering synchronization belongs to RenderBackend.
pub trait WindowControl {
    fn window_info(&mut self) -> WindowInfo;
    /// true: actual state is available; false: wait for the platform resize event.
    fn configure_window(&mut self, configuration: WindowConfiguration) -> Result<bool, String>;
    /// Called after the platform applies a native resize, including fullscreen changes.
    fn window_resized(&mut self) -> bool;
}

#[cfg(all(
    feature = "gamepad",
    any(target_os = "windows", target_os = "linux", target_os = "macos")
))]
pub mod gamepad;
