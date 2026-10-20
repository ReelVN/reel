use super::*;
use crate::{audio::*, media::*, video::*};
use aside_vm::snapshot::SnapshotBudget;
use std::{cell::RefCell, rc::Rc, time::Duration};

#[derive(Clone, Default)]
struct Host(Rc<RefCell<Vec<MediaPlayback>>>);
impl Host {
    fn set(&self, index: usize, seconds: f64, result: Option<MediaResult>) {
        let mut entries = self.0.borrow_mut();
        match &mut entries[index] {
            MediaPlayback::Audio(entry) => {
                entry.request.start_position = Duration::from_secs_f64(seconds);
                entry.result = result;
            }
            MediaPlayback::Video(entry) => {
                entry.position = Duration::from_secs_f64(seconds);
                entry.result = result;
            }
        }
    }
    fn push(&self, value: MediaPlayback) -> MediaId {
        let mut entries = self.0.borrow_mut();
        entries.push(value);
        MediaId(entries.len() as u64)
    }
}
impl MediaBackend for Host {
    fn play_audio(&mut self, request: AudioRequest) -> Result<MediaId, String> {
        Ok(self.push(MediaPlayback::Audio(AudioPlayback {
            request,
            queue: Vec::new(),
            paused: false,
            result: None,
        })))
    }
    fn commit_audio(&mut self, _: MediaId) {}
    fn play_video(&mut self, request: VideoRequest) -> Result<MediaId, String> {
        self.restore_video(VideoPlaybackState {
            position: request.start_position,
            request,
            paused: false,
            result: None,
        })
    }
    fn restore_video(&mut self, state: VideoPlaybackState) -> Result<MediaId, String> {
        Ok(self.push(MediaPlayback::Video(state)))
    }
    fn snapshot(&self, id: MediaId) -> Result<MediaPlayback, String> {
        Ok(self.0.borrow()[id.0 as usize - 1].clone())
    }
    fn position(&self, id: MediaId) -> Option<Duration> {
        Some(match &self.0.borrow()[id.0 as usize - 1] {
            MediaPlayback::Audio(entry) => entry.request.start_position,
            MediaPlayback::Video(entry) => entry.position,
        })
    }
    fn command(&mut self, id: MediaId, command: MediaCommand) -> Result<(), String> {
        let mut entries = self.0.borrow_mut();
        let (paused, position, result) = match &mut entries[id.0 as usize - 1] {
            MediaPlayback::Audio(e) => {
                (&mut e.paused, &mut e.request.start_position, &mut e.result)
            }
            MediaPlayback::Video(e) => (&mut e.paused, &mut e.position, &mut e.result),
        };
        match command {
            MediaCommand::Seek(target) => *position = target,
            MediaCommand::Pause => *paused = true,
            MediaCommand::Resume => *paused = false,
            MediaCommand::Stop => *result = Some(MediaResult::Stopped),
            _ => return Err("unsupported fixture command".into()),
        }
        Ok(())
    }
    fn cancel(&mut self, id: MediaId) {
        let pos = self.position(id).unwrap().as_secs_f64();
        self.set(id.0 as usize - 1, pos, Some(MediaResult::Cancelled));
    }
    fn group_volume(&mut self, _: Option<AudioGroup>, _: f64, _: Duration) -> Result<(), String> {
        Ok(())
    }
    fn maintain(&mut self, _: Duration) {}
    fn take_result(&mut self, id: MediaId) -> Option<MediaCompletion> {
        let result = match &self.0.borrow()[id.0 as usize - 1] {
            MediaPlayback::Audio(e) => e.result.clone(),
            MediaPlayback::Video(e) => e.result.clone(),
        };
        result.map(|result| MediaCompletion {
            result,
            position: self.position(id).unwrap(),
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
fn flush(vm: &mut SessionVm, session: &mut Session, seconds: f64) {
    for _ in 0..10 {
        vm.maintain_media().unwrap();
        vm.advance_time(session, seconds, 1024).unwrap();
        models::drive(vm, session, false);
    }
}
fn say(session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
}
fn dialogue(session: &Session, expected: &str) {
    assert!(
        matches!(session.view().interaction,
        Some(crate::InteractionView::Dialogue(d)) if d.text() == expected),
        "expected dialogue {expected}"
    );
}

#[test]
fn seek_updates_derived_position_and_freezes_repeated_cue_result() {
    for namespace in ["audio", "video"] {
        let play = if namespace == "audio" {
            r#"audio.play("track")"#
        } else {
            r#"video.play(video("movie"))"#
        };
        let source = format!(
            r#"fn opening() {{
            let media={play}; pause(media);
            let observed=derive(|| {namespace}.position(media));
            let marker=text("position",x:observed); stage.replace(marker);
            let cue={namespace}.at(media,2);
            {namespace}.seek(media,3);
            if {namespace}.position(media)!=3 {{throw "seek not immediately visible";}}
            {namespace}.seek(media,0.5);
            if !(await cue) || !(await cue) {{throw "reusable reached result changed";}}
            narrator("seek");
            if marker.x!=0.5 {{throw "seek did not update binding";}}
            let before_end={namespace}.at(media,0.75);
            stop(media);
            if await before_end {{throw "early stop reported arrival";}}
            if await media!="Stopped" {{throw "stop result";}}
            narrator("done");
        }}"#
        );
        for encoded in [false, true] {
            let (mut session, mut vm) = models::prepared(&source, encoded);
            vm.set_media(Box::new(Host::default())).unwrap();
            flush(&mut vm, &mut session, 0.);
            dialogue(&session, "seek");
            say(&mut session);
            flush(&mut vm, &mut session, 0.);
            dialogue(&session, "done");
            assert!(!vm.media.active());
        }
    }
}

#[test]
fn final_position_settles_cue_before_terminal_and_preserves_failures() {
    for (result, expected) in [
        (MediaResult::Completed, "true"),
        (MediaResult::Stopped, "true"),
        (MediaResult::Cancelled, "media owner retired"),
        (
            MediaResult::Failed("decode fixture".into()),
            "decode fixture",
        ),
    ] {
        for encoded in [false, true] {
            let source = r#"fn opening() {
                let media=audio.play("track"); let cue=audio.at(media,2);
                let mut value="";
                try { value=f"{await cue}"; }
                catch error {
                    value=error;
                }
                if audio.position(media)!=2 {throw "terminal cursor";}
                narrator(value);
            }"#;
            let (mut session, mut vm) = models::prepared(source, encoded);
            let host = Host::default();
            vm.set_media(Box::new(host.clone())).unwrap();
            models::drive(&mut vm, &mut session, false);
            host.set(0, 2., Some(result.clone()));
            vm.maintain_media().unwrap();
            assert_eq!(
                vm.media_poll_after(),
                Some(Duration::ZERO),
                "terminal needs publication without another clock"
            );
            flush(&mut vm, &mut session, 0.);
            dialogue(&session, expected);
        }
    }
}

#[test]
fn cue_failure_and_cancellation_keep_the_media_fault_category() {
    for encoded in [false, true] {
        for (result, kind) in [
            (MediaResult::Cancelled, FaultKind::Cancelled),
            (
                MediaResult::Failed("decoder".into()),
                FaultKind::InvalidState,
            ),
        ] {
            let (mut session, mut vm) = models::prepared(
                r#"fn opening(){let media=audio.play("track");await audio.at(media,1);}"#,
                encoded,
            );
            let host = Host::default();
            vm.set_media(Box::new(host.clone())).unwrap();
            models::drive(&mut vm, &mut session, false);
            host.set(0, 0., Some(result));
            vm.maintain_media().unwrap();
            vm.advance_time(&session, 0., 64).unwrap();
            let RunOutcome::Fault(id) = next(&mut vm, &mut session) else {
                panic!("expected media fault");
            };
            assert_eq!(vm.vm.delivery_fault(id).unwrap().unwrap().kind, kind);
        }
    }
}

#[test]
fn cue_waits_through_observed_loop_and_queue_position_rewinds() {
    for encoded in [false, true] {
        let source = r#"fn opening(){
            let sound=audio.play("track",looping:true);
            let cue=audio.at(sound,2);
            if !(await cue) {throw "rewind ended the condition";}
            narrator("arrived");
        }"#;
        let (mut session, mut vm) = models::prepared(source, encoded);
        let host = Host::default();
        vm.set_media(Box::new(host.clone())).unwrap();
        models::drive(&mut vm, &mut session, false);
        for position in [1.5, 0.2, 1.8, 0.1] {
            host.set(0, position, None);
            flush(&mut vm, &mut session, 0.);
            assert!(session.view().interaction.is_none());
        }
        host.set(0, 2., None);
        flush(&mut vm, &mut session, 0.);
        dialogue(&session, "arrived");
    }
}

