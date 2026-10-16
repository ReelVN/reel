//! Platform-independent author playback. The host owns decoding and output.
pub use reel_core::audio::{AudioGroup, AudioLoopRegion};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AudioId(pub u64);
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
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AudioResult {
    Completed,
    Stopped,
    Cancelled,
    Failed(String),
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AudioCompletion {
    pub result: AudioResult,
    pub position: Duration,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AudioPlayback {
    pub request: AudioRequest,
    pub paused: bool,
    pub result: Option<AudioResult>,
}
#[derive(Clone, Copy, Debug)]
pub enum AudioCommand {
    Stop,
    Pause,
    Resume,
    Volume { value: f64, duration: Duration },
    Pan { value: f64, duration: Duration },
    FadeOut(Duration),
}
/// Calls run on the control thread. Admission is nonblocking; implementations
/// retain results until taken, and retain resources until physically released.
pub trait AudioBackend {
    fn play(&mut self, request: AudioRequest) -> Result<AudioId, String>;
    /// Owned logical state; no PCM, device handles or mixer ownership cross the boundary.
    fn snapshot(&self, id: AudioId) -> Result<AudioPlayback, String>;
    /// Last observed content position, without copying the playback request.
    fn position(&self, id: AudioId) -> Option<Duration>;
    fn command(&mut self, id: AudioId, command: AudioCommand) -> Result<(), String>;
    /// None selects the master bus. Applies to existing and future sounds.
    fn group_volume(
        &mut self,
        group: Option<AudioGroup>,
        value: f64,
        duration: Duration,
    ) -> Result<(), String>;
    fn cancel(&mut self, id: AudioId);
    fn maintain(&mut self);
    fn take_result(&mut self, id: AudioId) -> Option<AudioCompletion>;
    fn poll_after(&self) -> Option<Duration>;
    fn suspend(&mut self);
    fn resume(&mut self);
    /// Idempotent. Closes admission, stops output and cancels pending loads.
    fn close(&mut self);
    fn cleanup_pending(&self) -> bool;
}
