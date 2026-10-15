use super::*;
use crate::{
    aside_session::{AsideSessionUi, StoryState},
    assets::AssetOptions,
    story_runtime::StoryRuntime,
};
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{SliceBudget, VmLimits, source},
};
use std::{cell::RefCell, rc::Rc, time::Instant};

#[derive(Clone)]
struct Output(Rc<RefCell<Option<OfflineOutput>>>);
impl AudioOutput for Output {
    fn capabilities(&self) -> AudioCaps {
        self.0.borrow().as_ref().unwrap().capabilities()
    }
    fn configure(&mut self, r: reel_media::AudioRequest) -> Result<AudioFormat, AudioError> {
        self.0.borrow_mut().as_mut().unwrap().configure(r)
    }
    fn start(&mut self, fill: PcmFill) -> Result<(), reel_core::result::Rejected<PcmFill>> {
        self.0.borrow_mut().as_mut().unwrap().start(fill)
    }
    fn suspend(&mut self) -> Result<(), AudioError> {
        self.0.borrow_mut().as_mut().unwrap().suspend()
    }
    fn resume(&mut self) -> Result<(), AudioError> {
        self.0.borrow_mut().as_mut().unwrap().resume()
    }
    fn status(&self) -> AudioStatus {
        self.0.borrow().as_ref().unwrap().status()
    }
    fn clock_observation(&self) -> AudioClockObservation {
        self.0.borrow().as_ref().unwrap().clock_observation()
    }
    fn stop(&mut self) -> Result<(), AudioError> {
        self.0.borrow_mut().as_mut().unwrap().stop()
    }
}
impl Output {
    fn render(&self) -> f32 {
        let mut pcm = [0_f32; 256];
        if let Some(output) = &mut *self.0.borrow_mut() {
            output.render(
                &mut pcm,
                CallbackTiming {
                    now: reel_core::clock::MonoTime(0),
                    latency_frames: None,
                    uncertainty_frames: None,
                },
            );
        }
        pcm.iter().map(|n| n.abs()).fold(0., f32::max)
    }
}
fn rig() -> (AudioPlayer, Output) {
    let output = Output(Rc::new(RefCell::new(None)));
    let sink = output.clone();
    let player = AudioPlayer::new(
        Assets::directory(AssetOptions {
            root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
            ..Default::default()
        }),
        AudioOptions::default(),
        WakeSignal::new(|| {}),
        move |wake| {
            *sink.0.borrow_mut() = Some(
                OfflineOutput::new(
                    AudioDeviceConfig {
                        sample_rate_hz: 48_000,
                        sample_format: SampleFormat::F32,
                        layout: ChannelLayout::Stereo,
                    },
                    wake,
                )
                .map_err(|e| e.to_string())?,
            );
            Ok(Box::new(sink))
        },
    );
    (player, output)
}
fn until(mut step: impl FnMut() -> bool) {
    let start = Instant::now();
    while !step() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "audio progress stalled"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn request(looping: bool) -> AudioRequest {
    AudioRequest {
        source: "assets/chime.wav".into(),
        looping,
        volume: 1.,
        group: AudioGroup::Effects,
        pan: 0.,
        fade_in: Duration::ZERO,
        start_position: Duration::ZERO,
    }
}

#[test]
fn restored_music_starts_at_saved_content_position_before_output_resumes() {
    let (mut first, output) = rig();
    let mut music = request(true);
    music.group = AudioGroup::Music;
    let id = first.play(music).unwrap();
    until(|| {
        first.maintain();
        output.render();
        first.snapshot(id).unwrap().request.start_position > Duration::from_millis(15)
    });
    first.suspend();
    let saved = first.snapshot(id).unwrap();
    assert!(saved.result.is_none());
    let position = saved.request.start_position;
    let (mut restored, restored_output) = rig();
    restored.suspend();
    let id = restored.play(saved.request).unwrap();
    until(|| {
        restored.maintain();
        matches!(restored.entries[&id].phase, Phase::Playing(_))
    });
    assert_eq!(restored_output.render(), 0.);
    restored.resume();
    until(|| {
        restored.maintain();
        restored_output.render();
        restored.snapshot(id).unwrap().request.start_position > position
    });
    first.close();
    restored.close();
    until(|| {
        first.maintain();
        restored.maintain();
        !first.cleanup_pending() && !restored.cleanup_pending()
    });
}

#[test]
fn author_audio_source_and_bytecode_share_real_decode_playback_waits_and_retirement() {
    let text = r#"fn main() {
        audio.group_volume("music", 0.5);
        audio.group_volume("master", 1, duration: 0.01);
        let early = audio.play("assets/chime.wav", group: "ui", looping: true, pan: -1, fade_in: 0.01);
        audio.stop(early, fade: 0.1);
        if await early != "Stopped" || await early != "Stopped" { throw "early stop"; }
        let crossfade = audio.play("assets/chime.wav", group: "music", looping: true, fade_in: 0.01);
        let sound = audio.play("assets/chime.wav", group: "voice");
        audio.pan(sound, 1, duration: 0.01);
        audio.volume(sound, 0.5, duration: 0.01);
        if await sound != "Completed" || await sound != "Completed" { throw "completion"; }
        audio.stop(crossfade, fade: 0.01);
        if await crossfade != "Stopped" { throw "fade completion"; }
        let mut invalid = 0;
        try { audio.group_volume("typo", 1); } catch error { invalid += 1; }
        try { audio.play("assets/chime.wav", pan: 2); } catch error { invalid += 1; }
        try { audio.volume(crossfade, 1, duration: -1); } catch error { invalid += 1; }
        if invalid != 3 { throw "audio validation"; }
        let music = audio.play("assets/chime.wav", looping: true);
        pause(music); audio.volume(music, 0); resume(music);
        stop(music);
        if await music != "Stopped" { throw "loop stop"; }
        let node = text("owned sound"); stage.replace(node);
        let owned = audio.play("assets/chime.wav", looping: true, owner: node);
        stage.replace(text("replacement"));
        let mut cancelled = false;
        try { await owned; } catch error { cancelled = true; }
        if !cancelled { throw "owner cancellation"; }
        let mut failed = false;
        try { await audio.play("assets/not-present.wav"); } catch error { failed = true; }
        if !failed { throw "missing resource"; }
    }"#;
    for binary in [false, true] {
        let (backend, output) = rig();
        let pcm = backend.pcm.clone();
        let mut story = source::compile_story(text, "audio_story", 1, "main").unwrap();
        if binary {
            let bytes =
                aside_vm::bytecode::encode(story.program.package(), Default::default()).unwrap();
            story.program =
                aside_vm::bytecode::VerifiedProgram::decode(&bytes, Default::default()).unwrap();
        }
        let driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap()
        .with_audio(Box::new(backend))
        .unwrap();
        let mut runtime = StoryRuntime::new(driver);
        let mut peak = 0_f32;
        until(|| {
            peak = peak.max(output.render());
            runtime.tick(SliceBudget::default(), || 0).unwrap();
            assert!(
                matches!(
                    runtime.driver().state(),
                    StoryState::Running | StoryState::Finished
                ),
                "state: {:?}, fault: {:?}",
                runtime.driver().state(),
                runtime.driver().fault()
            );
            runtime.driver().state() == StoryState::Finished && !runtime.needs_tick()
        });
        assert!(peak > 0.1, "real decoded PCM must reach output");
        assert_eq!(pcm.usage().bytes, 0);
        assert!(
            !runtime.needs_tick(),
            "retired audio must not keep the story polling"
        );
    }
}

