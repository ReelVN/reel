use super::*;
use crate::audio::{AudioPlayback, AudioQueueItem, AudioRequest};
use crate::video::VideoPlaybackState;
use crate::{
    Epoch, InputAction, Session, SessionId, SessionIdentity,
    aside::{
        RunOutcome, SessionBudget, SessionStep, SliceBudget, VmLimits,
        checkpoint::{CheckpointLoad, CheckpointRecord},
        snapshot::{SessionSnapshot, SnapshotBudget},
        source,
    },
};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, Default)]
struct Backend(Rc<RefCell<Transport>>);
#[derive(Default)]
struct Transport {
    entries: Vec<MediaPlayback>,
    events: Vec<&'static str>,
    gains: Vec<(MediaId, f64, Duration)>,
}
impl Backend {
    fn position_to(&self, seconds: f64) {
        for entry in &mut self.0.borrow_mut().entries {
            match entry {
                MediaPlayback::Audio(audio) => {
                    audio.request.start_position = Duration::from_secs_f64(seconds)
                }
                MediaPlayback::Video(video) => video.position = Duration::from_secs_f64(seconds),
            }
        }
    }
    fn terminal(&self) {
        for entry in &mut self.0.borrow_mut().entries {
            match entry {
                MediaPlayback::Audio(audio) => audio.result = Some(MediaResult::Stopped),
                MediaPlayback::Video(video) => video.result = Some(MediaResult::Stopped),
            }
        }
    }
}
impl MediaBackend for Backend {
    fn play_audio(&mut self, request: AudioRequest, paused: bool) -> Result<MediaId, String> {
        let mut state = self.0.borrow_mut();
        state.events.push(if paused {
            "prepare-audio"
        } else {
            "start-audio"
        });
        state.entries.push(MediaPlayback::Audio(AudioPlayback {
            request,
            queue: Vec::new(),
            paused,
            result: None,
        }));
        Ok(MediaId(state.entries.len() as u64))
    }
    fn commit_audio(&mut self, _: MediaId) {
        self.0.borrow_mut().events.push("commit");
    }
    fn queue_audio(&mut self, id: MediaId, item: AudioQueueItem) -> Result<(), String> {
        let mut state = self.0.borrow_mut();
        state.events.push("queue");
        let MediaPlayback::Audio(audio) = &mut state.entries[id.0 as usize - 1] else {
            return Err("not audio".into());
        };
        audio.queue.push(item);
        Ok(())
    }
    fn restore_video(&mut self, playback: VideoPlaybackState) -> Result<MediaId, String> {
        let mut state = self.0.borrow_mut();
        state.events.push(if playback.paused {
            "prepare-video"
        } else {
            "start-video"
        });
        state.entries.push(MediaPlayback::Video(playback));
        Ok(MediaId(state.entries.len() as u64))
    }
    fn play_video(
        &mut self,
        request: crate::video::VideoRequest,
        paused: bool,
    ) -> Result<MediaId, String> {
        self.restore_video(VideoPlaybackState {
            position: request.start_position,
            request,
            paused,
            result: None,
        })
    }
    fn snapshot(&self, id: MediaId) -> Result<MediaPlayback, String> {
        Ok(self.0.borrow().entries[id.0 as usize - 1].clone())
    }
    fn position(&self, id: MediaId) -> Option<Duration> {
        self.0
            .borrow()
            .entries
            .get(id.0 as usize - 1)
            .map(transport_position)
    }
    fn command(&mut self, id: MediaId, command: MediaCommand) -> Result<(), String> {
        let mut state = self.0.borrow_mut();
        state.events.push(match command {
            MediaCommand::Resume => "resume",
            MediaCommand::Pause => "pause",
            _ => "control",
        });
        let (paused, result) = match &mut state.entries[id.0 as usize - 1] {
            MediaPlayback::Audio(audio) => (&mut audio.paused, &mut audio.result),
            MediaPlayback::Video(video) => (&mut video.paused, &mut video.result),
        };
        match command {
            MediaCommand::Resume => *paused = false,
            MediaCommand::Pause => *paused = true,
            MediaCommand::Stop | MediaCommand::FadeOut(_) => *result = Some(MediaResult::Stopped),
            _ => {}
        }
        Ok(())
    }
    fn group_volume(&mut self, _: Option<AudioGroup>, _: f64, _: Duration) -> Result<(), String> {
        Ok(())
    }
    fn duck_audio(&mut self, id: MediaId, gain: f64, fade: Duration) -> Result<(), String> {
        let mut state = self.0.borrow_mut();
        state.events.push("duck");
        state.gains.push((id, gain, fade));
        Ok(())
    }
    fn cancel(&mut self, id: MediaId) {
        let _ = self.command(id, MediaCommand::Stop);
    }
    fn maintain(&mut self, _: Duration) {}
    fn take_result(&mut self, id: MediaId) -> Option<MediaCompletion> {
        let snapshot = self.snapshot(id).ok()?;
        let result = match &snapshot {
            MediaPlayback::Audio(audio) => audio.result.clone(),
            MediaPlayback::Video(video) => video.result.clone(),
        }?;
        Some(MediaCompletion {
            result,
            position: transport_position(&snapshot),
        })
    }
    fn poll_after(&self) -> Option<Duration> {
        None
    }
    fn suspend(&mut self) {}
    fn resume(&mut self) {}
    fn close(&mut self) {}
    fn cleanup_pending(&self) -> bool {
        false
    }
}

