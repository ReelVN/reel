//! Target media assembly; the shared coordinator consumes output/codec interfaces.
use super::*;

/// Assemble this target's native output behind the common media backend.
pub fn native(assets: Assets, options: MediaOptions, wake: WakeSignal) -> MediaPlayer {
    MediaPlayer::new(assets, options, wake, |wake| {
        CpalOutput::new(wake, reel_core::clock::MonoTime(0))
            .map(|output| Box::new(output) as Box<dyn AudioOutput>)
            .map_err(|e| e.to_string())
    })
}
