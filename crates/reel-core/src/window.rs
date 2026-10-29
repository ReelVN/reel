//! Portable window presentation state. Dimensions use client-area logical pixels.
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowSize {
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowCapabilities {
    pub resize: bool,
    pub fullscreen: bool,
    pub vsync: bool,
    pub pointer_move: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowInfo {
    pub platform: WindowPlatform,
    pub size: WindowSize,
    pub windowed_size: WindowSize,
    pub scale_factor: f64,
    pub fullscreen: bool,
    pub vsync: bool,
    pub capabilities: WindowCapabilities,
    /// Current display refresh rate in millihertz, or None when unavailable.
    pub refresh_rate_millihertz: Option<u32>,
    pub resolutions: Arc<[WindowSize]>,
}

/// Declared by the window host; separate from available window capabilities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowPlatform {
    Windows,
    Macos,
    Linux,
    Android,
    Ios,
    Web,
    /// PlayStation Vita's fixed native display and device controls.
    Vita,
    #[default]
    Unknown,
}

impl WindowPlatform {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::Linux => "linux",
            Self::Android => "android",
            Self::Ios => "ios",
            Self::Web => "web",
            Self::Vita => "vita",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowConfiguration {
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub fullscreen: Option<bool>,
    pub vsync: Option<bool>,
}
