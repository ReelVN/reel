use super::*;
use crate::{
    Epoch, InputAction, SessionId,
    aside::{VmLimits, source},
};

fn source_code(version: &str) -> String {
    format!(
        r#"
    use std::resume::{{state,root,flow,point}};
    class World {{ let mut score:int=0; }}
    class CurrentView {{
        let node:any; let callback:any;
        fn init(self,node,callback) {{self.node=node;self.callback=callback;}}
        fn version(self) {{"{version}"}}
        fn line(self, text:string) {{ flow("line"); point("say"); narrator(text); }}
        fn story(self,w:World) {{
            flow("opening");w.score+=100;
            let alias=self;
            point("first");narrator("First row");
            w.score+=1;
            point("second");self.line("{version} second row");
            if boots!=1||w.score!=108 {{throw "bootstrap or prefix replayed";}}
            if self!=alias||self.version()!="{version}"||self.callback()!="{version} current callback" {{throw "current context lost";}}
            w.score+=2;w.score
        }}
    }}
    state(World,"world"); root(start,"story");
    let mut boots=0;
    fn default_world() {{ let w=model(World());w.score=7;w }}
    fn start(w:World=default_world()) {{
        boots+=1;
        reading(|| {{
            let d=game.dialogue();
            let page=stack(){{text(derive(||d.text));text("{version} current view");}};
            let view=CurrentView(page,||"{version} current callback");
            stage.replace(page);
            run_flow(outer,w,context:view)
        }})
    }}
    fn outer(w:World,view:CurrentView) {{
        flow("outer"); let mut score:int=0;
        point("opening");let result=view.story(w);
        score=result;
        if view.version()!="{version}" {{throw "parent context lost";}} score
    }}
    "#
    )
}
fn fresh(code: &str, epoch: u64) -> (Session, SessionVm) {
    fresh_with_limits(code, epoch, VmLimits::default())
}

fn fresh_with_limits(code: &str, epoch: u64, limits: VmLimits) -> (Session, SessionVm) {
    let story = source::compile_story(code, "load", epoch, "start").unwrap();
    let initializer = story.initializer;
    let session = Session::new(SessionId(915), Epoch(epoch)).unwrap();
    let mut vm = SessionVm::load_story(&session, story, limits).unwrap();
    vm.start(&session, initializer).unwrap();
    (session, vm)
}
fn reach_wait(vm: &mut SessionVm, session: &mut Session) {
    for _ in 0..50_000 {
        match vm
            .step(
                session,
                SessionBudget::RetryMinimum(SliceBudget::default()),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(64))
                    .unwrap();
            }
            SessionStep::Vm(RunOutcome::Waiting(_)) | SessionStep::Idle => return,
            SessionStep::Vm(RunOutcome::Fault(_)) => panic!("fixture fault"),
            _ => (),
        }
    }
    panic!("fixture never waited");
}
fn advance(vm: &mut SessionVm, session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    vm.sync_reading(session).unwrap();
}

fn start_root(vm: &mut SessionVm, session: &mut Session) {
    loop {
        if let SessionStep::Vm(RunOutcome::Returned(_)) = vm
            .step(
                session,
                SessionBudget::RetryMinimum(SliceBudget::default()),
                || 0,
            )
            .unwrap()
        {
            vm.discard_delivery(session).unwrap();
            break;
        }
    }
    vm.start_root(session, None).unwrap();
}

#[test]
fn movable_load_restores_old_reading_then_runs_current_suffix_without_replaying_prefix() {
    let (mut session, mut vm) = fresh(&source_code("A"), 1);
    start_root(&mut vm, &mut session);
    reach_wait(&mut vm, &mut session);
    advance(&mut vm, &mut session);
    reach_wait(&mut vm, &mut session);
    let old_wait = vm.pending.as_ref().unwrap().wait;
    let history = session.view().history.len();
    session
        .reveal_more(
            session.root_owner(),
            session.view().interaction.unwrap().id(),
            3,
        )
        .unwrap();
    vm.sync_reading(&session).unwrap();
    let mut capture = vm.capture_checkpoint_begin(&session).unwrap();
    while !capture
        .step(&mut vm, &session, NativeBudget::RetryMinimum(1), 1)
        .unwrap()
    {}
    let captured = capture.finish(&mut vm).unwrap();
    assert!(
        captured.record().bindings()[0]
            .iter()
            .all(|binding| binding.key().0 != "score")
    );
    assert_eq!(
        captured
            .record()
            .bindings()
            .iter()
            .flatten()
            .filter(|binding| matches!(
                binding,
                crate::aside::checkpoint::CheckpointBinding::CurrentContext(_)
            ))
            .count(),
        3
    );
    let mut encoded = Vec::new();
    captured.record().write_to(&mut encoded).unwrap();
    let record = CheckpointRecord::decode(&encoded).unwrap();
    drop((captured, vm, session));

    let (mut session, mut vm) = fresh(&source_code("B"), 2);
    let mut load = CheckpointLoad::new(record, &vm).unwrap();
    let budget = SliceBudget {
        instructions: 1,
        ..Default::default()
    };
    let mut complete = false;
    for _ in 0..100_000 {
        if load
            .step(
                &mut vm,
                &mut session,
                "en",
                NativeBudget::RetryMinimum(1),
                1,
                budget,
                || 0,
            )
            .unwrap()
        {
            complete = true;
            break;
        }
    }
    assert!(complete, "load exceeded bounded turns");
    assert_ne!(vm.pending.as_ref().unwrap().wait, old_wait);
    assert_eq!(session.view().history.len(), history);
    let crate::InteractionView::Dialogue(dialogue) = session.view().interaction.unwrap() else {
        panic!("expected dialogue")
    };
    assert_eq!(
        dialogue.lines()[0].content.fallback.as_ref(),
        "A second row"
    );
    assert_eq!(dialogue.lines()[0].revealed_count, 3);
    advance(&mut vm, &mut session);
    for _ in 0..50_000 {
        match vm
            .step(
                &mut session,
                SessionBudget::RetryMinimum(SliceBudget::default()),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                vm.dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(64))
                    .unwrap();
            }
            SessionStep::Vm(RunOutcome::Returned(delivery)) => {
                assert_eq!(vm.vm.delivery_value(delivery).unwrap().as_int(), Some(110));
                return;
            }
            SessionStep::Vm(RunOutcome::Fault(_)) => panic!("current suffix failed"),
            _ => (),
        }
    }
    panic!("current suffix never returned");
}

