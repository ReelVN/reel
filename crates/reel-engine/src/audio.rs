//! Platform-independent author playback. The host owns decoding and output.
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AudioId(pub u64);
#[derive(Debug)]
pub struct AudioRequest {
    pub source: String,
    pub looping: bool,
    pub volume: f64,
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
    Volume(f64),
}
/// Calls run on the control thread. Admission is nonblocking; implementations
/// retain results until taken, and retain resources until physically released.
pub trait AudioBackend {
    fn play(&mut self, request: AudioRequest) -> Result<AudioId, String>;
    fn command(&mut self, id: AudioId, command: AudioCommand) -> Result<(), String>;
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
