use super::{tests::*, *};
use reel_engine::video::{VideoPlaybackState, VideoRequest};
mod retirement;

fn movie(looping: bool) -> VideoRequest {
    VideoRequest {
        source: "crates/reel-media/tests/fixtures/baseline-aac.mp4".into(),
        volume: 1.,
        looping,
        start_position: Duration::ZERO,
        end_position: None,
    }
}
fn rig_video() -> (MediaPlayer, Output) {
    let mut options = MediaOptions::default();
    options.mixer.max_sounds = 2;
    rig_at(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        options,
    )
}
fn snapshot(player: &MediaPlayer, id: MediaId) -> VideoPlaybackState {
    let MediaPlayback::Video(state) = player.snapshot(id).unwrap() else {
        panic!("video snapshot")
    };
    state
}

#[test]
fn initially_paused_video_prepares_picture_and_preserves_seek_restore_state() {
    let (mut player, output) = rig_video();
    let id = player.play_video(movie(false), true).unwrap();
    until(|| {
        player.maintain(Duration::from_secs(1));
        assert_eq!(output.render(), 0.);
        assert_eq!(player.position(id), Some(Duration::ZERO));
        player.take_video_frame(id).is_some()
    });
    player
        .command(id, MediaCommand::Seek(Duration::from_millis(250)))
        .unwrap();
    until(|| {
        player.maintain(Duration::from_secs(1));
        assert_eq!(output.render(), 0.);
        assert_eq!(player.position(id), Some(Duration::from_millis(250)));
        player.take_video_frame(id).is_some()
    });
    let saved = snapshot(&player, id);
    assert!(saved.paused);
    player.cancel(id);
    let restored = player.restore_video(saved).unwrap();
    until(|| {
        player.maintain(Duration::from_secs(1));
        assert_eq!(output.render(), 0.);
        assert_eq!(player.position(restored), Some(Duration::from_millis(250)));
        player.take_video_frame(restored).is_some()
    });
    player.command(restored, MediaCommand::Resume).unwrap();
    until(|| {
        player.maintain(Duration::from_millis(1));
        output.render() > 0.1
    });
    player.cancel(restored);
    player.close();
    until(|| {
        player.maintain(Duration::ZERO);
        !player.cleanup_pending()
    });
}

#[test]
fn initially_paused_video_can_resume_before_decode() {
    let (mut player, output) = rig_video();
    let id = player.play_video(movie(false), true).unwrap();
    player.command(id, MediaCommand::Resume).unwrap();
    until(|| {
        player.maintain(Duration::from_millis(1));
        output.render() > 0.1
    });
    assert!(!snapshot(&player, id).paused);
    player.cancel(id);
    player.close();
    until(|| {
        player.maintain(Duration::ZERO);
        !player.cleanup_pending()
    });
}

