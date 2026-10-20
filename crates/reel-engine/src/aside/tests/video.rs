use super::*;
use crate::media::*;
use crate::video::{VideoPlaybackState, VideoRequest};
use reel_core::{
    identity::{AssetKey, ContentVersion, ResourceVersion},
    image::*,
};
use reel_ui::{
    resources::{ResourceLease, ResourceRegistry},
    stage::NodeKind,
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

// Engine contracts use a deterministic host; real codec/mixer tests live in
// reel-media and reel-player and do not require a graphical device.
struct Entry {
    state: VideoPlaybackState,
    frame: bool,
}
struct Backend {
    entries: BTreeMap<MediaId, Entry>,
    next: u64,
    frame: ResourceLease,
}
impl Backend {
    fn new() -> Self {
        #[derive(Debug)]
        struct Planes;
        impl Yuv420Storage for Planes {
            fn plane(&self, i: usize) -> &[u8] {
                if i == 0 { &[16; 4] } else { &[128] }
            }
        }
        let mut registry = ResourceRegistry::new(4, 16 << 10).unwrap();
        let image = Yuv420Image::new(
            [2, 2],
            YuvColor {
                matrix: YuvMatrix::Bt709,
                full_range: false,
                transfer: YuvTransfer::Bt709,
            },
            Arc::new(Planes),
        )
        .unwrap();
        let handle = registry
            .register_yuv420(
                ResourceVersion {
                    key: AssetKey("frame".into()),
                    version: ContentVersion([1; 32]),
                },
                image,
            )
            .unwrap();
        let frame = registry.acquire(handle).unwrap();
        registry.release(handle);
        Self {
            entries: BTreeMap::new(),
            next: 0,
            frame,
        }
    }
}
impl MediaBackend for Backend {
    fn commit_audio(&mut self, _: MediaId) {}
    fn play_audio(&mut self, _: crate::audio::AudioRequest) -> Result<MediaId, String> {
        Err("no audio fixture".into())
    }
    fn play_video(&mut self, request: VideoRequest) -> Result<MediaId, String> {
        self.restore_video(VideoPlaybackState {
            position: request.start_position,
            request,
            paused: false,
            result: None,
        })
    }
    fn restore_video(&mut self, state: VideoPlaybackState) -> Result<MediaId, String> {
        self.next += 1;
        let id = MediaId(self.next);
        self.entries.insert(id, Entry { state, frame: true });
        Ok(id)
    }
    fn snapshot(&self, id: MediaId) -> Result<MediaPlayback, String> {
        Ok(MediaPlayback::Video(self.entries[&id].state.clone()))
    }
    fn position(&self, id: MediaId) -> Option<Duration> {
        self.entries.get(&id).map(|e| e.state.position)
    }
    fn command(&mut self, id: MediaId, cmd: MediaCommand) -> Result<(), String> {
        let entry = self.entries.get_mut(&id).unwrap();
        match cmd {
            MediaCommand::Seek(t) => {
                entry.state.position = t;
                entry.frame = true;
            }
            MediaCommand::Pause => entry.state.paused = true,
            MediaCommand::Resume => entry.state.paused = false,
            MediaCommand::Stop => entry.state.result = Some(MediaResult::Stopped),
            MediaCommand::Finish if !entry.state.request.looping => {
                entry.state.position = entry
                    .state
                    .request
                    .end_position
                    .unwrap_or(Duration::from_secs(1));
                entry.state.result = Some(MediaResult::Completed);
                entry.frame = true;
            }
            _ => return Err("unsupported fixture command".into()),
        }
        Ok(())
    }
    fn take_video_frame(&mut self, id: MediaId) -> Option<ResourceLease> {
        let entry = self.entries.get_mut(&id)?;
        std::mem::take(&mut entry.frame).then(|| self.frame.clone())
    }
    fn group_volume(
        &mut self,
        _: Option<crate::audio::AudioGroup>,
        _: f64,
        _: Duration,
    ) -> Result<(), String> {
        Ok(())
    }
    fn cancel(&mut self, id: MediaId) {
        self.entries.remove(&id);
    }
    fn maintain(&mut self, elapsed: Duration) {
        for entry in self.entries.values_mut() {
            if entry.state.paused || entry.state.result.is_some() || elapsed.is_zero() {
                continue;
            }
            entry.state.position += elapsed;
            let end = entry
                .state
                .request
                .end_position
                .unwrap_or(Duration::from_secs(1));
            if entry.state.position >= end && !entry.state.request.looping {
                entry.state.position = end;
                entry.state.result = Some(MediaResult::Completed);
            }
            entry.frame = true;
        }
    }
    fn take_result(&mut self, id: MediaId) -> Option<MediaCompletion> {
        let entry = self.entries.get(&id)?;
        let result = entry.state.result.clone()?;
        let position = entry.state.position;
        self.entries.remove(&id);
        Some(MediaCompletion { result, position })
    }
    fn poll_after(&self) -> Option<Duration> {
        (!self.entries.is_empty()).then_some(Duration::from_millis(16))
    }
    fn suspend(&mut self) {}
    fn resume(&mut self) {}
    fn close(&mut self) {
        self.entries.clear();
    }
    fn cleanup_pending(&self) -> bool {
        false
    }
}
fn pump(vm: &mut SessionVm, session: &mut Session) {
    for _ in 0..50 {
        models::drive(vm, session, false);
        let media = vm.maintain_media().unwrap();
        let sampled = vm.advance_time(session, vm.logical_time(), 64).unwrap();
        if !media && !sampled.woke {
            return;
        }
    }
    panic!("media failed to settle");
}
fn advance(vm: &mut SessionVm, session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    pump(vm, session);
}
fn restore(vm: &mut SessionVm, session: &mut Session) {
    use aside_vm::snapshot::SnapshotBudget;
    let program = vm.vm.program().clone();
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let bytes = capture.finish().unwrap().encode().unwrap();
    let mut snapshot = snapshot::SessionSnapshot::decode(&bytes).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut snapshot,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(43),
            epoch: Epoch(session.identity().epoch.0 + 1),
        },
        Default::default(),
        Some(Box::new(Backend::new())),
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    (*session, *vm) = restore.finish().unwrap();
    vm.resume_media();
    pump(vm, session);
}
fn has_frame(vm: &SessionVm) -> bool {
    vm.stage()
        .nodes()
        .any(|n| matches!(&n.kind, NodeKind::Image { source, .. } if source.lease().is_some()))
}
#[test]
fn terminal_video_collected_before_publication_restores_its_picture() {
    for encoded in [false, true] {
        let source = r#"fn opening(){
            let movie=video("movie.mp4",width:32,height:18); stage.replace(movie);
            let playing=movie.play(); narrator("save");
            if await playing!="Completed" || video.position(playing)!=1 {throw "restored terminal";}
            narrator("done");
        }"#;
        let (mut session, mut vm) = models::prepared(source, encoded);
        vm.set_media(Box::new(Backend::new())).unwrap();
        pump(&mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 2., 16);
        vm.maintain_media().unwrap();
        assert_eq!(vm.media_poll_after(), Some(Duration::ZERO));
        restore(&mut vm, &mut session);
        assert!(has_frame(&vm));
        advance(&mut vm, &mut session);
        assert!(
            matches!(session.view().interaction,Some(crate::InteractionView::Dialogue(d)) if d.text()=="done")
        );
    }
}
#[test]
fn video_nodes_share_activity_ownership_and_restore_live_and_terminal_frames() {
    source::compile_story(
        include_str!("../../../../../examples/author_video.aside"),
        "author_video",
        1,
        "main",
    )
    .unwrap();
    let code = r#"fn opening() {
        let movie = video("movie.mp4", width: 320, height: 180);
        stage.replace(movie);
        let a = movie.play(start: 0.2, end: 0.8);
        let waiter = spawn(|| await a); waiter.cancel();
        narrator("playing");
        pause(a);
        narrator("paused");
        video.seek(a, 0.6);
        if video.position(a) != 0.6 { throw "seek position"; }
        resume(a);
        narrator("resumed");
        if await a != "Completed" || await a != "Completed" { throw "completion"; }
        narrator("finished");
        if video.position(a) != 0.8 { throw "terminal position"; }
        let looping = movie.play(looping: true);
        let mut rejected = false;
        try { finish(looping); } catch error { rejected = true; }
        if !rejected { throw "infinite finish"; }
        let replacement = movie.play();
        if await looping != "Stopped" { throw "replacement"; }
        finish(replacement);
        if await replacement != "Completed" { throw "finish"; }
        let retired = movie.play();
        movie.remove();
        let mut cancelled = false;
        try { await retired; } catch error { cancelled = true; }
        if !cancelled { throw "node owner"; }
        let video2 = video("second.mp4"); stage.replace(video2);
        let owner = stack();
        let owned = video2.play(owner: owner); owner.remove();
        cancelled = false;
        try { await owned; } catch error { cancelled = true; }
        if !cancelled { throw "explicit owner"; }
        stage.replace(text("passed"));
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        vm.set_media(Box::new(Backend::new())).unwrap();
        pump(&mut vm, &mut session);
        assert!(has_frame(&vm));
        collect(&mut vm);
        if encoded {
            restore(&mut vm, &mut session);
            assert!(has_frame(&vm));
        }
        advance(&mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 10., 16);
        pump(&mut vm, &mut session);
        advance(&mut vm, &mut session);
        activities::advance_clock(&mut vm, &session, 10.3, 16);
        pump(&mut vm, &mut session);
        advance(&mut vm, &mut session);
        assert!(has_frame(&vm));
        restore(&mut vm, &mut session);
        assert!(has_frame(&vm), "terminal video frame must survive a save");
        advance(&mut vm, &mut session);
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("passed".into()))
        );
        collect(&mut vm);
        vm.sync_stage(&session).unwrap();
        vm.maintain_media().unwrap();
        assert!(!vm.media.active());
    }
}