fn code(body: &str) -> String {
    format!(
        r#"
    use std::resume::{{state,root,flow,point}};
    use reel::types::MediaCue;
    class World {{let mut track:any=null;let mut alias:any=null;let mut cue:MediaCue|null=null;let mut movie:any=null;let mut second_voice:any=null;}}
    state(World,"world");root(main,"game");
    fn main(state:World=model(World())){{reading(||{{
        let dialogue=game.dialogue();stage.replace(stack(){{text(derive(||dialogue.text));}});
        if state.movie!=null{{stage.replace(state.movie);}}
        run_flow(story,state)
    }})}}
    fn story(state:World){{flow("media-story");{body}}}
"#
    )
}
fn loaded(code: &str, version: u64, encoded: bool, backend: Backend) -> (Session, SessionVm) {
    let mut story = source::compile_story(code, "media_protocol", version, "main").unwrap();
    if encoded {
        story.program = aside_vm::bytecode::VerifiedProgram::decode(
            &story
                .program
                .encode(Default::default())
                .unwrap()
                .into_bytes(),
            Default::default(),
        )
        .unwrap();
    }
    let initializer = story.initializer;
    let session = Session::new(SessionId(918), Epoch(version)).unwrap();
    let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
    vm.set_media(Box::new(backend)).unwrap();
    vm.start(&session, initializer).unwrap();
    (session, vm)
}
fn next(vm: &mut SessionVm, session: &mut Session) -> RunOutcome {
    for _ in 0..100_000 {
        match vm
            .step(
                session,
                SessionBudget::RetryMinimum(SliceBudget {
                    instructions: 1,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::BudgetYield(_)) | SessionStep::Progress => {}
            SessionStep::Vm(outcome) => return outcome,
            other => panic!("unexpected media step {other:?}"),
        }
    }
    panic!("media fixture exhausted work")
}
fn settle(vm: &mut SessionVm, session: &mut Session) -> RunOutcome {
    loop {
        vm.maintain_media().unwrap();
        vm.sample_media_positions().unwrap();
        match next(vm, session) {
            RunOutcome::HostCall(ticket) => {
                vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(128))
                    .unwrap();
            }
            outcome => {
                vm.sync_reading(session).unwrap();
                return outcome;
            }
        }
    }
}
fn start(code: &str, encoded: bool, backend: Backend) -> (Session, SessionVm) {
    let (mut session, mut vm) = loaded(code, 1, encoded, backend);
    assert!(matches!(
        settle(&mut vm, &mut session),
        RunOutcome::Returned(_)
    ));
    vm.discard_delivery(&mut session).unwrap();
    vm.start_root(&session, None).unwrap();
    assert!(matches!(
        settle(&mut vm, &mut session),
        RunOutcome::Waiting(_)
    ));
    (session, vm)
}
fn capture(vm: &mut SessionVm, session: &Session) -> CheckpointRecord {
    let mut capture = vm.capture_checkpoint_begin(session).unwrap();
    for _ in 0..100_000 {
        if capture
            .step(vm, session, NativeBudget::RetryMinimum(1), 1)
            .unwrap()
        {
            let capture = capture.finish(vm).unwrap();
            let mut bytes = Vec::new();
            capture.record().write_to(&mut bytes).unwrap();
            return CheckpointRecord::decode(&bytes).unwrap();
        }
    }
    panic!("media capture exhausted work")
}
fn restore(
    code: &str,
    encoded: bool,
    record: CheckpointRecord,
    backend: Backend,
) -> (Session, SessionVm) {
    let (mut session, mut vm) = loaded(code, 2, encoded, backend);
    let mut load = CheckpointLoad::new(record, &vm).unwrap();
    for _ in 0..100_000 {
        if load
            .step(
                &mut vm,
                &mut session,
                "en",
                NativeBudget::RetryMinimum(1),
                1,
                SliceBudget {
                    instructions: 1,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap()
        {
            return (session, vm);
        }
    }
    panic!("media load exhausted work")
}
fn restore_exact(vm: &SessionVm, session: &Session, backend: Backend) -> (Session, SessionVm) {
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let bytes = capture.finish().unwrap().encode().unwrap();
    let mut image = SessionSnapshot::decode(&bytes).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        vm.vm.program().clone(),
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(918),
            epoch: Epoch(2),
        },
        Default::default(),
        Some(Box::new(backend)),
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    restore.finish().unwrap()
}

fn advance(session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
        if session.completion_pending() {
            break;
        }
    }
}