#[test]
fn owner_retirement_and_waiter_cancellation_preserve_shared_sound() {
    let source = r#"fn opening() {
        let node=stack(); stage.replace(node);
        let media=audio.play("track",owner:node);
        let held={cue:null};
        scope(body:|| {held.cue=audio.at(media,100);});
        let mut retired=false;
        try {await held.cue;} catch error {retired=error=="media position owner retired";}
        if !retired {throw "position owner survived";}
        let cue=audio.at(media,1);
        let task=spawn(|| {await cue;});
        await run(delay(0.01));
        task.cancel();
        try {await task;} catch error {}
        audio.seek(media,1);
        if !(await cue) {throw "shared cue cancelled";}
        if audio.position(media)!=1 {throw "shared sound stopped";}
        narrator("alive");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        let host = Host::default();
        vm.set_media(Box::new(host.clone())).unwrap();
        flush(&mut vm, &mut session, 0.);
        flush(&mut vm, &mut session, 0.1);
        dialogue(&session, "alive");
        assert!(matches!(&host.0.borrow()[0],MediaPlayback::Audio(e) if e.result.is_none()));
        assert!(
            !vm.media
                .snapshot_records()
                .any(|record| matches!(record.unwrap().part, snapshot::Part::MediaCue))
        );
    }
}

