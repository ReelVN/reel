//! Platform implementations selected by target and enabled devices.
#[cfg(all(feature = "vita", target_os = "vita"))]
pub mod vita;
#[cfg(any(test, all(feature = "vita", target_os = "vita")))]
#[path = "platform/vita/signals.rs"]
mod vita_signals;
#[cfg(any(test, all(feature = "vita", target_os = "vita")))]
#[path = "platform/vita/text.rs"]
mod vita_text;
#[cfg(feature = "winit")]
pub mod winit;

#[cfg(all(
    feature = "gamepad",
    any(target_os = "windows", target_os = "linux", target_os = "macos")
))]
pub mod gamepad;

#[cfg(feature = "window")]
pub mod desktop;