#[test]
fn audio_queue_position_aliases_and_adoption_use_one_typed_graph_source_asb() {
    let a = code(
        r#"audio.play("unretained.ogg");state.track=audio.play("old.ogg",start:2,volume:0.4);audio.queue(state.track,"tail.ogg",start:1);state.alias=state.track;point("line");narrator("A");if state.track!=state.alias{throw "alias";}audio.stop(state.track);await state.track;40"#,
    );
    let b = code(
        r#"state.track=audio.play("changed.ogg");state.alias=state.track;point("line");narrator("B");if state.track!=state.alias{throw "alias";}if state.track.position()!=4.5{throw "position";}audio.stop(state.track);await state.track;42"#,
    );
    for encoded in [false, true] {
        let old = Backend::default();
        let (session, mut vm) = start(&a, encoded, old.clone());
        assert_eq!(old.0.borrow().entries.len(), 2);
        old.position_to(4.5);
        let record = capture(&mut vm, &session);
        let backend = Backend::default();
        let (mut session, mut vm) = restore(&b, encoded, record, backend.clone());
        {
            let transport = backend.0.borrow();
            assert_eq!(transport.entries.len(), 1);
            let MediaPlayback::Audio(audio) = &transport.entries[0] else {
                unreachable!()
            };
            assert_eq!(audio.request.source, "old.ogg");
            assert_eq!(audio.request.start_position, Duration::from_secs_f64(4.5));
            assert_eq!(audio.queue.len(), 1);
            assert_eq!(audio.queue[0].source, "tail.ogg");
            assert!(!audio.paused);
            assert_eq!(
                transport.events,
                ["prepare-audio", "queue", "commit", "resume"]
            );
        }
        advance(&mut session);
        let RunOutcome::Returned(delivery) = settle(&mut vm, &mut session) else {
            panic!("resumed audio did not complete")
        };
        assert_eq!(vm.vm.delivery_value(delivery).unwrap().as_int(), Some(42));
    }
}

#[test]
fn overlapping_ducks_restore_owned_levels_and_release_independently() {
    let program = code(
        r#"
        state.track=audio.play("music.ogg",group:"music",volume:0.8);
        state.alias=audio.play("first.ogg",group:"voice");
        state.second_voice=audio.play("second.ogg",group:"voice");
        audio.duck(state.track,by:state.alias,gain:0.5,attack:0.1,release:0.2);
        audio.duck(state.track,by:state.second_voice,gain:0.25,attack:0.15,release:0.3);
        point("line");narrator("A");
        audio.stop(state.alias);await state.alias;
        audio.stop(state.second_voice);await state.second_voice;
        42"#,
    );
    let old = Backend::default();
    let (session, mut vm) = start(&program, true, old.clone());
    assert_eq!(
        old.0.borrow().gains,
        [
            (MediaId(1), 0.5, Duration::from_millis(100)),
            (MediaId(1), 0.125, Duration::from_millis(150)),
        ],
    );
    let exact = Backend::default();
    let _ = restore_exact(&vm, &session, exact.clone());
    assert_eq!(exact.0.borrow().gains.last().unwrap().1, 0.125);

    let record = capture(&mut vm, &session);
    let backend = Backend::default();
    let (mut session, mut vm) = restore(&program, true, record, backend.clone());
    assert_eq!(
        &backend.0.borrow().events[..6],
        [
            "prepare-audio",
            "prepare-audio",
            "prepare-audio",
            "duck",
            "duck",
            "commit"
        ],
    );
    let last = *backend.0.borrow().gains.last().expect("restored duck");
    assert_eq!((last.1, last.2), (0.125, Duration::ZERO));
    let music = last.0;
    assert!(matches!(&backend.0.borrow().entries[music.0 as usize - 1],
            MediaPlayback::Audio(entry) if entry.request.volume == 0.8));
    advance(&mut session);
    let RunOutcome::Returned(delivery) = settle(&mut vm, &mut session) else {
        panic!("voice activities did not retire");
    };
    assert_eq!(vm.vm.delivery_value(delivery).unwrap().as_int(), Some(42));
    let gains = &backend.0.borrow().gains;
    assert_eq!(
        gains[gains.len() - 2..],
        [
            (music, 0.25, Duration::from_millis(200)),
            (music, 1., Duration::from_millis(300)),
        ],
    );
}