#[test]
fn real_audio_controls_freeze_position_mute_loop_and_reclaim_on_output_failure() {
    let (mut player, output) = rig();
    let id = player.play(request(true)).unwrap();
    until(|| {
        player.maintain();
        output.render() > 0.1
    });
    player.command(id, AudioCommand::Pause).unwrap();
    // Flush the renderer's already-generated block; paused playback remains live.
    for _ in 0..4 {
        output.render();
    }
    assert_eq!(output.render(), 0.);
    assert!(player.take_result(id).is_none());
    player.command(id, AudioCommand::Resume).unwrap();
    until(|| output.render() > 0.1);
    player
        .command(
            id,
            AudioCommand::Volume {
                value: 0.,
                duration: Duration::ZERO,
            },
        )
        .unwrap();
    for _ in 0..4 {
        output.render();
    }
    assert_eq!(output.render(), 0.);
    player
        .command(
            id,
            AudioCommand::Volume {
                value: 1.,
                duration: Duration::ZERO,
            },
        )
        .unwrap();
    until(|| output.render() > 0.1);
    player.suspend();
    let before = output.clock_observation().generated_frames;
    for _ in 0..20 {
        assert_eq!(output.render(), 0.);
        player.maintain();
    }
    assert_eq!(output.clock_observation().generated_frames, before);
    player.resume();
    until(|| output.render() > 0.1);
    output.0.borrow_mut().as_mut().unwrap().fail_device();
    let result = loop {
        player.maintain();
        if let Some(result) = player.take_result(id) {
            break result;
        }
    };
    assert!(matches!(result, AudioResult::Failed(_)));
    assert!(player.take_result(id).is_none());
    player.close();
    until(|| {
        player.maintain();
        !player.cleanup_pending()
    });
    assert_eq!(player.pcm_usage().bytes, 0);
    assert!(player.poll_after().is_none());
}

#[test]
fn loading_cancellation_and_pcm_budget_failure_leave_no_resident_requests() {
    let (mut player, output) = rig();
    let id = player.play(request(false)).unwrap();
    player.cancel(id);
    player.close();
    until(|| {
        player.maintain();
        !player.cleanup_pending()
    });
    assert_eq!(player.pcm_usage().bytes, 0);
    assert_eq!(output.render(), 0.);
    let (mut player, _) = rig();
    player.pcm = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 8,
            max_bytes: 64,
        },
    );
    let id = player.play(request(false)).unwrap();
    let mut result = None;
    until(|| {
        player.maintain();
        result = player.take_result(id);
        result.is_some()
    });
    assert!(matches!(result, Some(AudioResult::Failed(_))));
    player.close();
    until(|| {
        player.maintain();
        !player.cleanup_pending()
    });
    assert_eq!(player.pcm_usage().bytes, 0);
}
