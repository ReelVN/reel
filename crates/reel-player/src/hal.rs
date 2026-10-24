//! Host contracts consumed by the shared player. Native event loops translate
//! input before dispatch; render, media and storage retain their own contracts.
use reel_core::{
    clock::{Clock, MonoTime},
    window::{WindowConfiguration, WindowInfo},
};

/// Commands applied by the event loop after shared player work completes.
pub trait PlatformHost {
    fn request_redraw(&mut self);
    /// `None` permits sleeping until an input or completion wake.
    fn set_wake_deadline(&mut self, when: Option<MonoTime>);
    /// Called after submission, immediately before native presentation.
    fn before_present(&mut self);
    fn request_exit(&mut self);
}

/// Optional display operations. Capabilities describe supported requests.
pub trait WindowControl {
    fn window_info(&mut self) -> WindowInfo;
    /// `true` means the actual state is available; `false` awaits a resize event.
    fn configure_window(&mut self, configuration: WindowConfiguration) -> Result<bool, String>;
    fn window_resized(&mut self) -> bool;
    /// Client-area logical coordinates. `false` means pointer movement is unsupported.
    fn move_pointer(&mut self, position: [f64; 2]) -> Result<bool, String>;
}

/// Physical output geometry, independent of an author's logical canvas.
#[derive(Clone, Copy, Debug)]
pub struct DisplayMetrics {
    pub physical_size: [u32; 2],
    pub scale_factor: f64,
}

/// Host services used by a running player. Device construction and recovery
/// belong to the platform assembly, which supplies a renderer separately.
pub trait Host: PlatformHost + WindowControl + Clock {
    fn display_metrics(&self) -> DisplayMetrics;
    fn set_title(&mut self, title: &str);
    fn set_visible(&mut self, visible: bool);
    /// Create a native event-loop wake callback for background services.
    fn wake_callback(&self) -> std::sync::Arc<dyn Fn() + Send + Sync>;
    /// Consume portable cursor, clipboard and text-input requests.
    fn ui_signal_handler(&self) -> impl FnMut(reel_ui::app::RenderRootSignal) + 'static;
}