#[test]
fn terminal_audio_await_returns_its_saved_result_without_starting_transport() {
    let b = code(
        r#"state.track=audio.play("changed.ogg");point("line");narrator("B");await state.track"#,
    );
    for omitted in [false, true] {
        let a = code(if omitted {
            r#"game.reading().set_skip(true, unread: true);
                narrator("Skipped", wait: false);
                state.track=audio.play("omitted.ogg", group: "voice", skip: "omit");
                game.reading().set_skip(false);
                point("line");narrator("A");await state.track"#
        } else {
            r#"state.track=audio.play("old.ogg");point("line");narrator("A");await state.track"#
        });
        let old = Backend::default();
        let (session, mut vm) = start(&a, false, old.clone());
        if omitted {
            assert!(old.0.borrow().entries.is_empty());
        } else {
            old.terminal();
        }
        let record = capture(&mut vm, &session);
        let backend = Backend::default();
        let (mut session, mut vm) = restore(&b, false, record, backend.clone());
        assert!(backend.0.borrow().entries.is_empty());
        advance(&mut session);
        let RunOutcome::Returned(delivery) = settle(&mut vm, &mut session) else {
            panic!("terminal media did not complete")
        };
        assert_eq!(
            crate::aside::text::read_string(
                vm.vm.heap(),
                vm.vm.delivery_value(delivery).unwrap(),
                64,
            )
            .unwrap(),
            "Stopped"
        );
    }
}

#[test]
fn dialogue_voice_restores_without_replaying_and_keeps_history_replay_path() {
    for (encoded, skipping) in [(false, false), (true, true), (true, false)] {
        let skip = if skipping {
            "game.reading().set_skip(true, unread: true);"
        } else {
            ""
        };
        let before = code(&format!(
            r#"{skip} point("line");narrator("A",voice:"original.ogg",voice_volume:0.4);42"#,
        ));
        let verify = if skipping {
            r#"if game.dialogue().voice!=null { throw "fast-forward started voice"; }"#
        } else {
            r#"if game.dialogue().voice.position()!=2.5 { throw "voice position"; }"#
        };
        let after = code(&format!(
            r#"point("line");narrator("B",voice:"changed.ogg");
                {verify}
                let input=game.history()[0].lines[0].voice;
                if input!="original.ogg" {{throw "history voice";}}
                audio.play(input,group:"voice");42"#,
        ));
        let old = Backend::default();
        let (session, mut vm) = start(&before, encoded, old.clone());
        assert_eq!(old.0.borrow().entries.len(), usize::from(!skipping));
        assert_eq!(
            session.view().history[0].lines()[0].voice(),
            Some("original.ogg")
        );
        let (restored, _) = restore_exact(&vm, &session, Backend::default());
        assert_eq!(
            restored.view().history[0].lines()[0].voice(),
            Some("original.ogg")
        );
        old.position_to(2.5);
        let record = capture(&mut vm, &session);
        let backend = Backend::default();
        let (mut session, mut vm) = restore(&after, encoded, record, backend.clone());
        {
            let state = backend.0.borrow();
            assert_eq!(state.entries.len(), usize::from(!skipping));
            if let Some(MediaPlayback::Audio(audio)) = state.entries.first() {
                assert_eq!(audio.request.source, "original.ogg");
                assert_eq!(audio.request.start_position, Duration::from_secs_f64(2.5));
                assert_eq!(audio.request.volume, 0.4);
            }
        }
        advance(&mut session);
        let RunOutcome::Returned(delivery) = settle(&mut vm, &mut session) else {
            panic!("voice history replay did not finish");
        };
        assert_eq!(vm.vm.delivery_value(delivery).unwrap().as_int(), Some(42));
        let state = backend.0.borrow();
        let Some(MediaPlayback::Audio(replay)) = state.entries.last() else {
            panic!("history voice did not start");
        };
        assert_eq!(replay.request.source, "original.ogg");
        assert_eq!(replay.request.start_position, Duration::ZERO);
    }
}

