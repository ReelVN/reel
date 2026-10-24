//! Device audio output, media activities, decoding and background PCM retirement.
//!
//! Configure an output before constructing its mixer. The callback uses
//! preallocated buffers and bounded queues; [`AudioReclaimService`] retains
//! retired PCM until physical release. Native output uses the `cpal-output`
//! feature, desktop software codecs use `decode`, and movie decoding uses `video`.
//! Native codec selection stays behind the shared [`decode`] interfaces.
#![allow(clippy::result_large_err)]

mod activity;
pub use activity::*;
mod retirement;
pub use retirement::{AudioActivityRetirement, AudioOwner};
mod contracts;
pub mod decode;
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

#[cfg(feature = "video")]
pub mod video;

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
