//! Portable audio request and playback state.
use crate::media::MediaResult;
pub use reel_core::audio::{AudioGroup, AudioLoopRegion, AudioRate};
use std::time::Duration;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudioRequest {
    pub source: String,
    pub loop_region: Option<AudioLoopRegion>,
    pub volume: f64,
    /// Content speed and any remaining linear rate transition. Defaults to 1.
    #[serde(default)]
    pub rate: AudioRate,
    pub group: AudioGroup,
    pub pan: f64,
    pub fade_in: Duration,
    pub start_position: Duration,
    pub end_position: Option<Duration>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudioQueueItem {
    pub source: String,
    pub start_position: Duration,
    pub end_position: Option<Duration>,
    pub loop_region: Option<AudioLoopRegion>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudioPlayback {
    pub request: AudioRequest,
    /// Future items in submission order; a nonempty queue exits the current loop.
    pub queue: Vec<AudioQueueItem>,
    pub paused: bool,
    pub result: Option<MediaResult>,
}
