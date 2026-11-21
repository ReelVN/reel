use reel_core::{
    clock::{Clock, MonoTime, UnixMillis},
    window::{WindowConfiguration, WindowInfo, WindowPlatform, WindowSize},
};
use reel_player::hal::{DisplayMetrics, Host, PlatformHost, WindowControl};
use reel_ui::app::RenderRootSignal;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use web_time::{Instant, SystemTime, UNIX_EPOCH};

pub(super) struct CanvasHost {
    origin: Instant,
    pub metrics: DisplayMetrics,
    pub exiting: bool,
    redraw: Arc<AtomicBool>,
}

impl CanvasHost {
    pub fn new(metrics: DisplayMetrics) -> Self {
        Self {
            origin: Instant::now(),
            metrics,
            exiting: false,
            redraw: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn take_redraw(&self) -> bool {
        self.redraw.swap(false, Ordering::Relaxed)
    }
}

impl Clock for CanvasHost {
    fn monotonic_now(&self) -> MonoTime {
        MonoTime(self.origin.elapsed().as_nanos() as u64)
    }

    fn wall_time(&self) -> Option<UnixMillis> {
        let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
        Some(UnixMillis(elapsed.as_millis() as i64))
    }
}

impl PlatformHost for CanvasHost {
    fn request_redraw(&mut self) {
        self.redraw.store(true, Ordering::Relaxed);
    }

    // Browser display cadence is driven by requestAnimationFrame.
    fn set_wake_deadline(&mut self, _when: Option<MonoTime>) {}
    fn before_present(&mut self) {}

    fn request_exit(&mut self) {
        self.exiting = true;
    }
}

impl WindowControl for CanvasHost {
    fn window_info(&mut self) -> WindowInfo {
        let [width, height] = self.metrics.physical_size;
        let size = WindowSize {
            width: f64::from(width),
            height: f64::from(height),
        };
        WindowInfo {
            platform: WindowPlatform::Web,
            size,
            windowed_size: size,
            scale_factor: self.metrics.scale_factor,
            vsync: true,
            ..Default::default()
        }
    }

    fn configure_window(&mut self, _configuration: WindowConfiguration) -> Result<bool, String> {
        Err("The page controls the preview canvas size and display settings.".into())
    }

    fn window_resized(&mut self) -> bool {
        false
    }
    fn move_pointer(&mut self, _position: [f64; 2]) -> Result<bool, String> {
        Ok(false)
    }
}

impl Host for CanvasHost {
    fn display_metrics(&self) -> DisplayMetrics {
        self.metrics
    }
    fn set_title(&mut self, _title: &str) {}
    fn set_visible(&mut self, _visible: bool) {}

    fn wake_callback(&self) -> Arc<dyn Fn() + Send + Sync> {
        // The next browser frame drains the session's ready work.
        Arc::new(|| {})
    }

    fn ui_signal_handler(&self) -> impl FnMut(RenderRootSignal) + 'static {
        let redraw = self.redraw.clone();
        move |signal| {
            if matches!(
                signal,
                RenderRootSignal::RequestRedraw | RenderRootSignal::RequestAnimFrame
            ) {
                redraw.store(true, Ordering::Relaxed);
            }
        }
    }
}
