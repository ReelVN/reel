//! Portable audio request and playback state.
use crate::media::MediaResult;
pub use reel_core::audio::{AudioGroup, AudioLoopRegion};
use std::time::Duration;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudioRequest {
    pub source: String,
    pub loop_region: Option<AudioLoopRegion>,
    pub volume: f64,
    pub group: AudioGroup,
    pub pan: f64,
    pub fade_in: Duration,
    pub start_position: Duration,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudioPlayback {
    pub request: AudioRequest,
    pub paused: bool,
    pub result: Option<MediaResult>,
}
