//! Candidate transports stay behind the host output gate until session adoption.
use super::*;
use reel_engine::{
    audio::{AudioGroup, AudioPlayback, AudioQueueItem, AudioRequest},
    media::{MediaBackend, MediaCommand, MediaCompletion, MediaId, MediaPlayback, MediaResult},
};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Saved,
    Live,
    Candidate,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Suspend,
    Prepare,
    Commit,
    TransportResume,
    OutputResume,
    Sound,
    Close,
}
#[derive(Debug)]
struct Event {
    role: Role,
    operation: Operation,
    gated: bool,
    live_audible: bool,
}
struct Adapter {
    role: Role,
    gated: bool,
    committed: bool,
    audible: bool,
    playback: Option<AudioPlayback>,
}
#[derive(Default)]
struct Trace {
    adapters: Vec<Adapter>,
    events: Vec<Event>,
}
struct Media {
    trace: Rc<RefCell<Trace>>,
    index: usize,
}
impl Media {
    fn new(trace: &Rc<RefCell<Trace>>, role: Role) -> Self {
        let mut state = trace.borrow_mut();
        let index = state.adapters.len();
        state.adapters.push(Adapter {
            role,
            gated: false,
            committed: false,
            audible: false,
            playback: None,
        });
        Self {
            trace: trace.clone(),
            index,
        }
    }
    fn record(&self, operation: Operation) {
        let mut state = self.trace.borrow_mut();
        let adapter = &state.adapters[self.index];
        let event = Event {
            role: adapter.role,
            operation,
            gated: adapter.gated,
            live_audible: state
                .adapters
                .iter()
                .any(|a| a.role == Role::Live && a.audible),
        };
        state.events.push(event);
    }
    fn update_output(&self) {
        let started = {
            let mut state = self.trace.borrow_mut();
            let adapter = &mut state.adapters[self.index];
            let audible = !adapter.gated
                && adapter.committed
                && adapter
                    .playback
                    .as_ref()
                    .is_some_and(|p| !p.paused && p.result.is_none());
            let started = audible && !adapter.audible;
            adapter.audible = audible;
            started
        };
        if started {
            self.record(Operation::Sound);
        }
    }
}
impl MediaBackend for Media {
    fn play_audio(&mut self, request: AudioRequest, paused: bool) -> Result<MediaId, String> {
        self.record(Operation::Prepare);
        self.trace.borrow_mut().adapters[self.index].playback = Some(AudioPlayback {
            request,
            queue: Vec::new(),
            paused,
            result: None,
        });
        Ok(MediaId(1))
    }
    fn commit_audio(&mut self, _: MediaId) {
        self.record(Operation::Commit);
        self.trace.borrow_mut().adapters[self.index].committed = true;
        self.update_output();
    }
    fn queue_audio(&mut self, _: MediaId, item: AudioQueueItem) -> Result<(), String> {
        self.trace.borrow_mut().adapters[self.index]
            .playback
            .as_mut()
            .unwrap()
            .queue
            .push(item);
        Ok(())
    }
    fn snapshot(&self, _: MediaId) -> Result<MediaPlayback, String> {
        Ok(MediaPlayback::Audio(
            self.trace.borrow().adapters[self.index]
                .playback
                .as_ref()
                .unwrap()
                .clone(),
        ))
    }
    fn position(&self, _: MediaId) -> Option<Duration> {
        self.trace.borrow().adapters[self.index]
            .playback
            .as_ref()
            .map(|p| p.request.start_position)
    }
    fn command(&mut self, _: MediaId, command: MediaCommand) -> Result<(), String> {
        if matches!(command, MediaCommand::Resume) {
            self.record(Operation::TransportResume);
        }
        let mut state = self.trace.borrow_mut();
        let playback = state.adapters[self.index].playback.as_mut().unwrap();
        match command {
            MediaCommand::Resume => playback.paused = false,
            MediaCommand::Pause => playback.paused = true,
            MediaCommand::Stop | MediaCommand::FadeOut(_) => {
                playback.result = Some(MediaResult::Stopped)
            }
            _ => {}
        }
        drop(state);
        self.update_output();
        Ok(())
    }
    fn group_volume(&mut self, _: Option<AudioGroup>, _: f64, _: Duration) -> Result<(), String> {
        Ok(())
    }
    fn cancel(&mut self, id: MediaId) {
        self.command(id, MediaCommand::Stop).unwrap();
    }
    fn maintain(&mut self, _: Duration) {}
    fn take_result(&mut self, _: MediaId) -> Option<MediaCompletion> {
        None
    }
    fn poll_after(&self) -> Option<Duration> {
        None
    }
    fn suspend(&mut self) {
        self.trace.borrow_mut().adapters[self.index].gated = true;
        self.update_output();
        self.record(Operation::Suspend);
    }
    fn resume(&mut self) {
        self.record(Operation::OutputResume);
        self.trace.borrow_mut().adapters[self.index].gated = false;
        self.update_output();
    }
    fn close(&mut self) {
        self.trace.borrow_mut().adapters[self.index].audible = false;
        self.record(Operation::Close);
    }
    fn cleanup_pending(&self) -> bool {
        false
    }
}

