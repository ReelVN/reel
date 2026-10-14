//! Platform-independent author playback. The host owns decoding and output.
pub use reel_core::audio::AudioGroup;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AudioId(pub u64);
#[derive(Debug)]
pub struct AudioRequest {
    pub source: String,
    pub looping: bool,
    pub volume: f64,
    pub group: AudioGroup,
    pub pan: f64,
    pub fade_in: Duration,
}
#[derive(Clone, Debug, PartialEq)]
pub enum AudioResult {
    Completed,
    Stopped,
    Cancelled,
    Failed(String),
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
    fn take_result(&mut self, id: AudioId) -> Option<AudioResult>;
    fn poll_after(&self) -> Option<Duration>;
    fn suspend(&mut self);
    fn resume(&mut self);
    /// Idempotent. Closes admission, stops output and cancels pending loads.
    fn close(&mut self);
    fn cleanup_pending(&self) -> bool;
}
