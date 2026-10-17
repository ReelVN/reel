//! Host-owned audio and video transport. Native devices remain in adapters.
use crate::{
    audio::{AudioGroup, AudioPlayback, AudioRequest},
    video::{VideoPlaybackState, VideoRequest},
};
use reel_ui::resources::ResourceLease;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MediaId(pub u64);
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MediaResult {
    Completed,
    Stopped,
    Cancelled,
    Failed(String),
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MediaCompletion {
    pub result: MediaResult,
    pub position: Duration,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum MediaPlayback {
    Audio(AudioPlayback),
    Video(VideoPlaybackState),
}
#[derive(Clone, Copy, Debug)]
pub enum MediaCommand {
    Seek(Duration),
    Stop,
    Pause,
    Resume,
    Finish,
    Volume { value: f64, duration: Duration },
    Pan { value: f64, duration: Duration },
    FadeOut(Duration),
}
/// Nonblocking control-thread boundary. Results remain until taken; retired
/// readers, workers and device allocations remain owned until released.
pub trait MediaBackend {
    fn play_audio(&mut self, request: AudioRequest) -> Result<MediaId, String>;
    fn play_video(&mut self, _request: VideoRequest) -> Result<MediaId, String> {
        Err("video playback is not installed".into())
    }
    fn restore_video(&mut self, state: VideoPlaybackState) -> Result<MediaId, String> {
        let _ = state;
        Err("video restoration is not installed".into())
    }
    fn snapshot(&self, id: MediaId) -> Result<MediaPlayback, String>;
    fn position(&self, id: MediaId) -> Option<Duration>;
    fn command(&mut self, id: MediaId, command: MediaCommand) -> Result<(), String>;
    fn take_video_frame(&mut self, _id: MediaId) -> Option<ResourceLease> {
        None
    }
    /// None selects master; videos use the same music bus as other media.
    fn group_volume(
        &mut self,
        group: Option<AudioGroup>,
        value: f64,
        duration: Duration,
    ) -> Result<(), String>;
    fn cancel(&mut self, id: MediaId);
    /// Elapsed session time excludes host suspension. Audio uses the mixer clock.
    fn maintain(&mut self, elapsed: Duration);
    fn take_result(&mut self, id: MediaId) -> Option<MediaCompletion>;
    fn poll_after(&self) -> Option<Duration>;
    fn suspend(&mut self);
    fn resume(&mut self);
    fn close(&mut self);
    fn cleanup_pending(&self) -> bool;
}
