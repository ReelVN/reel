//! Desktop media assembly; the shared coordinator only consumes output/codec interfaces.
use super::*;

/// Assemble the desktop output behind the common media backend.
pub fn desktop(assets: Assets, options: MediaOptions, wake: WakeSignal) -> MediaPlayer {
    MediaPlayer::new(assets, options, wake, |wake| {
        CpalOutput::new(wake, reel_core::clock::MonoTime(0))
            .map(|output| Box::new(output) as Box<dyn AudioOutput>)
            .map_err(|e| e.to_string())
    })
}
