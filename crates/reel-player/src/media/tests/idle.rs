use super::*;
fn state(output: &Output) -> AudioState {
    output.status().state
}
fn expire(player: &mut MediaPlayer) {
    let quiet = player
        .output_idle
        .quiet
        .as_mut()
        .expect("idle grace scheduled");
    quiet.0 = Instant::now() - Duration::from_secs(1);
}
fn drain(player: &mut MediaPlayer, output: &Output) {
    until(|| {
        player.maintain(Duration::ZERO);
        output.render();
        player.reclaim.as_ref().unwrap().status().live_sources == 0
    });
}
#[test]
fn silent_session_and_paused_preparation_keep_native_output_unstarted() {
    let (mut player, output) = rig();
    for _ in 0..8 {
        player.maintain(Duration::ZERO);
        assert_eq!(output.render(), 0.);
    }
    assert_eq!(state(&output), AudioState::Stopped);
    assert_eq!(output.clock_observation().generated_frames, 0);
    let id = player.play_audio(request(true), true).unwrap();
    player.commit_audio(id);
    until(|| {
        player.maintain(Duration::ZERO);
        matches!(player.entries[&id].phase, Phase::Ready(..))
    });
    player.suspend();
    player.resume();
    assert_eq!(state(&output), AudioState::Stopped);
    player.command(id, MediaCommand::Resume).unwrap();
    until(|| {
        player.maintain(Duration::ZERO);
        output.render() > 0.1
    });
    assert_eq!(state(&output), AudioState::Running);
    player.cancel(id);
    drain(&mut player, &output);
}
#[test]
fn consecutive_effects_finish_resampler_tails_and_reuse_the_stream_before_idle_sleep() {
    let (mut player, output) = rig();
    let mut epoch = None;
    for _ in 0..3 {
        let id = player.play_audio(request(false), false).unwrap();
        player.commit_audio(id);
        let mut peak = 0_f32;
        until(|| {
            player.maintain(Duration::ZERO);
            peak = peak.max(output.render());
            player
                .take_result(id)
                .is_some_and(|event| event.result == MediaResult::Completed)
        });
        assert!(peak > 0.1);
        let next = output.status().output_epoch;
        assert_eq!(
            *epoch.get_or_insert(next),
            next,
            "burst reopened the native stream"
        );
        assert_eq!(state(&output), AudioState::Running);
    }
    drain(&mut player, &output);
    player.maintain(Duration::ZERO);
    assert!(
        player.poll_after().is_some(),
        "idle deadline must keep host maintenance scheduled"
    );
    // No native Sound is stranded on a stopped/unused queue at the sleep point.
    assert_eq!(player.reclaim.as_ref().unwrap().status().live_sources, 0);
    expire(&mut player);
    until(|| {
        output.render();
        player.maintain(Duration::ZERO);
        state(&output) == AudioState::Suspended
    });
    let before = output.clock_observation().generated_frames;
    assert_eq!(output.render(), 0.);
    assert_eq!(output.clock_observation().generated_frames, before);
    let id = player.play_audio(request(false), false).unwrap();
    player.commit_audio(id);
    until(|| {
        player.maintain(Duration::ZERO);
        output.render() > 0.1
    });
    assert_eq!(output.status().output_epoch, epoch.unwrap());
    player.cancel(id);
    drain(&mut player, &output);
}
#[test]
fn paused_sleep_waits_past_an_inflight_callback_and_new_play_rearms_after_system_resume() {
    let (mut player, output) = rig();
    let id = player.play_audio(request(true), false).unwrap();
    player.commit_audio(id);
    until(|| {
        player.maintain(Duration::ZERO);
        output.render() > 0.1
    });
    player.command(id, MediaCommand::Pause).unwrap();
    expire(&mut player);
    player.maintain(Duration::ZERO);
    assert_eq!(
        state(&output),
        AudioState::Running,
        "no post-command callback yet"
    );
    output.render();
    player.maintain(Duration::ZERO);
    let position = player.position(id).unwrap();
    assert_eq!(
        state(&output),
        AudioState::Running,
        "one short callback can still precede command observation"
    );
    until(|| {
        assert_eq!(output.render(), 0.);
        player.maintain(Duration::ZERO);
        state(&output) == AudioState::Suspended
    });
    assert_eq!(player.position(id), Some(position));
    let epoch = output.status().output_epoch;
    player.suspend();
    player.resume();
    assert_eq!(
        state(&output),
        AudioState::Suspended,
        "system resume preserves all-paused idle state"
    );
    let next = player.play_audio(request(false), false).unwrap();
    player.commit_audio(next);
    until(|| {
        player.maintain(Duration::ZERO);
        output.render() > 0.1
    });
    assert_eq!(player.position(id), Some(position));
    assert_eq!(output.status().output_epoch, epoch);
    player.cancel(next);
    player.command(id, MediaCommand::Resume).unwrap();
    until(|| output.render() > 0.1);
    player.cancel(id);
    drain(&mut player, &output);
}
#[test]
fn cancelling_held_audio_still_processes_its_stop_and_retires() {
    let (mut player, output) = rig();
    let id = player.play_audio(request(false), false).unwrap();
    // An uncommitted initial batch is accepted held, without opening AudioOut.
    until(|| {
        player.maintain(Duration::ZERO);
        matches!(player.entries[&id].phase, Phase::Playing(_))
    });
    assert_eq!(state(&output), AudioState::Stopped);
    player.cancel(id);
    drain(&mut player, &output);
    assert_eq!(player.pcm_usage().bytes, 0);
}
struct RejectStart(Output);
impl AudioOutput for RejectStart {
    fn capabilities(&self) -> AudioCaps {
        self.0.capabilities()
    }
    fn configure(&mut self, request: reel_media::AudioRequest) -> Result<AudioFormat, AudioError> {
        self.0.configure(request)
    }
    fn start(&mut self, request: PcmFill) -> Result<(), reel_core::result::Rejected<PcmFill>> {
        Err(reel_core::result::Rejected {
            request,
            reason: reel_core::result::RejectReason::new(
                reel_core::result::RejectCode::Unavailable,
                "delayed native build rejected",
            ),
        })
    }
    fn suspend(&mut self) -> Result<(), AudioError> {
        self.0.suspend()
    }
    fn resume(&mut self) -> Result<(), AudioError> {
        self.0.resume()
    }
    fn status(&self) -> AudioStatus {
        self.0.status()
    }
    fn clock_observation(&self) -> AudioClockObservation {
        self.0.clock_observation()
    }
    fn stop(&mut self) -> Result<(), AudioError> {
        self.0.stop()
    }
}
#[test]
fn delayed_native_start_rejection_settles_accepted_activities_and_releases_renderer() {
    let output = Output(Rc::new(RefCell::new(None)));
    let sink = output.clone();
    let mut player = MediaPlayer::new(
        Assets::directory(AssetOptions {
            root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
            ..Default::default()
        }),
        MediaOptions::default(),
        WakeSignal::new(|| {}),
        move |wake| {
            *sink.0.borrow_mut() = Some(
                OfflineOutput::new(
                    AudioDeviceConfig {
                        sample_rate_hz: 48000,
                        sample_format: SampleFormat::F32,
                        layout: ChannelLayout::Stereo,
                    },
                    wake,
                )
                .unwrap(),
            );
            Ok(Box::new(RejectStart(sink)))
        },
    );
    assert!(
        player.failure.is_none(),
        "configuration succeeds without building a stream"
    );
    let id = player.play_audio(request(false), false).unwrap();
    player.commit_audio(id);
    let mut result = None;
    until(|| {
        player.maintain(Duration::ZERO);
        result = player.take_result(id);
        result.is_some()
    });
    assert!(matches!(result.unwrap().result, MediaResult::Failed(_)));
    assert_eq!(
        player.failure.as_deref(),
        Some("delayed native build rejected")
    );
    assert_eq!(
        output.clock_observation().generated_frames,
        0,
        "rejected fill stays uncalled"
    );
    player.close();
    until(|| {
        player.maintain(Duration::ZERO);
        !player.cleanup_pending()
    });
    assert_eq!(player.pcm_usage().bytes, 0);
}
#[test]
fn system_suspend_preserves_a_running_fade_out_instead_of_business_stopping_it() {
    let (mut player, output) = rig();
    let id = player.play_audio(request(true), false).unwrap();
    player.commit_audio(id);
    until(|| {
        player.maintain(Duration::ZERO);
        output.render() > 0.1
    });
    player
        .command(id, MediaCommand::FadeOut(Duration::from_millis(100)))
        .unwrap();
    player.suspend();
    for _ in 0..10 {
        player.maintain(Duration::ZERO);
        assert_eq!(output.render(), 0.);
    }
    assert!(
        player.take_result(id).is_none(),
        "system suspension must not settle the fade-out"
    );
    assert!(!player.audio_snapshot(id).unwrap().paused);
    player.resume();
    let mut result = None;
    until(|| {
        output.render();
        player.maintain(Duration::ZERO);
        result = player.take_result(id);
        result.is_some()
    });
    assert_eq!(result.unwrap().result, MediaResult::Stopped);
    drain(&mut player, &output);
}
