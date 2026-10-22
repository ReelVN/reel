//! Measure position publication together with actual derived native reads.
use reel_engine::{
    Epoch, InputAction, Session, SessionId,
    aside::{self, RunOutcome, SessionBudget, SessionStep, SessionVm, SliceBudget, VmLimits},
    audio::{AudioGroup, AudioRequest},
    media::{MediaBackend, MediaCommand, MediaCompletion, MediaId, MediaPlayback},
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{
    alloc::System,
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

struct Host(Rc<Cell<f64>>);
impl MediaBackend for Host {
    fn play_audio(&mut self, _: AudioRequest, _: bool) -> Result<MediaId, String> {
        Ok(MediaId(1))
    }
    fn commit_audio(&mut self, _: MediaId) {}
    fn snapshot(&self, _: MediaId) -> Result<MediaPlayback, String> {
        Err("not used".into())
    }
    fn position(&self, _: MediaId) -> Option<Duration> {
        Some(Duration::from_secs_f64(self.0.get()))
    }
    fn command(&mut self, _: MediaId, command: MediaCommand) -> Result<(), String> {
        if let MediaCommand::Seek(value) = command {
            self.0.set(value.as_secs_f64());
        }
        Ok(())
    }
    fn group_volume(&mut self, _: Option<AudioGroup>, _: f64, _: Duration) -> Result<(), String> {
        Ok(())
    }
    fn cancel(&mut self, _: MediaId) {}
    fn maintain(&mut self, _: Duration) {}
    fn take_result(&mut self, _: MediaId) -> Option<MediaCompletion> {
        None
    }
    fn poll_after(&self) -> Option<Duration> {
        Some(Duration::from_millis(10))
    }
    fn suspend(&mut self) {}
    fn resume(&mut self) {}
    fn close(&mut self) {}
    fn cleanup_pending(&self) -> bool {
        false
    }
}

fn initially_paused_activities_have_no_per_sample_allocation() {
    let source = r#"fn opening() {
        let node=stack();stage.replace(node);
        run(tween(node,x:100,duration:1),paused:true);
        clock(interval:0.01,paused:true);
        narrator("paused");
    }"#;
    let mut session = Session::new(SessionId(2), Epoch(1)).unwrap();
    let story = aside::source::compile_story(source, "paused_cost", 1, "opening").unwrap();
    let mut vm = SessionVm::load(&session, story.program, VmLimits::default()).unwrap();
    for entry in [story.initializer, story.entry] {
        vm.start(&session, entry).unwrap();
        drive(&mut vm, &mut session);
    }
    let before = vm.vm().heap().stats().chunk_allocations;
    let region = Region::new(GLOBAL);
    for i in 1..=120 {
        assert!(!vm.advance_time(&session, i as f64, 1024).unwrap().pending);
    }
    let measured = region.change();
    let chunks = vm.vm().heap().stats().chunk_allocations - before;
    eprintln!(
        "paused samples=120; allocations={}; reallocations={}; VM new_chunks={chunks}",
        measured.allocations, measured.reallocations
    );
    assert_eq!(
        (measured.allocations, measured.reallocations, chunks),
        (0, 0, 0)
    );
    assert!(!vm.activities_need_time());
    assert_eq!(vm.activities_poll_after(), None);
}

#[test]
fn media_position_sampling_and_observer_cost() {
    // One test owns the process-wide allocator while all scenarios are measured.
    initially_paused_activities_have_no_per_sample_allocation();
    let mut baseline = [(0, 0, 0, 0); 3];
    for kind in ["clock", "media"] {
        for (index, consumers) in [0, 1, 64].into_iter().enumerate() {
            let read = if kind == "clock" {
                "timer.elapsed"
            } else {
                "audio.position(media)"
            };
            let source = format!(
                r#"fn opening() {{
            let media=audio.play("measure");
            let timer=clock(interval:0.000001);
            let root=stack();
            for i in 0..{consumers} {{
                root.add(stack(width:1,height:1,x:derive(||{read})));
            }}
            stage.replace(root); narrator("measure");
        }}"#
            );
            let position = Rc::new(Cell::new(0.));
            let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
            let story =
                aside::source::compile_story(&source, "media_allocations", 1, "opening").unwrap();
            let mut vm = SessionVm::load(&session, story.program, VmLimits::default()).unwrap();
            vm.set_media(Box::new(Host(position.clone()))).unwrap();
            for entry in [story.initializer, story.entry] {
                vm.start(&session, entry).unwrap();
                drive(&mut vm, &mut session);
            }
            for tick in 1..=16 {
                sample(&mut vm, &mut session, &position, tick);
            }
            let heap = vm.vm().heap().stats();
            let region = Region::new(GLOBAL);
            let started = Instant::now();
            let mut sample_allocations = 0;
            let mut sample_reallocations = 0;
            for tick in 17..=136 {
                let (allocations, reallocations) = sample(&mut vm, &mut session, &position, tick);
                sample_allocations += allocations;
                sample_reallocations += reallocations;
            }
            let elapsed = started.elapsed();
            let measured = region.change();
            let chunks = vm.vm().heap().stats().chunk_allocations - heap.chunk_allocations;
            let counts = (
                measured.allocations,
                measured.reallocations,
                measured.bytes_allocated,
                chunks,
            );
            if kind == "clock" {
                baseline[index] = counts;
            } else {
                assert_eq!(
                    counts, baseline[index],
                    "media reads added allocations to the same observer workload"
                );
            }

            for node in vm
                .stage()
                .nodes()
                .filter(|node| node.props.width == Some(1.))
            {
                let expected = if kind == "clock" {
                    (136_f64 / 60. / 0.000001).floor() * 0.000001
                } else {
                    Duration::from_secs_f64(136. / 60.).as_secs_f64()
                };
                assert!((node.props.x - expected).abs() < 1e-10);
            }
            assert_eq!(
                (sample_allocations, sample_reallocations),
                (0, 0),
                "position/time publication allocated"
            );
            eprintln!(
                "source={kind}; consumers={consumers}; samples=120; elapsed={elapsed:?}; publication allocations={sample_allocations}/{sample_reallocations}; observers allocations={}; reallocations={}; bytes={}; VM new_chunks={chunks}",
                measured.allocations, measured.reallocations, measured.bytes_allocated
            );
        }
    }
    for pending in [0, 64, 1024] {
        let source = format!(
            r#"fn opening() {{
            let media=audio.play("measure");
            for i in 0..{pending} {{audio.at(media,100000);}}
            narrator("ready");
            for i in 0..60 {{audio.seek(media,1);audio.seek(media,0);}}
            narrator("done");
        }}"#
        );
        let position = Rc::new(Cell::new(0.));
        let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
        let story = aside::source::compile_story(&source, "media_seek", 1, "opening").unwrap();
        let mut vm = SessionVm::load(&session, story.program, VmLimits::default()).unwrap();
        vm.set_media(Box::new(Host(position))).unwrap();
        for entry in [story.initializer, story.entry] {
            vm.start(&session, entry).unwrap();
            drive(&mut vm, &mut session);
        }
        let id = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap();
        }
        let region = Region::new(GLOBAL);
        let started = Instant::now();
        drive(&mut vm, &mut session);
        let elapsed = started.elapsed();
        let measured = region.change();
        eprintln!(
            "seek pending_cues={pending}; calls=120; elapsed={elapsed:?}; allocations={}; reallocations={}",
            measured.allocations, measured.reallocations
        );
        assert!(
            matches!(session.view().interaction,Some(reel_engine::InteractionView::Dialogue(d)) if d.text()=="done")
        );
    }
}
fn sample(
    vm: &mut SessionVm,
    session: &mut Session,
    position: &Cell<f64>,
    tick: u32,
) -> (usize, usize) {
    let time = f64::from(tick) / 60.;
    position.set(time);
    let region = Region::new(GLOBAL);
    vm.maintain_media().unwrap();
    vm.advance_time(session, time, 1024).unwrap();
    let publish = region.change();
    drive(vm, session);
    (publish.allocations, publish.reallocations)
}
fn drive(vm: &mut SessionVm, session: &mut Session) {
    for _ in 0..100_000 {
        match vm
            .step(
                session,
                SessionBudget::RetryMinimum(SliceBudget::default()),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                vm.dispatch(
                    session,
                    ticket,
                    "en",
                    reel_engine::aside::NativeBudget::Exact(1024),
                )
                .unwrap();
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Vm(RunOutcome::Fault(id)) => panic!("position fault: {id:?}"),
            SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => panic!("{error:?}"),
            SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                if !vm.has_runnable_tasks() && !vm.activities_pending() =>
            {
                vm.sync_stage(session).unwrap();
                return;
            }
            _ => {}
        }
    }
    panic!("position observer did not settle");
}
