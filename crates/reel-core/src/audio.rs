//! Platform-independent audio routing and transport shared by hosts and media.

use std::time::Duration;
mod rate;
pub use rate::AudioRate;

/// A half-open content interval. An absent end selects the end of the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AudioLoopRegion {
    pub start: Duration,
    pub end: Option<Duration>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(usize)]
pub enum AudioGroup {
    Music,
    Voice,
    #[default]
    Effects,
    Ui,
}
impl AudioGroup {
    pub const ALL: [Self; 4] = [Self::Music, Self::Voice, Self::Effects, Self::Ui];
}
