//! Portable video state, independent of container codecs and native output.
use crate::media::MediaResult;
use std::time::Duration;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VideoRequest {
    pub source: String,
    pub volume: f64,
    pub looping: bool,
    pub start_position: Duration,
    pub end_position: Option<Duration>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VideoPlaybackState {
    pub request: VideoRequest,
    pub position: Duration,
    pub paused: bool,
    pub result: Option<MediaResult>,
}
