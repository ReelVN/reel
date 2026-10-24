//! Platform implementations selected by target and enabled devices.
#[cfg(feature = "winit")]
pub mod winit;

#[cfg(all(
    feature = "gamepad",
    any(target_os = "windows", target_os = "linux", target_os = "macos")
))]
pub mod gamepad;

#[cfg(feature = "window")]
pub mod desktop;
