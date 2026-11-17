//! Explicit native-device acceptance; ordinary CI uses the offline player contracts.
#![cfg(all(
    feature = "window",
    any(target_os = "windows", target_os = "linux", target_os = "macos")
))]

use reel_engine::{
    Epoch, Session, SessionId,
    aside::{SliceBudget, VmLimits},
};
use reel_media::DesktopAudio;
use reel_player::{
    aside_session::{AsideSessionUi, StoryState},
    assets::{AssetOptions, Assets},
    media::{MediaOptions, MediaPlayer},
    story_runtime::StoryRuntime,
};
use std::time::{Duration, Instant};

#[test]
#[ignore = "requires a real default audio output device"]
fn author_audio_source_and_asb_complete_on_native_output() {
    // Silence preserves actual decoding, PCM consumption and device callbacks.
    let source = r#"fn main() {
        let sound = audio.play("assets/chime.wav", volume: 0);
        if await sound != "Completed" || await sound != "Completed" { throw "native completion"; }
        let music = audio.play("assets/chime.wav", looping: true, volume: 0);
        pause(music); resume(music); stop(music);
        if await music != "Stopped" { throw "native stop"; }
    }"#;
    let checked = reel_cli::check(source, "native_audio.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    let stories = [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
    ];
    for story in stories {
        let wake = reel_core::wake::WakeSignal::new(|| {});
        let output = DesktopAudio::new(wake.clone());
        let backend = MediaPlayer::new(
            Assets::directory(AssetOptions {
                root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
                ..Default::default()
            }),
            MediaOptions::default(),
            wake,
            move |_| {
                output
                    .session()
                    .map(|audio| Box::new(audio) as Box<dyn reel_media::AudioOutput>)
                    .map_err(|error| error.to_string())
            },
        );
        let driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap()
        .with_media(Box::new(backend))
        .unwrap();
        let mut runtime = StoryRuntime::new(driver);
        let start = Instant::now();
        while runtime.needs_tick() {
            runtime
                .tick(SliceBudget::default(), || start.elapsed().as_nanos() as u64)
                .unwrap();
            assert!(
                matches!(
                    runtime.driver().state(),
                    StoryState::Running | StoryState::Finished
                ),
                "native audio state: {:?}, fault: {:?}",
                runtime.driver().state(),
                runtime.driver().fault()
            );
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "native audio wait or physical retirement stalled"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(runtime.driver().state(), StoryState::Finished);
        assert!(!runtime.driver().cleanup_pending());
    }
}
