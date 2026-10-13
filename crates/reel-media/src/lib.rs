//! AudioOutput and the Kira bridge, architecture contract 1.1.
#![allow(clippy::result_large_err)]

mod activity;
pub use activity::*;
mod retirement;
pub use retirement::{AudioActivityRetirement, AudioOwner};
mod contracts;
mod maintenance;
mod mixer;
mod output;
mod pcm;
pub use contracts::*;
pub use maintenance::{AudioReclaimService, ReclaimLimits, ReclaimStatus};
pub use mixer::{
    DecodedSound, DecodedSoundRequest, KiraMixer, MixerLimits, Playback, ReclaimProgress,
};
pub use output::{OfflineOutput, UnderrunSignal, bounded_pcm_queue};
pub use pcm::DeviceSample;

#[cfg(all(
    feature = "cpal-output",
    any(target_os = "linux", target_os = "windows", target_os = "macos")
))]
mod desktop;
#[cfg(all(
    feature = "cpal-output",
    any(target_os = "linux", target_os = "windows", target_os = "macos")
))]
pub use desktop::CpalOutput;