#[test]
fn video_target_and_transport_position_restore_in_the_same_graph() {
    let a = code(
        r#"state.movie=video("old.mp4",width:160,height:90);state.track=state.movie.play();point("line");narrator("A");7"#,
    );
    let b = code(
        r#"state.movie=video("changed.mp4");state.track=state.movie.play();point("line");narrator("B");8"#,
    );
    let old = Backend::default();
    let (session, mut vm) = start(&a, true, old.clone());
    old.position_to(3.25);
    let record = capture(&mut vm, &session);
    let backend = Backend::default();
    let (mut session, mut vm) = restore(&b, true, record, backend.clone());
    {
        let transport = backend.0.borrow();
        assert_eq!(transport.entries.len(), 1);
        let MediaPlayback::Video(video) = &transport.entries[0] else {
            unreachable!()
        };
        assert_eq!(video.request.source, "old.mp4");
        assert_eq!(video.position, Duration::from_secs_f64(3.25));
        assert!(!video.paused);
        assert_eq!(transport.events, ["prepare-video", "resume"]);
    }
    assert_eq!(vm.media.live.len(), 1);
    let target = vm.media.live[0].target.unwrap();
    assert_eq!(vm.stage.scene.node(target).unwrap().props.width, Some(160.));
    advance(&mut session);
    assert!(matches!(
        settle(&mut vm, &mut session),
        RunOutcome::Returned(_)
    ));
}

#[test]
fn cue_wait_reconnects_its_source_and_preserves_the_previous_page_source_asb() {
    let a = code(
        r#"state.track=audio.play("old.ogg");state.cue=state.track.at(5);point("line");narrator("A");point("cue");await state.cue"#,
    );
    let b = code(
        r#"state.track=audio.play("changed.ogg");state.cue=state.track.at(99);point("line");narrator("B");point("cue");await state.cue"#,
    );
    for encoded in [false, true] {
        let old = Backend::default();
        let (mut session, mut vm) = start(&a, encoded, old.clone());
        advance(&mut session);
        assert!(matches!(
            settle(&mut vm, &mut session),
            RunOutcome::Waiting(_)
        ));
        assert!(session.view().interaction.is_none());
        old.position_to(2.5);
        let record = capture(&mut vm, &session);
        let backend = Backend::default();
        let (mut session, mut vm) = restore(&b, encoded, record, backend.clone());
        assert_eq!(
            vm.checkpoint_display().unwrap().lines[0].fallback_text(),
            "A"
        );
        assert_eq!(session.view().history.len(), 1);
        assert_eq!(backend.0.borrow().entries.len(), 1);
        assert_eq!(vm.media.cues.len(), 1);
        let cue = vm.vm.heap().root_value(vm.media.cues[0]).unwrap();
        let playback = vm.vm.heap().root_value(vm.media.live[0].root).unwrap();
        assert_ne!(
            vm.vm
                .heap()
                .host_payload(playback, Slot::Owner as u32)
                .unwrap()
                .tag(),
            ValueTag::Null
        );
        assert!(
            vm.vm
                .heap()
                .host_payload(cue, Slot::Id as u32)
                .unwrap()
                .same_bits(playback)
        );
        assert!(
            vm.vm
                .heap()
                .host_payload(cue, Slot::Owner as u32)
                .unwrap()
                .same_bits(
                    vm.vm
                        .heap()
                        .host_payload(playback, Slot::Owner as u32)
                        .unwrap()
                )
        );
        backend.position_to(5.25);
        let RunOutcome::Returned(delivery) = settle(&mut vm, &mut session) else {
            panic!("restored cue did not reach its saved threshold")
        };
        assert_eq!(
            vm.vm.delivery_value(delivery).unwrap().as_bool(),
            Some(true)
        );
    }
}
