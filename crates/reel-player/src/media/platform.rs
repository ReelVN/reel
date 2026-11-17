//! Target media assembly; the shared coordinator consumes output/codec interfaces.
use super::*;

pub(crate) fn desktop_media(
    assets: Assets,
    options: MediaOptions,
    wake: WakeSignal,
) -> impl Fn() -> Box<dyn MediaBackend> {
    let output = DesktopAudio::new(wake.clone());
    move || {
        let output = output.clone();
        Box::new(MediaPlayer::new(
            assets.clone(),
            options.clone(),
            wake.clone(),
            move |_| {
                output
                    .session()
                    .map(|output| Box::new(output) as Box<dyn AudioOutput>)
                    .map_err(|error| error.to_string())
            },
        ))
    }
}
