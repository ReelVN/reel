use super::*;
use reel_core::audio::AudioRate;

fn rate_rig(streaming: bool) -> (MediaPlayer, Output) {
    rig_at(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
        MediaOptions {
            static_pcm_threshold_bytes: if streaming { 0 } else { 8 << 20 },
            stream_window_frames: 16_384,
            ..Default::default()
        },
    )
}
fn close(player: &mut MediaPlayer) {
    player.close();
    until(|| {
        player.maintain(Duration::ZERO);
        !player.cleanup_pending()
    });
    assert_eq!(player.pcm_usage().bytes, 0);
    assert_eq!(player.descriptors.usage().bytes, 0);
}

#[test]
fn audio_rate_scales_real_output_duration_for_static_and_streamed_sources() {
    for streaming in [false, true] {
        for rate in [0.25, 0.5, 1., 2., 4.] {
            let (mut player, output) = rate_rig(streaming);
            let mut request = request(false);
            request.rate = AudioRate::constant(rate);
            request.end_position = Some(Duration::from_millis(160));
            let id = player.play_audio(request, false).unwrap();
            player.commit_audio(id);
            until(|| {
                player.maintain(Duration::ZERO);
                player.entries[&id].started
            });
            let mut blocks = 0;
            let mut active_blocks = 0;
            let mut previous = Duration::ZERO;
            let mut peak = 0_f32;
            until(|| {
                peak = peak.max(output.render());
                blocks += 1;
                player.maintain(Duration::ZERO);
                let position = player.position(id).unwrap();
                active_blocks += usize::from(position > previous);
                previous = position;
                matches!(player.entries[&id].phase, Phase::Done(_))
            });
            let elapsed = active_blocks as f64 * 128. / 48_000.;
            assert!(
                (elapsed - 0.16 / rate).abs() < 0.008,
                "streaming={streaming}, rate={rate}, output={elapsed}"
            );
            assert!(peak > 0.1);
            let terminal = player.take_result(id).unwrap();
            assert_eq!(terminal.result, MediaResult::Completed);
            assert_eq!(terminal.position, Duration::from_millis(160));
            eprintln!(
                "audio rate: stream={streaming}, rate={rate}, source=0.16s, consuming output={elapsed:.6}s, total blocks={blocks}, peak={peak}"
            );
            close(&mut player);
        }
    }
}

#[test]
fn audio_rate_ramp_survives_pause_restore_seek_queue_and_cancellation() {
    for streaming in [false, true] {
        let (mut player, output) = rate_rig(streaming);
        let mut request = request(true);
        request.rate = AudioRate::constant(0.5);
        let id = player.play_audio(request, false).unwrap();
        player
            .command(
                id,
                MediaCommand::Rate {
                    value: 2.,
                    duration: Duration::from_secs(1),
                },
            )
            .unwrap();
        player.commit_audio(id);
        until(|| {
            player.maintain(Duration::ZERO);
            output.render();
            player.audio_snapshot(id).unwrap().request.rate.value > 0.8
        });
        player.command(id, MediaCommand::Pause).unwrap();
        for _ in 0..4 {
            output.render();
        }
        let saved = player.audio_snapshot(id).unwrap();
        assert!(saved.request.rate.remaining > Duration::from_millis(700));
        for _ in 0..32 {
            assert_eq!(output.render(), 0.);
        }
        let paused = player.audio_snapshot(id).unwrap();
        assert_eq!(paused.request.rate, saved.request.rate);
        assert_eq!(paused.request.start_position, saved.request.start_position);
        let (mut restored, sink) = rate_rig(streaming);
        let next = restored
            .play_audio(saved.request.clone(), saved.paused)
            .unwrap();
        restored.commit_audio(next);
        until(|| {
            restored.maintain(Duration::ZERO);
            matches!(restored.entries[&next].phase, Phase::Ready(..))
        });
        assert_eq!(
            restored.audio_snapshot(next).unwrap().request.rate,
            saved.request.rate
        );
        restored.command(next, MediaCommand::Resume).unwrap();
        until(|| {
            restored.maintain(Duration::ZERO);
            sink.render();
            restored.audio_snapshot(next).unwrap().request.rate.value == 2.
        });
        restored
            .command(next, MediaCommand::Seek(Duration::from_millis(80)))
            .unwrap();
        assert_eq!(restored.position(next), Some(Duration::from_millis(80)));
        assert_eq!(
            restored.audio_snapshot(next).unwrap().request.rate,
            AudioRate::constant(2.)
        );
        restored
            .queue_audio(
                next,
                AudioQueueItem {
                    source: "assets/chime.wav".into(),
                    start_position: Duration::ZERO,
                    end_position: Some(Duration::from_millis(180)),
                    loop_region: None,
                },
            )
            .unwrap();
        until(|| {
            restored.maintain(Duration::ZERO);
            sink.render();
            let saved = restored.audio_snapshot(next).unwrap();
            assert_eq!(saved.request.rate.value, 2.);
            matches!(restored.entries[&next].phase, Phase::Done(_))
        });
        assert_eq!(
            restored.take_result(next).unwrap().position,
            Duration::from_millis(180)
        );
        player.cancel(id);
        assert!(player.take_result(id).is_none());
        assert!(
            player.audio_snapshot(id).is_err(),
            "owner cancellation retained the transport"
        );
        close(&mut player);
        close(&mut restored);
    }
}

#[test]
fn audio_rate_rejects_unbounded_values_and_preserves_pending_commands() {
    let (mut player, output) = rig();
    for rate in [0., -1., 0.24, 4.01, f64::NAN, f64::INFINITY] {
        let mut request = request(false);
        request.rate = AudioRate::constant(rate);
        assert!(player.play_audio(request, false).is_err());
    }
    let id = player.play_audio(request(true), true).unwrap();
    player.commit_audio(id);
    player
        .command(
            id,
            MediaCommand::Rate {
                value: 0.5,
                duration: Duration::ZERO,
            },
        )
        .unwrap();
    player
        .command(
            id,
            MediaCommand::Rate {
                value: 1.5,
                duration: Duration::from_secs(1),
            },
        )
        .unwrap();
    assert!(
        player
            .command(
                id,
                MediaCommand::Rate {
                    value: f64::NAN,
                    duration: Duration::ZERO
                }
            )
            .is_err()
    );
    let state = player.audio_snapshot(id).unwrap();
    assert_eq!(
        state.request.rate,
        AudioRate {
            value: 0.5,
            target: 1.5,
            remaining: Duration::from_secs(1)
        }
    );
    assert_eq!(output.render(), 0.);
    player.cancel(id);
    assert!(player.take_result(id).is_none());
    assert!(player.audio_snapshot(id).is_err());
    close(&mut player);
}