#[test]
fn pending_cue_snapshot_preserves_shared_media_and_completed_result() {
    let source = r#"fn opening() {
        let media=audio.play("track");
        let alias=media;
        let reached=audio.at(media,0);
        let cue=audio.at(media,2);
        narrator("save");
        if alias!=media || !(await reached) {throw "restored identity/result";}
        audio.seek(alias,2);
        if !(await cue) || !(await cue) {throw "restored pending cue";}
        audio.seek(media,0);
        if !(await reached) || !(await cue) {throw "restored result rewound";}
        narrator("restored");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        vm.set_media(Box::new(Host::default())).unwrap();
        flush(&mut vm, &mut session, 0.);
        dialogue(&session, "save");
        let program = vm.vm.program().clone();
        let mut capture = vm.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let mut image =
            snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap())
                .unwrap();
        let host = Host::default();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            program,
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(41),
                epoch: Epoch(30),
            },
            crate::ReadProgress::default(),
            Some(Box::new(host.clone())),
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut session, mut vm) = restore.finish().unwrap();
        say(&mut session);
        flush(&mut vm, &mut session, 0.);
        dialogue(&session, "restored");
        assert_eq!(host.0.borrow().len(), 1);
        assert!(
            !vm.media
                .snapshot_records()
                .any(|record| matches!(record.unwrap().part, snapshot::Part::MediaCue))
        );
    }
}

#[test]
fn cue_round_does_not_restart_the_position_publication_barrier() {
    let source = r#"fn opening() {
        let media=audio.play("track"); let cues=[];
        for i in 0..128 {cues.append(audio.at(media,1));}
        for cue in cues {if !(await cue) {throw "missed position";}}
        narrator("all reached");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        let host = Host::default();
        vm.set_media(Box::new(host.clone())).unwrap();
        models::drive(&mut vm, &mut session, false);
        host.set(0, 2., None);
        // StoryRuntime collects before sampling; task maintenance collects again.
        for _ in 0..12 {
            vm.maintain_media().unwrap();
            vm.advance_time(&session, 0., 1).unwrap();
            models::drive(&mut vm, &mut session, false);
            vm.maintain_media().unwrap();
        }
        dialogue(&session, "all reached");
    }
}