#[test]
fn video_shares_output_restores_paused_transport_and_terminal_poster_without_sound() {
    let (mut player, output) = rig_video();
    let mut music = request(true);
    music.source = "examples/assets/chime.wav".into();
    let music_id = player.play_audio(music, false).unwrap();
    player.commit_audio(music_id);
    let id = player.play_video(movie(false), false).unwrap();
    let mut frame = None;
    until(|| {
        player.maintain(Duration::from_millis(2));
        output.render();
        if let Some(next) = player.take_video_frame(id) {
            frame = Some(next);
        }
        frame.is_some() && player.position(id).unwrap() > Duration::from_millis(20)
    });
    let reel_ui::resources::ResourceView::Yuv420(image) = frame.as_ref().unwrap().view() else {
        panic!("native YUV lease")
    };
    assert_eq!(image.size(), [64, 48]);
    assert_eq!(image.plane(0).len(), 64 * 48);
    player.command(id, MediaCommand::Pause).unwrap();
    for milliseconds in 0..1000 {
        player
            .command(id, MediaCommand::Seek(Duration::from_millis(milliseconds)))
            .unwrap();
    }
    player
        .command(id, MediaCommand::Seek(Duration::from_millis(625)))
        .unwrap();
    assert_eq!(
        player.decoder_generations(),
        1,
        "scrubbing retains one retiring decoder and only the latest target"
    );
    assert_eq!(player.position(id), Some(Duration::from_millis(625)));
    until(|| {
        player.maintain(Duration::from_millis(2));
        assert!(
            player.decoder_generations() <= 1,
            "seek generations cannot overlap"
        );
        output.render();
        if let Some(next) = player.take_video_frame(id) {
            frame = Some(next);
            true
        } else {
            false
        }
    });
    let saved = snapshot(&player, id);
    assert_eq!(saved.position, Duration::from_millis(625));
    assert!(saved.paused);
    assert_eq!(
        saved.request.start_position,
        Duration::ZERO,
        "seek must preserve loop/segment origin"
    );
    player.cancel(music_id);
    player.close();
    until(|| {
        player.maintain(Duration::ZERO);
        !player.cleanup_pending()
    });
    assert!(
        player.video_usage().bytes > 0,
        "accepted frame owns its decoded storage after session closure"
    );
    drop(frame);
    assert_eq!(player.video_usage().bytes, 0);

    let (mut restored, output) = rig_video();
    let id = restored.restore_video(saved).unwrap();
    let mut final_frame = None;
    until(|| {
        restored.maintain(Duration::from_millis(2));
        assert_eq!(
            output.render(),
            0.,
            "restored paused movie cannot emit audio"
        );
        final_frame = restored.take_video_frame(id);
        final_frame.is_some()
    });
    assert_eq!(restored.position(id), Some(Duration::from_millis(625)));
    restored.command(id, MediaCommand::Resume).unwrap();
    let mut audible = false;
    let mut completed = None;
    until(|| {
        restored.maintain(Duration::from_millis(2));
        audible |= output.render() > 0.01;
        if let Some(frame) = restored.take_video_frame(id) {
            final_frame = Some(frame);
        }
        let state = snapshot(&restored, id);
        if state.result.is_some() {
            completed = Some(state);
            true
        } else {
            false
        }
    });
    assert!(audible, "real AAC reaches the same mixer output");
    let completed = completed.unwrap();
    assert_eq!(completed.result, Some(MediaResult::Completed));
    assert_eq!(completed.position, Duration::from_secs(1));
    let last_version = final_frame.as_ref().unwrap().version().clone();
    assert_eq!(
        restored.take_result(id).unwrap().result,
        MediaResult::Completed
    );
    restored.close();
    until(|| {
        restored.maintain(Duration::ZERO);
        !restored.cleanup_pending()
    });
    drop(final_frame);
    assert_eq!(restored.video_usage().bytes, 0);

    let (mut poster, output) = rig_video();
    let id = poster.restore_video(completed).unwrap();
    let mut image = None;
    let mut result = None;
    until(|| {
        poster.maintain(Duration::from_millis(20));
        assert_eq!(
            output.render(),
            0.,
            "terminal restoration only decodes a picture"
        );
        if let Some(frame) = poster.take_video_frame(id) {
            image = Some(frame);
        }
        result = poster.take_result(id);
        result.is_some()
    });
    assert_eq!(image.as_ref().unwrap().version(), &last_version);
    assert_eq!(result.unwrap().result, MediaResult::Completed);
    poster.close();
    until(|| {
        poster.maintain(Duration::ZERO);
        !poster.cleanup_pending()
    });
    drop(image);
    assert_eq!(poster.video_usage().bytes, 0);
}

#[test]
fn video_loop_keeps_segment_origin_and_cancellation_reaps_streams() {
    let (mut player, output) = rig_video();
    let mut request = movie(true);
    request.start_position = Duration::from_millis(200);
    request.end_position = Some(Duration::from_millis(400));
    let id = player.play_video(request, false).unwrap();
    let mut last = Duration::ZERO;
    let mut wrapped = false;
    until(|| {
        player.maintain(Duration::from_millis(2));
        output.render();
        drop(player.take_video_frame(id));
        let position = player.position(id).unwrap();
        wrapped |= position < last;
        last = position;
        wrapped
    });
    assert_eq!(
        snapshot(&player, id).request.start_position,
        Duration::from_millis(200)
    );
    assert!(player.command(id, MediaCommand::Finish).is_err());
    player.command(id, MediaCommand::Stop).unwrap();
    assert_eq!(player.take_result(id).unwrap().result, MediaResult::Stopped);
    let missing = player
        .play_video(
            VideoRequest {
                source: "not-present.mp4".into(),
                ..movie(false)
            },
            false,
        )
        .unwrap();
    until(|| {
        player.maintain(Duration::ZERO);
        output.render();
        if let Some(result) = player.take_result(missing) {
            assert!(
                matches!(result.result, MediaResult::Failed(ref error) if error.contains("not-present.mp4"))
            );
            true
        } else {
            false
        }
    });
    let pending = player.play_video(movie(false), false).unwrap();
    player.cancel(pending);
    // Admission counts cancelled readers until their decoder threads exit.
    // A burst of replace/cancel operations cannot bypass the host's capacity.
    for _ in 0..128 {
        match player.play_video(movie(false), false) {
            Ok(id) => player.cancel(id),
            Err(error) => {
                assert_eq!(error, "media activity capacity exhausted");
                player.maintain(Duration::ZERO);
            }
        }
        assert!(player.decoder_generations() <= player.options.mixer.max_sounds);
    }
    player.close();
    until(|| {
        player.maintain(Duration::ZERO);
        !player.cleanup_pending()
    });
    assert_eq!(player.video_usage().bytes, 0);
    assert!(player.poll_after().is_none());
}
