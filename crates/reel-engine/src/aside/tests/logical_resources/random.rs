use super::*;
use crate::{
    InputAction,
    aside::{
        checkpoint::{CheckpointLoad, CheckpointRecord},
        data::Capture,
    },
};

const STORY: &str = r#"
    use std::Random;
    use std::resume::{state,root,flow,point};
    class World {
        let mut stream:Random|null=null;
        let mut alias:Random|null=null;
    }
    state(World,"world");root(main,"game");
    fn main(w:World=model(World())) {reading(||run_flow(story,w))}
    fn draws(stream,alias) {
        let integer=stream.int(0,1000000);
        let fraction=alias.float();
        let picked=stream.pick([17,19,23,29,31,37,41,43]);
        let values=[0,1,2,3,4,5,6,7];
        let mut i=7;
        while i>0 {
            let j=alias.int(0,i+1);
            let last=values[i];
            values[i]=values[j];
            values[j]=last;
            i-=1;
        }
        [integer,fraction,picked,values]
    }
    fn story(w:World) {
        flow("poem");
        w.stream=random(seed:42);
        w.alias=w.stream;
        let stream=w.stream;
        draws(stream,w.alias);
        point("between_rounds");narrator("Choose the next word.");
        if stream!=w.stream||stream!=w.alias {throw "random stream identity";}
        let initial=random(42);
        [draws(stream,w.alias),draws(w.alias,stream),draws(initial,initial),1]
    }
"#;

fn fresh(code: &str, version: u64, encoded: bool) -> (Session, SessionVm) {
    let mut story = source::compile_story(code, "random_checkpoint", version, "main").unwrap();
    if encoded {
        story.program = bc::VerifiedProgram::decode(
            &bc::encode(story.program.package(), Default::default()).unwrap(),
            Default::default(),
        )
        .unwrap();
        story.resume = story
            .program
            .verify_resume(
                story.resume.table().clone(),
                source::types(),
                Default::default(),
            )
            .unwrap();
    }
    let initializer = story.initializer;
    let session = Session::new(SessionId(701), Epoch(version)).unwrap();
    let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
    vm.start(&session, initializer).unwrap();
    (session, vm)
}

fn settle(vm: &mut SessionVm, session: &mut Session) -> RunOutcome {
    for _ in 0..128 {
        match next(vm, session) {
            RunOutcome::HostCall(ticket) => {
                vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(1))
                    .unwrap();
            }
            outcome => return outcome,
        }
    }
    panic!("random fixture did not settle");
}

fn continue_draws(vm: &mut SessionVm, session: &mut Session, version: i64) -> [Vec<u8>; 3] {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
        if session.completion_pending() {
            break;
        }
    }
    vm.sync_reading(session).unwrap();
    let outcome = settle(vm, session);
    let RunOutcome::Returned(delivery) = outcome else {
        panic!("expected continued random draws, got {outcome:?}");
    };
    let heap = vm.vm.heap();
    let result = vm.vm.delivery_value(delivery).unwrap();
    assert_eq!(
        heap.sequence_get(result, 3).unwrap().as_int(),
        Some(version)
    );
    std::array::from_fn(|index| {
        let batch = heap.sequence_get(result, index as u32).unwrap();
        let mut capture = Capture::new(heap, batch).unwrap();
        while !capture.step(heap, 1, 1).unwrap().0 {}
        let (document, _charge) = capture.finish().unwrap();
        let mut bytes = Vec::new();
        document.write_to(&mut bytes).unwrap();
        bytes
    })
}

#[test]
fn random_stream_resumes_draws_and_shuffle_across_story_versions_source_asb() {
    let current = STORY
        .replace("seed:42", "seed:716")
        .replace("draws(initial,initial),1", "draws(initial,initial),2");
    for encoded in [false, true] {
        let (mut session, mut old) = fresh(STORY, 1, encoded);
        assert!(matches!(
            settle(&mut old, &mut session),
            RunOutcome::Returned(_)
        ));
        old.discard_delivery(&mut session).unwrap();
        old.start_root(&session, None).unwrap();
        assert!(matches!(
            settle(&mut old, &mut session),
            RunOutcome::Waiting(_)
        ));
        old.sync_reading(&session).unwrap();

        let mut capture = old.capture_checkpoint_begin(&session).unwrap();
        while !capture
            .step(&mut old, &session, NativeBudget::RetryMinimum(1), 1)
            .unwrap()
        {}
        let saved = capture.finish(&mut old).unwrap();
        let mut bytes = Vec::new();
        saved.record().write_to(&mut bytes).unwrap();
        let record = CheckpointRecord::decode(&bytes).unwrap();
        let expected = continue_draws(&mut old, &mut session, 1);
        assert_ne!(expected[0], expected[2]);
        assert_ne!(expected[0], expected[1]);

        let (mut session, mut vm) = fresh(&current, 2, encoded);
        let mut load = CheckpointLoad::new(record, &vm).unwrap();
        assert!(
            (0..100_000).any(|_| {
                load.step(
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
            }),
            "random checkpoint exceeded its work budget"
        );
        assert_eq!(continue_draws(&mut vm, &mut session, 2), expected);
    }
}
