//! Native window state and asynchronous resize completion, independent of GPU APIs.
use reel_core::window::{
    WindowCapabilities, WindowConfiguration, WindowInfo, WindowPlatform, WindowSize,
};
use std::sync::Arc;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::{
    dpi::LogicalSize,
    monitor::MonitorHandle,
    window::{Fullscreen, Window},
};

struct Pending {
    fullscreen: bool,
    resize_after_exit: Option<WindowSize>,
}
pub(super) struct WindowDisplay {
    windowed: WindowSize,
    monitor: Option<MonitorHandle>,
    scale: f64,
    resolutions: Arc<[WindowSize]>,
    pending: Option<Pending>,
}
fn size(window: &Window) -> WindowSize {
    let size = window.inner_size().to_logical::<f64>(window.scale_factor());
    WindowSize {
        width: size.width,
        height: size.height,
    }
}
fn platform() -> WindowPlatform {
    if cfg!(target_arch = "wasm32") {
        WindowPlatform::Web
    } else if cfg!(target_os = "windows") {
        WindowPlatform::Windows
    } else if cfg!(target_os = "macos") {
        WindowPlatform::Macos
    } else if cfg!(target_os = "linux") {
        WindowPlatform::Linux
    } else if cfg!(target_os = "android") {
        WindowPlatform::Android
    } else if cfg!(target_os = "ios") {
        WindowPlatform::Ios
    } else {
        WindowPlatform::Unknown
    }
}
fn matches_size(window: &Window, desired: WindowSize) -> bool {
    window.inner_size()
        == LogicalSize::new(desired.width, desired.height).to_physical::<u32>(window.scale_factor())
}
impl WindowDisplay {
    pub(super) fn new(window: &Window) -> Self {
        Self {
            windowed: size(window),
            monitor: None,
            scale: 0.,
            resolutions: Arc::from([]),
            pending: None,
        }
    }
    pub(super) fn info(&mut self, window: &Window) -> WindowInfo {
        let monitor = window.current_monitor();
        let scale = window.scale_factor();
        if self.monitor != monitor || self.scale != scale {
            let mut resolutions: Vec<_> = monitor
                .as_ref()
                .into_iter()
                .flat_map(|monitor| monitor.video_modes())
                .map(|mode| {
                    let size = mode.size().to_logical::<f64>(scale);
                    WindowSize {
                        width: size.width,
                        height: size.height,
                    }
                })
                .collect();
            resolutions.sort_by(|a, b| {
                a.width
                    .total_cmp(&b.width)
                    .then(a.height.total_cmp(&b.height))
            });
            resolutions.dedup();
            self.resolutions = resolutions.into();
            self.monitor = monitor;
            self.scale = scale;
        }
        let fullscreen = window.fullscreen().is_some();
        let current = size(window);
        if !fullscreen && self.pending.is_none() {
            self.windowed = current;
        }
        let desktop = cfg!(any(
            target_os = "windows",
            target_os = "linux",
            target_os = "macos"
        ));
        WindowInfo {
            platform: platform(),
            size: current,
            windowed_size: self.windowed,
            scale_factor: scale,
            fullscreen,
            vsync: true,
            capabilities: WindowCapabilities {
                resize: desktop,
                fullscreen: desktop,
                vsync: false,
                // Wayland requires a locked cursor; this API moves an ordinary
                // client-area pointer and does not acquire a lock.
                pointer_move: window.window_handle().is_ok_and(|handle| {
                    matches!(
                        handle.as_raw(),
                        RawWindowHandle::Win32(_)
                            | RawWindowHandle::AppKit(_)
                            | RawWindowHandle::Xlib(_)
                            | RawWindowHandle::Xcb(_)
                    )
                }),
            },
            resolutions: self.resolutions.clone(),
        }
    }
    pub(super) fn configure(
        &mut self,
        window: &Window,
        change: WindowConfiguration,
    ) -> Result<bool, String> {
        if self.pending.is_some() {
            return Err("window configuration is Busy".into());
        }
        let before = self.info(window);
        self.windowed = WindowSize {
            width: change.width.unwrap_or(self.windowed.width),
            height: change.height.unwrap_or(self.windowed.height),
        };
        let fullscreen = change.fullscreen.unwrap_or(before.fullscreen);
        if fullscreen != before.fullscreen {
            self.pending = Some(Pending {
                fullscreen,
                resize_after_exit: (!fullscreen).then_some(self.windowed),
            });
            let previous = window.inner_size();
            window.set_fullscreen(
                fullscreen.then(|| Fullscreen::Borderless(window.current_monitor())),
            );
            let reached_size = if fullscreen {
                window
                    .current_monitor()
                    .is_some_and(|monitor| window.inner_size() == monitor.size())
            } else {
                matches_size(window, self.windowed)
            };
            if window.inner_size() != previous || reached_size {
                return Ok(self.resized(window));
            }
            return Ok(false);
        }
        if !fullscreen && (change.width.is_some() || change.height.is_some()) {
            let desired = self.windowed;
            // Logical dimensions may round to the existing physical size. In
            // that case the platform need not emit another resize event.
            if matches_size(window, desired) {
                self.windowed = size(window);
                return Ok(true);
            }
            if let Some(actual) =
                window.request_inner_size(LogicalSize::new(desired.width, desired.height))
            {
                let actual = actual.to_logical::<f64>(window.scale_factor());
                self.windowed = WindowSize {
                    width: actual.width,
                    height: actual.height,
                };
                return Ok(true);
            }
            // Windows applies SetWindowPos immediately but Winit returns None.
            // The actual size also covers an already-applied native request.
            if matches_size(window, desired) {
                self.windowed = size(window);
                return Ok(true);
            }
            self.pending = Some(Pending {
                fullscreen: false,
                resize_after_exit: None,
            });
            return Ok(false);
        }
        Ok(true)
    }
    pub(super) fn resized(&mut self, window: &Window) -> bool {
        let Some(pending) = &mut self.pending else {
            if window.fullscreen().is_none() {
                self.windowed = size(window);
            }
            return true;
        };
        if window.fullscreen().is_some() != pending.fullscreen {
            return false;
        }
        if let Some(desired) = pending.resize_after_exit.take()
            && !matches_size(window, desired)
            && window
                .request_inner_size(LogicalSize::new(desired.width, desired.height))
                .is_none()
            && !matches_size(window, desired)
        {
            return false;
        }
        self.pending = None;
        if window.fullscreen().is_none() {
            self.windowed = size(window);
        }
        true
    }
}
