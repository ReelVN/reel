//! Opt-in developer measurements through the ordinary player and package path.
//! Request JSON and terminal status use schema 1; frame TSV tokens remain stable.
pub mod capture;
pub use capture::{CachePolicy, FrameCaptureOptions};
#[cfg(feature = "performance")]
pub mod frame_archive;
#[cfg(any(target_os = "vita", feature = "window", test))]
pub(crate) mod recording;

/// Select sampled VM timing for subsequent slices on the calling thread.
/// Disabled timing issues no observer clocks or timing-counter writes. The
/// supplied VM budget clock and execution semantics retain their ordinary path.
pub fn set_vm_profiling_enabled(enabled: bool) -> Result<bool, &'static str> {
    #[cfg(feature = "vm-profiling")]
    {
        aside_vm::executor::set_profiling_enabled(enabled);
        Ok(enabled)
    }
    #[cfg(not(feature = "vm-profiling"))]
    {
        if enabled {
            Err("vm-timing=on requires reel-player/vm-profiling")
        } else {
            Ok(false)
        }
    }
}

#[cfg(feature = "performance")]
mod request;
#[cfg(feature = "performance")]
pub use request::{Capabilities, Request, StatusState};