#[test]
fn a_saved_dialogue_cannot_reconnect_to_a_different_wait_at_the_same_point() {
    let code = r#"
        use std::resume::{state,root,flow,point};
        use reel::types::Completion;
        class World { let score:int=0; }
        state(World,"world");root(start,"story");
        fn start(w:World=model(World())) { run_flow(story,w,context:completion()); }
        fn story(w:World,cue:Completion) {
            flow("story");
            point("line");
            READING_WAIT
        }
    "#;
    let old = code.replace(
        "READING_WAIT",
        "narrator(\"Saved dialogue\"); if !cue.pending { throw \"unexpected completion\"; }",
    );
    let (mut session, mut vm) = fresh(&old, 1);
    start_root(&mut vm, &mut session);
    reach_wait(&mut vm, &mut session);
    let mut capture = vm.capture_checkpoint_begin(&session).unwrap();
    while !capture
        .step(&mut vm, &session, NativeBudget::RetryMinimum(256), 256)
        .unwrap()
    {}
    let captured = capture.finish(&mut vm).unwrap();

    let current = code.replace("READING_WAIT", "await cue;");
    let (mut candidate_session, mut candidate) = fresh(&current, 2);
    let mut load = CheckpointLoad::new(captured.record().clone(), &candidate).unwrap();
    let failure = (0..1_024)
        .find_map(|_| {
            load.step(
                &mut candidate,
                &mut candidate_session,
                "en",
                NativeBudget::RetryMinimum(256),
                256,
                SliceBudget::default(),
                || 0,
            )
            .err()
        })
        .expect("the incompatible wait must terminate the restore");
    assert!(failure.to_string().contains("saved reading interaction"));
}

#[test]
fn load_returns_a_terminal_error_when_collection_cannot_free_enough_objects() {
    let (mut live_session, mut live) = fresh(&source_code("A"), 1);
    start_root(&mut live, &mut live_session);
    reach_wait(&mut live, &mut live_session);
    let mut capture = live.capture_checkpoint_begin(&live_session).unwrap();
    while !capture
        .step(
            &mut live,
            &live_session,
            NativeBudget::RetryMinimum(256),
            256,
        )
        .unwrap()
    {}
    let captured = capture.finish(&mut live).unwrap();

    let current = format!("let retained_for_restore = [1, 2];\n{}", source_code("B"));
    let (_, probe) = fresh(&current, 2);
    let initial_objects = probe.vm.heap().stats().live_objects;
    let mut limits = VmLimits::default();
    limits.heap.max_objects = initial_objects;
    let (mut candidate_session, mut candidate) = fresh_with_limits(&current, 2, limits);
    let mut load = CheckpointLoad::new(captured.record().clone(), &candidate).unwrap();
    let error = (0..1_024)
        .find_map(|_| {
            load.step(
                &mut candidate,
                &mut candidate_session,
                "en",
                NativeBudget::RetryMinimum(256),
                256,
                SliceBudget::default(),
                || 0,
            )
            .err()
        })
        .expect("the blocked candidate must terminate");
    let CheckpointError::ResourceBlocked(blocked) = error else {
        panic!("unexpected restore error: {error}");
    };
    assert_eq!(
        blocked.resource,
        aside_vm::executor::BlockedResource::HeapObjects
    );
    assert_eq!(candidate.vm.heap().stats().collections, 1);
    assert!(live_session.view().interaction.is_some());
}
