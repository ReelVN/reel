use super::*;
use crate::{
    aside_session::{AsideSessionUi, StoryState},
    assets::AssetOptions,
    story_runtime::StoryRuntime,
};
use reel_core::budget::OwnedBytes;
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{SliceBudget, VmLimits, source},
    audio::AudioLoopRegion,
};
use std::{cell::RefCell, rc::Rc, time::Instant};

#[derive(Clone)]
pub(super) struct Output(Rc<RefCell<Option<OfflineOutput>>>);
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
    pub(super) fn render(&self) -> f32 {
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
fn rig() -> (MediaPlayer, Output) {
    rig_at(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
        MediaOptions::default(),
    )
}
pub(super) fn rig_at(root: std::path::PathBuf, options: MediaOptions) -> (MediaPlayer, Output) {
    rig_assets(
        Assets::directory(AssetOptions {
            root,
            ..Default::default()
        }),
        options,
    )
}
fn rig_assets(assets: Assets, options: MediaOptions) -> (MediaPlayer, Output) {
    let output = Output(Rc::new(RefCell::new(None)));
    let sink = output.clone();
    let player = MediaPlayer::new(assets, options, WakeSignal::new(|| {}), move |wake| {
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
    });
    (player, output)
}
#[track_caller]
pub(super) fn until(mut step: impl FnMut() -> bool) {
    let start = Instant::now();
    while !step() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "audio progress stalled"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
pub(super) fn request(looping: bool) -> AudioRequest {
    AudioRequest {
        source: "assets/chime.wav".into(),
        loop_region: looping.then(AudioLoopRegion::default),
        volume: 1.,
        group: AudioGroup::Effects,
        pan: 0.,
        fade_in: Duration::ZERO,
        start_position: Duration::ZERO,
        end_position: None,
    }
}

#[test]
fn initially_paused_audio_prepares_seeks_and_resumes_before_or_after_decode() {
    for resume_early in [false, true] {
        let (mut player, output) = rig();
        let id = player.play_audio(request(false), true).unwrap();
        player.commit_audio(id);
        assert!(player.entries[&id].paused);
        player
            .command(id, MediaCommand::Seek(Duration::from_millis(50)))
            .unwrap();
        if resume_early {
            player.command(id, MediaCommand::Resume).unwrap();
        } else {
            until(|| {
                player.maintain(Duration::from_secs(1));
                assert_eq!(output.render(), 0.);
                assert_eq!(player.position(id), Some(Duration::from_millis(50)));
                matches!(player.entries[&id].phase, Phase::Ready(..))
            });
            assert!(player.entries[&id].paused);
            for _ in 0..120 {
                player.maintain(Duration::from_millis(16));
                assert_eq!(output.render(), 0.);
            }
            assert_eq!(player.position(id), Some(Duration::from_millis(50)));
            player.command(id, MediaCommand::Resume).unwrap();
        }
        until(|| {
            player.maintain(Duration::from_millis(1));
            output.render() > 0.1
        });
        player.command(id, MediaCommand::Stop).unwrap();
        until(|| {
            player.maintain(Duration::ZERO);
            output.render();
            player.take_result(id).is_some_and(|result| {
                assert_eq!(result.result, MediaResult::Stopped);
                true
            })
        });
        player.close();
        until(|| {
            player.maintain(Duration::ZERO);
            !player.cleanup_pending()
        });
    }
}

#[test]
fn ready_intro_waits_for_explicit_batch_and_blocked_successor() {
    use reel_core::{
        identity::{AssetKey, ContentVersion, IoId},
        io::{AssetIo, DescribeRequest, IoCaps, IoEvent, ReadRequest, SaveWrite},
        result::Rejected,
        test_support::{MemoryAsset, MemoryAssetIo},
    };
    struct Io(Rc<RefCell<MemoryAssetIo>>);
    impl AssetIo for Io {
        fn capabilities(&self) -> IoCaps {
            self.0.borrow().capabilities()
        }
        fn describe(
            &mut self,
            request: DescribeRequest,
        ) -> Result<IoId, Rejected<DescribeRequest>> {
            self.0.borrow_mut().describe(request)
        }
        fn read(&mut self, request: ReadRequest) -> Result<IoId, Rejected<ReadRequest>> {
            self.0.borrow_mut().read(request)
        }
        fn replace_save(&mut self, request: SaveWrite) -> Result<IoId, Rejected<SaveWrite>> {
            self.0.borrow_mut().replace_save(request)
        }
        fn cancel(&mut self, id: IoId) {
            self.0.borrow_mut().cancel(id);
        }
        fn poll(&mut self, out: &mut Vec<IoEvent>, limit: usize) {
            self.0.borrow_mut().poll(out, limit);
        }
    }
    let mut control = None;
    let assets = Assets::with_adapters(AssetOptions::default(), |wake| {
        let limits = QueueLimits {
            max_requests: 32,
            max_bytes: 8 << 20,
        };
        let pool = BudgetPool::new(BudgetCategory::CpuInput, limits);
        let mut io = MemoryAssetIo::new(limits, limits, wake.clone());
        let bytes = include_bytes!("../../../../examples/assets/chime.wav");
        for source in ["assets/chime.wav", "slow.wav"] {
            io.insert_asset(
                ResourceVersion {
                    key: AssetKey(source.into()),
                    version: ContentVersion([0; 32]),
                },
                MemoryAsset {
                    bytes: OwnedBytes::new(
                        bytes.to_vec(),
                        pool.reserve(bytes.len() as u64).unwrap(),
                    )
                    .unwrap(),
                    media_type: None,
                },
            )
            .unwrap();
        }
        let io = Rc::new(RefCell::new(io));
        control = Some(io.clone());
        (
            Box::new(Io(io)),
            Box::new(crate::assets::ImageExecutor::new(limits, wake)),
        )
    });
    let control = control.unwrap();
    let (mut player, output) = rig_assets(assets, MediaOptions::default());
    // Native play returns; allow all preparation and PCM callbacks before queue.
    let id = player.play_audio(request(false), false).unwrap();
    until(|| {
        control.borrow_mut().complete_next_read();
        player.maintain(Duration::ZERO);
        assert_eq!(output.render(), 0.0);
        matches!(player.entries[&id].phase, Phase::Playing(_))
    });
    assert!(!player.entries[&id].started);
    player
        .queue_audio(
            id,
            AudioQueueItem {
                source: "slow.wav".into(),
                start_position: Duration::ZERO,
                end_position: None,
                loop_region: None,
            },
        )
        .unwrap();
    // Native queue returns; repeated polling cannot close the synchronous batch.
    for _ in 0..32 {
        player.maintain(Duration::ZERO);
        assert_eq!(output.render(), 0.0);
    }
    assert_eq!(player.entries[&id].initial_batch, None);
    // A real task wait commits. The second read is still deliberately unresolved.
    player.commit_audio(id);
    assert_eq!(player.entries[&id].initial_batch, Some(1));
    for _ in 0..32 {
        player.maintain(Duration::ZERO);
        assert_eq!(output.render(), 0.0);
    }
    assert!(!player.entries[&id].started);
    assert_eq!(player.position(id), Some(Duration::ZERO));
    until(|| {
        control.borrow_mut().complete_next_read();
        player.maintain(Duration::ZERO);
        player.entries[&id].started
    });
    assert!(matches!(
        player.entries[&id].queue[0].phase,
        Phase::Playing(_)
    ));
    until(|| output.render() > 0.1);
    player.close();
    until(|| {
        player.maintain(Duration::ZERO);
        !player.cleanup_pending()
    });
    assert_eq!(player.pcm_usage().bytes, 0);
}

#[test]
fn queue_preserves_current_item_remaining_order_pause_and_content_restore() {
    queue_restore(MediaOptions::default());
}

#[test]
fn streaming_queue_preserves_current_item_remaining_order_pause_and_content_restore() {
    queue_restore(MediaOptions {
        static_pcm_threshold_bytes: 0,
        stream_window_frames: 256,
        ..Default::default()
    });
}

#[test]
#[ignore = "requires REEL_LONG_AUDIO pointing to an original full long track"]
fn external_long_audio_streams_seeks_restores_and_retires_with_bounded_pcm() {
    let path =
        std::path::PathBuf::from(std::env::var_os("REEL_LONG_AUDIO").expect("REEL_LONG_AUDIO"));
    let source = path.file_name().unwrap().to_str().unwrap().to_owned();
    let root = path.parent().unwrap().to_path_buf();
    let options = MediaOptions {
        pcm_bytes: 2 << 20,
        stream_window_frames: 4096,
        ..Default::default()
    };
    let (mut first, output) = rig_at(root.clone(), options.clone());
    let mut music = request(true);
    music.source = source.clone();
    music.group = AudioGroup::Music;
    let id = first.play_audio(music, false).unwrap();
    first.commit_audio(id);
    let mut peak = 0f32;
    until(|| {
        first.maintain(Duration::ZERO);
        peak = peak.max(output.render());
        if let Phase::Done(result) = &first.entries[&id].phase {
            panic!("long track ended during startup: {result:?}");
        }
        peak > 0.
            && first
                .position(id)
                .is_some_and(|p| p > Duration::from_millis(100))
    });
    first
        .command(id, MediaCommand::Seek(Duration::from_secs(30)))
        .unwrap();
    assert_eq!(first.position(id), Some(Duration::from_secs(30)));
    until(|| {
        first.maintain(Duration::ZERO);
        output.render();
        if let Phase::Done(result) = &first.entries[&id].phase {
            panic!("long track failed seeking: {result:?}");
        }
        first
            .position(id)
            .is_some_and(|p| p > Duration::from_millis(30_050))
    });
    first.command(id, MediaCommand::Pause).unwrap();
    output.render();
    first.maintain(Duration::ZERO);
    let saved = first.audio_snapshot(id).unwrap();
    let position = saved.request.start_position;
    first.close();
    until(|| {
        first.maintain(Duration::ZERO);
        !first.cleanup_pending()
    });
    assert_eq!(first.pcm_usage().bytes, 0);
    let first_peak = first.pcm_usage().peak_bytes;
    let (mut restored, restored_output) = rig_at(root.clone(), options.clone());
    let second = restored.play_audio(saved.request, true).unwrap();
    restored.commit_audio(second);
    until(|| {
        restored.maintain(Duration::ZERO);
        matches!(restored.entries[&second].phase, Phase::Ready(_, _))
    });
    assert_eq!(restored.position(second), Some(position));
    assert_eq!(restored_output.render(), 0.);
    restored.command(second, MediaCommand::Resume).unwrap();
    until(|| {
        restored.maintain(Duration::ZERO);
        restored_output.render();
        restored
            .position(second)
            .is_some_and(|p| p > position + Duration::from_millis(20))
    });
    restored.command(second, MediaCommand::Stop).unwrap();
    restored.close();
    until(|| {
        restored.maintain(Duration::ZERO);
        !restored.cleanup_pending()
    });
    assert_eq!(restored.pcm_usage().bytes, 0);
    let (mut looping, loop_output) = rig_at(root, options);
    let mut loop_request = request(true);
    loop_request.source = source;
    loop_request.loop_region = Some(AudioLoopRegion {
        start: Duration::from_millis(100),
        end: Some(Duration::from_millis(160)),
    });
    let loop_id = looping.play_audio(loop_request, false).unwrap();
    looping.commit_audio(loop_id);
    let mut previous = Duration::ZERO;
    let mut wraps = 0;
    until(|| {
        looping.maintain(Duration::ZERO);
        loop_output.render();
        if let Phase::Done(result) = &looping.entries[&loop_id].phase {
            panic!("long track loop failed: {result:?}");
        }
        let position = looping.position(loop_id).unwrap();
        if position < previous {
            wraps += 1;
        }
        previous = position;
        wraps >= 3
    });
    looping.close();
    until(|| {
        looping.maintain(Duration::ZERO);
        !looping.cleanup_pending()
    });
    assert_eq!(looping.pcm_usage().bytes, 0);
    eprintln!(
        "original long track: first_peak_pcm={first_peak}, restored_peak_pcm={}, final_pcm=0, audible_peak={peak}, restored_position={position:?}",
        restored.pcm_usage().peak_bytes
    );
}

fn queue_restore(options: MediaOptions) {
    let ms = Duration::from_millis;
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let (mut player, output) = rig_at(root.clone(), options.clone());
    let mut intro = request(false);
    intro.group = AudioGroup::Voice;
    intro.start_position = ms(200);
    intro.end_position = Some(ms(220));
    let id = player.play_audio(intro, false).unwrap();
    player
        .queue_audio(
            id,
            AudioQueueItem {
                source: "assets/chime.wav".into(),
                start_position: ms(20),
                end_position: Some(ms(100)),
                loop_region: Some(AudioLoopRegion {
                    start: ms(40),
                    end: Some(ms(90)),
                }),
            },
        )
        .unwrap();
    player
        .queue_audio(
            id,
            AudioQueueItem {
                source: "assets/chime.wav".into(),
                start_position: ms(120),
                end_position: Some(ms(180)),
                loop_region: None,
            },
        )
        .unwrap();
    player.commit_audio(id);
    until(|| {
        player.maintain(Duration::ZERO);
        player.entries[&id].started
    });
    assert!(
        player.entries[&id]
            .queue
            .iter()
            .all(|segment| matches!(segment.phase, Phase::Playing(_))),
        "initial batch is prepared before first PCM"
    );
    until(|| {
        output.render();
        player.maintain(Duration::ZERO);
        player.entries[&id].item == 1
    });
    player.command(id, MediaCommand::Pause).unwrap();
    for _ in 0..4 {
        output.render();
    }
    let saved = player.audio_snapshot(id).unwrap();
    assert!(saved.paused);
    assert_eq!(saved.request.group, AudioGroup::Voice);
    assert!(saved.request.loop_region.is_some());
    assert_eq!(saved.request.end_position, Some(ms(100)));
    assert!((ms(20)..ms(90)).contains(&saved.request.start_position));
    assert_eq!(saved.queue.len(), 1);
    assert_eq!(saved.queue[0].start_position, ms(120));
    assert_eq!(saved.queue[0].end_position, Some(ms(180)));
    let position = saved.request.start_position;
    let (mut restored, second_output) = rig_at(root, options);
    let second = restored.play_audio(saved.request, false).unwrap();
    for item in saved.queue {
        restored.queue_audio(second, item).unwrap();
    }
    restored.commit_audio(second);
    restored.command(second, MediaCommand::Pause).unwrap();
    until(|| {
        restored.maintain(Duration::ZERO);
        matches!(restored.entries[&second].phase, Phase::Ready(_, _))
    });
    assert_eq!(second_output.render(), 0.);
    assert_eq!(restored.position(second), Some(position));
    restored.command(second, MediaCommand::Resume).unwrap();
    let mut peak = 0_f32;
    until(|| {
        restored.maintain(Duration::ZERO);
        peak = peak.max(second_output.render());
        matches!(restored.entries[&second].phase, Phase::Done(_))
    });
    assert!(peak > 0.1);
    let done = restored.audio_snapshot(second).unwrap();
    assert!(done.queue.is_empty());
    assert_eq!(done.request.loop_region, None);
    assert_eq!(done.request.start_position, ms(180));
    assert_eq!(done.request.end_position, Some(ms(180)));
    assert_eq!(done.result, Some(MediaResult::Completed));
    assert!(
        restored
            .queue_audio(
                second,
                AudioQueueItem {
                    source: "assets/chime.wav".into(),
                    start_position: Duration::ZERO,
                    end_position: None,
                    loop_region: None
                }
            )
            .is_err()
    );
    player.close();
    restored.close();
    until(|| {
        player.maintain(Duration::ZERO);
        restored.maintain(Duration::ZERO);
        !player.cleanup_pending() && !restored.cleanup_pending()
    });
    assert_eq!(player.pcm_usage().bytes + restored.pcm_usage().bytes, 0);
    assert_eq!(
        player.descriptors.usage().bytes + restored.descriptors.usage().bytes,
        0
    );
}

#[test]
fn queue_capacity_rejection_keeps_original_and_missing_successor_fails_it_once() {
    let (mut player, output) = rig_at(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
        MediaOptions {
            mixer: MixerLimits {
                max_sounds: 2,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let id = player.play_audio(request(true), false).unwrap();
    player.commit_audio(id);
    let missing = AudioQueueItem {
        source: "assets/missing.wav".into(),
        start_position: Duration::ZERO,
        end_position: None,
        loop_region: None,
    };
    player.queue_audio(id, missing.clone()).unwrap();
    assert!(
        player
            .queue_audio(id, missing)
            .unwrap_err()
            .contains("capacity")
    );
    assert_eq!(player.audio_snapshot(id).unwrap().queue.len(), 1);
    until(|| {
        player.maintain(Duration::ZERO);
        output.render();
        matches!(player.entries[&id].phase, Phase::Done(_))
    });
    assert!(matches!(
        player.take_result(id).unwrap().result,
        MediaResult::Failed(_)
    ));
    assert!(player.take_result(id).is_none());
    player.close();
    until(|| {
        player.maintain(Duration::ZERO);
        !player.cleanup_pending()
    });
    assert_eq!(player.pcm_usage().bytes, 0);
    assert_eq!(player.descriptors.usage().bytes, 0);
}

#[test]
fn restored_music_starts_at_saved_content_position_before_output_resumes() {
    let (mut first, output) = rig();
    let mut music = request(true);
    music.group = AudioGroup::Music;
    let id = first.play_audio(music, false).unwrap();
    first.commit_audio(id);
    until(|| {
        first.maintain(Duration::ZERO);
        output.render();
        first.audio_snapshot(id).unwrap().request.start_position > Duration::from_millis(15)
    });
    first.suspend();
    first
        .command(id, MediaCommand::Seek(Duration::from_millis(49)))
        .unwrap();
    let saved = first.audio_snapshot(id).unwrap();
    assert_eq!(saved.request.start_position, Duration::from_millis(49));
    assert!(saved.result.is_none());
    let position = saved.request.start_position;
    let (mut restored, restored_output) = rig();
    restored.suspend();
    let id = restored.play_audio(saved.request, false).unwrap();
    restored.commit_audio(id);
    until(|| {
        restored.maintain(Duration::ZERO);
        matches!(restored.entries[&id].phase, Phase::Playing(_))
    });
    assert_eq!(restored_output.render(), 0.);
    restored.resume();
    until(|| {
        restored.maintain(Duration::ZERO);
        restored_output.render();
        restored.audio_snapshot(id).unwrap().request.start_position > position
    });
    first.close();
    restored.close();
    until(|| {
        first.maintain(Duration::ZERO);
        restored.maintain(Duration::ZERO);
        !first.cleanup_pending() && !restored.cleanup_pending()
    });
}

#[test]
fn seek_preserves_activity_pause_loop_and_immediate_snapshot_through_decode() {
    let (mut player, output) = rig();
    let ms = Duration::from_millis;
    let mut music = request(true);
    music.group = AudioGroup::Music;
    music.end_position = Some(ms(60));
    music.volume = 0.5;
    music.pan = -0.5;
    music.loop_region = Some(AudioLoopRegion {
        start: ms(20),
        end: None,
    });
    let id = player.play_audio(music, false).unwrap();
    player.commit_audio(id);
    player.command(id, MediaCommand::Pause).unwrap();
    player.command(id, MediaCommand::Seek(ms(10))).unwrap();
    assert_eq!(player.position(id), Some(ms(10)));
    until(|| {
        player.maintain(Duration::ZERO);
        matches!(player.entries[&id].phase, Phase::Queued(_))
    });
    // The queued decode must use this newer request when it reaches the control thread.
    player.command(id, MediaCommand::Seek(ms(95))).unwrap();
    until(|| {
        player.maintain(Duration::ZERO);
        matches!(player.entries[&id].phase, Phase::Ready(..))
    });
    assert_eq!(player.position(id), Some(ms(55)));
    player.command(id, MediaCommand::Seek(ms(5))).unwrap();
    assert_eq!(
        player.position(id),
        Some(ms(5)),
        "seeking back retains the loop intro"
    );
    player.command(id, MediaCommand::Resume).unwrap();
    player.maintain(Duration::ZERO);
    let Phase::Playing(media) = player.entries[&id].phase else {
        panic!("not admitted")
    };
    let residency = player.pcm.usage().bytes;
    assert!(residency > 0);
    assert!(output.render() > 0.);

    player.command(id, MediaCommand::Pause).unwrap();
    player.command(id, MediaCommand::Seek(ms(30))).unwrap();
    let saved = player.audio_snapshot(id).unwrap();
    assert!(saved.paused);
    assert_eq!(saved.request.start_position, ms(30));
    assert_eq!(saved.request.volume, 0.5);
    assert_eq!(saved.request.pan, -0.5);
    assert_eq!(saved.request.loop_region.unwrap().start, ms(20));
    assert_eq!(saved.request.end_position, Some(ms(60)));
    for _ in 0..4 {
        output.render();
    }
    assert_eq!(output.render(), 0.);
    assert_eq!(player.position(id), Some(ms(30)));

    player.suspend();
    for position in 0..1000 {
        player
            .command(id, MediaCommand::Seek(ms(position)))
            .unwrap();
    }
    assert_eq!(player.position(id), Some(ms(39)));
    assert_eq!(
        player.audio_snapshot(id).unwrap().request.start_position,
        ms(39)
    );
    assert!(matches!(player.entries[&id].phase, Phase::Playing(current) if current == media));
    assert_eq!(
        player.pcm.usage().bytes,
        residency,
        "seek must reuse decoded PCM"
    );
    player.resume();
    player.command(id, MediaCommand::Resume).unwrap();
    until(|| {
        output.render();
        player.maintain(Duration::ZERO);
        player.position(id).unwrap() > ms(39)
    });
    assert!(player.take_result(id).is_none());
    player.command(id, MediaCommand::Stop).unwrap();
    player.maintain(Duration::ZERO);
    assert_eq!(player.take_result(id).unwrap().result, MediaResult::Stopped);

    let mut bounded = request(false);
    bounded.start_position = ms(10);
    bounded.end_position = Some(ms(50));
    let ending = player.play_audio(bounded, false).unwrap();
    player.commit_audio(ending);
    until(|| {
        player.maintain(Duration::ZERO);
        matches!(player.entries[&ending].phase, Phase::Playing(_))
    });
    player.command(ending, MediaCommand::Pause).unwrap();
    player
        .command(ending, MediaCommand::Seek(ms(10_000)))
        .unwrap();
    assert_eq!(player.position(ending), Some(ms(50)));
    for _ in 0..4 {
        output.render();
        player.maintain(Duration::ZERO);
    }
    assert!(
        player.take_result(ending).is_none(),
        "paused end waits for resume"
    );
    player.command(ending, MediaCommand::Resume).unwrap();
    until(|| {
        output.render();
        player.maintain(Duration::ZERO);
        matches!(player.entries[&ending].phase, Phase::Done(_))
    });
    player
        .command(ending, MediaCommand::Seek(Duration::ZERO))
        .unwrap();
    let completion = player.take_result(ending).unwrap();
    assert_eq!(completion.result, MediaResult::Completed);
    assert_eq!(completion.position, ms(50));
    player.close();
    until(|| {
        player.maintain(Duration::ZERO);
        !player.cleanup_pending()
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
        let tiny = audio.play("assets/chime.wav", start: 0.1, end: 0.100000001);
        if await tiny != "Completed" || audio.position(tiny) != 0.1 { throw "quantized empty segment"; }
        let crossfade = audio.play("assets/chime.wav", group: "music", looping: true, fade_in: 0.01);
        let sound = audio.play("assets/chime.wav", group: "voice", end: 0.2);
        if audio.queue(sound, "assets/chime.wav", start: 0.03, end: 0.12) != null { throw "queue result"; }
        audio.queue(sound, "assets/chime.wav", start: 0.1, end: 0.18);
        pause(sound);
        if audio.seek(sound, 0.02) != null || audio.position(sound) != 0.02 { throw "seek result"; }
        audio.seek(sound, 0.01);
        if audio.position(sound) != 0.01 { throw "latest seek"; }
        resume(sound);
        audio.pan(sound, 1, duration: 0.01);
        audio.volume(sound, 0.5, duration: 0.01);
        if await sound != "Completed" || await sound != "Completed" { throw "completion"; }
        if audio.position(sound) != 0.18 { throw "queue end not retained"; }
        audio.seek(sound, 0);
        if await sound != "Completed" { throw "seek restarted completed activity"; }
        audio.stop(crossfade, fade: 0.01);
        if await crossfade != "Stopped" { throw "fade completion"; }
        let mut invalid = 0;
        try { audio.group_volume("typo", 1); } catch error { invalid += 1; }
        try { audio.play("assets/chime.wav", pan: 2); } catch error { invalid += 1; }
        try { audio.volume(crossfade, 1, duration: -1); } catch error { invalid += 1; }
        try { audio.seek(crossfade, -1); } catch error { invalid += 1; }
        try { audio.queue(sound, "assets/chime.wav"); } catch error { invalid += 1; }
        try { audio.queue(crossfade, "assets/chime.wav", looping: false, loop_start: 1); } catch error { invalid += 1; }
        try { audio.play("assets/chime.wav", start: 0.1, end: 0.1); } catch error { invalid += 1; }
        try { audio.queue(sound, "assets/chime.wav", start: 0.1, end: 0.05); } catch error { invalid += 1; }
        try { await audio.play("assets/chime.wav", end: 1); } catch error { invalid += 1; }
        try { await audio.play("assets/chime.wav", end: 0.1, looping: true, loop_end: 0.2); } catch error { invalid += 1; }
        if invalid != 10 { throw "audio validation"; }
        let music = audio.play("assets/chime.wav", looping: true);
        pause(music); audio.volume(music, 0); resume(music);
        stop(music);
        if await music != "Stopped" { throw "loop stop"; }
        let node = text("owned sound"); stage.replace(node);
        let owned = audio.play("assets/chime.wav", looping: true, owner: node);
        audio.queue(owned, "assets/chime.wav");
        audio.queue(owned, "assets/chime.wav");
        stage.replace(text("replacement"));
        let mut cancelled = false;
        try { await owned; } catch error { cancelled = true; }
        if !cancelled { throw "owner cancellation"; }
        let mut failed = false;
        try { await audio.play("assets/not-present.wav"); } catch error { failed = true; }
        if !failed { throw "missing resource"; }
        let broken = audio.play("assets/chime.wav", looping: true);
        audio.queue(broken, "assets/not-present.wav");
        let mut failures = 0;
        try { await broken; } catch error { failures += 1; }
        try { await broken; } catch error { failures += 1; }
        if failures != 2 { throw "queued failure terminal"; }
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
        .with_media(Box::new(backend))
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
fn bounded_audio_source_and_asb_restore_real_transport_and_queue_endpoints() {
    use aside_vm::snapshot::SnapshotBudget;
    use reel_engine::{
        InputAction, SessionIdentity,
        aside::{SessionVm, snapshot::SessionSnapshot},
    };
    let text = r#"fn main() {
        let sound = audio.play("assets/chime.wav", start: 0.04, end: 0.08);
        audio.queue(sound, "assets/chime.wav", start: 0.02, end: 0.12);
        pause(sound);
        narrator("save bounded audio");
        if audio.position(sound) != 0.04 { throw "restored absolute position"; }
        resume(sound);
        if await sound != "Completed" || audio.position(sound) != 0.12 { throw "restored queue end"; }
        if await sound != "Completed" { throw "terminal replay"; }
    }"#;
    for binary in [false, true] {
        let (backend, output) = rig();
        let pcm = backend.pcm.clone();
        let mut compiled = source::compile_story(text, "bounded_audio", 1, "main").unwrap();
        if binary {
            let bytes =
                aside_vm::bytecode::encode(compiled.program.package(), Default::default()).unwrap();
            compiled.program =
                aside_vm::bytecode::VerifiedProgram::decode(&bytes, Default::default()).unwrap();
        }
        let driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            compiled,
            "en",
            VmLimits::default(),
        )
        .unwrap()
        .with_media(Box::new(backend))
        .unwrap();
        let mut original = StoryRuntime::new(driver);
        until(|| {
            original.tick(SliceBudget::default(), || 0).unwrap();
            assert_eq!(original.driver().state(), StoryState::Running);
            assert_eq!(
                output.render(),
                0.,
                "paused initial batch must remain silent"
            );
            original
                .driver()
                .ui()
                .session()
                .view()
                .interaction
                .is_some()
                && pcm.usage().bytes > 0
        });
        let live = original.driver();
        let mut capture = live.snapshot_begin().unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let bytes = capture.finish().unwrap().encode().unwrap();
        let mut image = SessionSnapshot::decode(&bytes).unwrap();
        let (backend, output) = rig();
        let restored_pcm = backend.pcm.clone();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            live.adapter().vm().program().clone(),
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(2),
                epoch: Epoch(2),
            },
            Default::default(),
            Some(Box::new(backend)),
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (session, vm) = restore.finish().unwrap();
        let mut restored = AsideSessionUi::restored(
            session,
            vm,
            "en".into(),
            live.ui().assets(),
            live.initializer(),
        )
        .unwrap();
        while !restored.prepare_restored(SliceBudget::default()).unwrap() {}
        restored.resume_media();
        original.close().unwrap();
        until(|| {
            original.tick(SliceBudget::default(), || 0).unwrap();
            original.close_complete()
        });
        assert_eq!(pcm.usage().bytes, 0);
        let session = restored.ui_mut().session_mut();
        let interaction = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
                .unwrap();
        }
        let mut runtime = StoryRuntime::new(restored);
        let mut peak = 0_f32;
        until(|| {
            peak = peak.max(output.render());
            runtime.tick(SliceBudget::default(), || 0).unwrap();
            assert!(
                matches!(
                    runtime.driver().state(),
                    StoryState::Running | StoryState::Finished
                ),
                "{:?}",
                runtime.driver().fault()
            );
            runtime.driver().state() == StoryState::Finished && !runtime.needs_tick()
        });
        assert!(peak > 0.1);
        assert_eq!(restored_pcm.usage().bytes, 0);
    }
}

#[test]
fn real_audio_controls_freeze_position_mute_loop_and_reclaim_on_output_failure() {
    let (mut player, output) = rig();
    let id = player.play_audio(request(true), false).unwrap();
    player.commit_audio(id);
    until(|| {
        player.maintain(Duration::ZERO);
        output.render() > 0.1
    });
    player.command(id, MediaCommand::Pause).unwrap();
    // Flush the renderer's already-generated block; paused playback remains live.
    for _ in 0..4 {
        output.render();
    }
    assert_eq!(output.render(), 0.);
    assert!(player.take_result(id).is_none());
    player.command(id, MediaCommand::Resume).unwrap();
    until(|| output.render() > 0.1);
    player
        .command(
            id,
            MediaCommand::Volume {
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
            MediaCommand::Volume {
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
        player.maintain(Duration::ZERO);
    }
    assert_eq!(output.clock_observation().generated_frames, before);
    player.resume();
    until(|| output.render() > 0.1);
    output.0.borrow_mut().as_mut().unwrap().fail_device();
    let result = loop {
        player.maintain(Duration::ZERO);
        if let Some(completion) = player.take_result(id) {
            break completion.result;
        }
    };
    assert!(matches!(result, MediaResult::Failed(_)));
    assert!(player.take_result(id).is_none());
    player.close();
    until(|| {
        player.maintain(Duration::ZERO);
        !player.cleanup_pending()
    });
    assert_eq!(player.pcm_usage().bytes, 0);
    assert!(player.poll_after().is_none());
}

#[test]
fn loading_cancellation_and_pcm_budget_failure_leave_no_resident_requests() {
    let (mut player, output) = rig();
    let id = player.play_audio(request(false), false).unwrap();
    player.commit_audio(id);
    player.cancel(id);
    player.close();
    until(|| {
        player.maintain(Duration::ZERO);
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
    let id = player.play_audio(request(false), false).unwrap();
    player.commit_audio(id);
    let mut result = None;
    until(|| {
        player.maintain(Duration::ZERO);
        result = player.take_result(id).map(|completion| completion.result);
        result.is_some()
    });
    assert!(
        matches!(result, Some(MediaResult::Failed(error)) if error.starts_with("assets/chime.wav: "))
    );
    player.close();
    until(|| {
        player.maintain(Duration::ZERO);
        !player.cleanup_pending()
    });
    assert_eq!(player.pcm_usage().bytes, 0);
}
