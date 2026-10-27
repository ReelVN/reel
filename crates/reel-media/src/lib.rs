//! Device audio output, media activities, decoding and background PCM retirement.
//!
//! Configure an output before constructing its mixer. The callback uses
//! preallocated buffers and bounded queues; [`AudioReclaimService`] retains
//! retired PCM until physical release. Desktop output uses the `cpal-output`
//! feature, software audio codecs use `decode`, and offline Opus encoding uses
//! `encode`. `video` provides playback contracts and queues; `video-software`
//! adds MP4 H.264/AAC decoding.
//! Codec selection stays behind the shared [`decode`] interfaces.
#![allow(clippy::result_large_err)]

mod activity;
pub use activity::*;
mod retirement;
pub use retirement::{AudioActivityRetirement, AudioOwner};
mod contracts;
pub mod decode;
#[cfg(feature = "encode")]
pub mod encode;
mod maintenance;
mod mixer;
mod output;
mod pcm;
mod worker_event;
pub use contracts::*;
pub use maintenance::{AudioReclaimService, ReclaimLimits, ReclaimStatus};
pub use mixer::{
    DecodeLimits, DecodedSound, DecodedSoundRequest, KiraMixer, MixerLimits, Playback,
    ReclaimProgress,
};
pub use output::{OfflineOutput, UnderrunSignal, bounded_pcm_queue};
pub use pcm::DeviceSample;

#[cfg(all(feature = "vita-output", target_os = "vita"))]
mod vita;
#[cfg(all(feature = "vita-output", target_os = "vita"))]
pub use vita::VitaOutput;

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