#[test]
fn shared_cue_delivers_more_than_one_wait_slice_after_backend_stops_polling() {
    let source = r#"fn opening() {
        let root=stack();stage.replace(root);
        let sound=audio.play("track");
        let cue=audio.at(sound,1);
        for i in 0..130 {
            spawn(|| {if !(await cue) {throw "cue result";} root.x+=1;});
        }
        let cancelled=spawn(|| {await cue;throw "cancelled waiter resumed";});
        await run(delay(0.01));
        cancelled.cancel();
        try {await cancelled;} catch error {}
        narrator("ready");
        if root.x!=130 {throw "waiters lost after backend stopped polling";}
        narrator("done");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        let host = Host::default();
        vm.set_media(Box::new(host.clone())).unwrap();
        flush(&mut vm, &mut session, 0.);
        flush(&mut vm, &mut session, 0.1);
        dialogue(&session, "ready");
        host.set(0, 1., Some(MediaResult::Completed));
        vm.maintain_media().unwrap();
        vm.advance_time(&session, 0.1, 1024).unwrap();
        assert_eq!(
            vm.media_poll_after(),
            Some(Duration::from_millis(10)),
            "the remaining wait slice must wake a host whose backend stopped polling"
        );
        for _ in 0..12 {
            if vm.media_poll_after().is_none() {
                break;
            }
            vm.maintain_media().unwrap();
            vm.advance_time(&session, 0.1, 1024).unwrap();
            models::drive(&mut vm, &mut session, false);
        }
        say(&mut session);
        flush(&mut vm, &mut session, 0.1);
        dialogue(&session, "done");
        assert_eq!(vm.media_poll_after(), None);
        assert!(
            !vm.media
                .snapshot_records()
                .any(|record| matches!(record.unwrap().part, snapshot::Part::MediaWait))
        );
    }
}

#[test]
fn continuous_media_and_clock_settle_the_same_sliced_reactive_batch() {
    let source = r#"fn sample(media) {
        let before=audio.position(media);
        let mut i=0; while i<8 {i+=1;}
        if before!=audio.position(media) {throw "media changed during derived batch";}
        before
    }
    fn opening() {
        let media=audio.play("track");
        let timer=clock(interval:0.002);
        let root=stack();
        for i in 0..6 {
            root.add(text("sample",x:derive(||sample(media)),y:derive(||timer.elapsed)));
        }
        stage.replace(root);
        let delayed=spawn(|| {await run(delay(0.5)); root.opacity=0.5;});
        narrator("running");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        let host = Host::default();
        vm.set_media(Box::new(host.clone())).unwrap();
        models::drive(&mut vm, &mut session, false);
        let mut publications = 0;
        let mut last = 0.;
        for tick in 1..=4000 {
            let time = f64::from(tick) / 1000.;
            host.set(0, time, None);
            vm.maintain_media().unwrap();
            vm.advance_time(&session, time, 1).unwrap();
            match vm
                .step(
                    &mut session,
                    SessionBudget::Exact(SliceBudget {
                        instructions: 3,
                        ..Default::default()
                    }),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    vm.dispatch(&mut session, ticket, "en", 1).unwrap();
                }
                SessionStep::Vm(RunOutcome::Returned(_)) => {
                    vm.discard_delivery(&mut session).unwrap()
                }
                SessionStep::Vm(RunOutcome::Fault(id)) => panic!(
                    "{:?}",
                    vm.format_fault_value(vm.vm.delivery_fault(id).unwrap().unwrap())
                ),
                SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => panic!("{error:?}"),
                _ => {}
            }
            let mut nodes=vm.stage().nodes().filter(|node|matches!(&node.kind,reel_ui::stage::NodeKind::Text(text) if text.as_ref()=="sample"));
            let first = nodes.next().unwrap();
            for node in nodes {
                assert_eq!(node.props.x, first.props.x);
                assert_eq!(node.props.y, first.props.y);
            }
            if first.props.x != last {
                assert!(first.props.x > last);
                last = first.props.x;
                publications += 1;
            }
        }
        assert!(publications >= 2, "continuous media subscription starved");
        flush(&mut vm, &mut session, 4.);
        for node in vm.stage().nodes() {
            if matches!(&node.kind,reel_ui::stage::NodeKind::Text(text) if text.as_ref()=="sample")
            {
                assert_eq!(node.props.x, 4.);
                assert_eq!(
                    node.props.y, 4.,
                    "media subscription starved ordinary clock"
                );
            }
        }
        assert!(
            vm.stage().nodes().any(|node| node.props.opacity == 0.5),
            "delay starved"
        );
    }
}