fn media_source(current: bool, fail: bool) -> String {
    let code = source(current).replace(
        "let mut bootstraps:int=0;",
        "let mut music:any=null;let mut bootstraps:int=0;",
    );
    if current {
        let code = code.replace(
            "stage.replace(menu(\"B title\"));",
            "let live=audio.play(\"live.ogg\");stage.replace(menu(\"B title\"));",
        );
        if fail {
            code.replace(
                "stage.replace(page);run_flow",
                "throw \"candidate Root failed\";stage.replace(page);run_flow",
            )
        } else {
            code
        }
    } else {
        code.replace(
            "world.primary.score+=5;",
            "world.music=audio.play(\"saved.ogg\");world.primary.score+=5;",
        )
    }
}
fn with_media(
    code: &str,
    version: u64,
    storage: Arc<DirectorySaves>,
    trace: &Rc<RefCell<Trace>>,
    role: Role,
) -> StoryRuntime {
    let mut story = runtime(code, version, storage);
    story
        .driver_mut()
        .unwrap()
        .adapter_mut()
        .set_media(Box::new(Media::new(trace, role)))
        .unwrap();
    let trace = trace.clone();
    story.set_media_factory(move || Box::new(Media::new(&trace, Role::Candidate)));
    story
}

#[test]
fn candidate_media_is_gated_through_prepare_and_failure_keeps_live_output() {
    let folder = folder();
    let storage = Arc::new(DirectorySaves::new(&folder, 64 << 20));
    let saved_trace = Rc::new(RefCell::new(Trace::default()));
    let mut old = with_media(
        &media_source(false, false),
        1,
        storage.clone(),
        &saved_trace,
        Role::Saved,
    );
    drain(&mut old);
    let mut screen = Screen::new(&old);
    assert!(screen.present(&mut old));
    screen.click(&mut old, 40.);
    drain(&mut old);
    has_text(&old, "saved");
    close(&mut old);

    for fail in [false, true] {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut current = with_media(
            &media_source(true, fail),
            27,
            storage.clone(),
            &trace,
            Role::Live,
        );
        drain(&mut current);
        assert!(trace.borrow().adapters[0].audible);
        let identity = current.driver().ui().session().identity();
        let mut screen = Screen::new(&current);
        assert!(screen.present(&mut current));
        screen.click(&mut current, 140.);
        let deadline = Instant::now() + Duration::from_secs(120);
        while current.needs_tick() {
            assert!(Instant::now() < deadline, "media checkpoint did not settle");
            current.tick(SliceBudget::default(), || 0).unwrap();
            let released = trace
                .borrow()
                .events
                .iter()
                .any(|e| e.role == Role::Candidate && e.operation == Operation::OutputResume);
            assert_eq!(
                released,
                current.driver().ui().session().identity() != identity
            );
            std::thread::yield_now();
        }
        {
            let trace = trace.borrow();
            let candidate: Vec<_> = trace
                .events
                .iter()
                .filter(|e| e.role == Role::Candidate)
                .collect();
            assert_eq!(candidate[0].operation, Operation::Suspend);
            assert_eq!(candidate[1].operation, Operation::Prepare);
            assert!(
                candidate
                    .iter()
                    .take_while(|e| e.operation != Operation::OutputResume)
                    .all(|e| e.gated && e.live_audible)
            );
            assert_eq!(
                trace.adapters[1].playback.as_ref().unwrap().request.source,
                "saved.ogg"
            );
            if fail {
                assert!(trace.adapters[0].audible);
                assert!(!trace.adapters[1].audible);
                assert!(
                    !candidate
                        .iter()
                        .any(|e| matches!(e.operation, Operation::Sound | Operation::OutputResume))
                );
                assert_eq!(current.driver().ui().session().identity(), identity);
                has_text(&current, "B title");
                has_text(&current, "load failed");
            } else {
                let operations: Vec<_> = candidate.iter().map(|e| e.operation).collect();
                assert_eq!(
                    operations,
                    [
                        Operation::Suspend,
                        Operation::Prepare,
                        Operation::Commit,
                        Operation::TransportResume,
                        Operation::OutputResume,
                        Operation::Sound
                    ]
                );
                assert!(trace.adapters[1].audible);
                assert!(!trace.adapters[0].audible);
                assert_eq!(dialogue(&current).0, OLD_LINE);
            }
        }
        close(&mut current);
    }
    storage.delete("slot").unwrap();
    std::fs::remove_dir(folder).unwrap();
}
