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
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowInfo {
    pub size: WindowSize,
    pub windowed_size: WindowSize,
    pub scale_factor: f64,
    pub fullscreen: bool,
    pub vsync: bool,
    pub capabilities: WindowCapabilities,
    pub resolutions: Arc<[WindowSize]>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowConfiguration {
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub fullscreen: Option<bool>,
    pub vsync: Option<bool>,
}
