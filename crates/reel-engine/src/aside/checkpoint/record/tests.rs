//! Current Root bootstrap and a native business callback share one restored graph.
use super::*;
use crate::aside::{
    NativeBudget, NativeDispatch, RunOutcome, SessionBudget, SessionStep, SessionVm, SliceBudget,
    VmLimits, source,
};
use crate::{Epoch, Session, SessionId};
use aside_vm::{
    Value, bytecode as bc,
    executor::{HostResponse, HostTicket},
    resume::ResumeProgress,
};

const A: &str = r#"
use std::resume::{state,root,flow,point};
use reel::types::{Node,ImageNode};
class Profile {let mut score:int=0;let mut sprite:ImageNode|null=null;let mut alias:Node|null=null;}
state(Profile,"profile");root(main,"game");
fn new_profile(){
    let profile=model(Profile());
    profile.score=41;
    profile.sprite=image("sprite.png",x:12,width:100,height:50);
    profile.alias=profile.sprite;
    profile
}
fn main(profile:Profile=new_profile()){
    reading(||{stage.replace(stack(){profile.sprite;text(derive(||f"A {profile.score}"));});run_flow(opening,profile)})
}
fn opening(profile:Profile){
    flow("root"); point("child-result");let result=child(profile);
    profile.score+result
}
fn child(state:Profile){flow("child");point("line");narrator("A dialogue");state.score}
"#;
const B: &str = r#"
use std::resume::{state,root,flow,point};
use reel::types::{Node,ImageNode};
let mut prefixes=0;let mut boots=0;
fn unrelated(){900}
class Profile {
    let extra:array=[5,6];let mut alias:Node|null=null;let mut score:int=0;let mut sprite:ImageNode|null=null;
    fn init(self){throw "ordinary init replayed";}
    fn boost(self){self.score+self.extra[0]}
}
state(Profile,"profile");root(main,"game");
fn main(profile:Profile=model(Profile())){
    boots+=1;
    reading(||{
        profile.sprite.x=14;
        stage.replace(stack(){profile.sprite;text(derive(||f"B {profile.score}"));});
        run_flow(opening,profile)
    })
}
fn opening(profile:Profile){
    flow("root");prefixes+=100;let unused=900;
    point("child-result");let result=child(profile);
    if prefixes!=0||boots!=1 {throw "business prefix or bootstrap replayed";}
    profile.score+result+100
}
fn child(state:Profile){
    flow("child");prefixes+=100;point("line");narrator("B current dialogue");
    state.score+=1;state.boost()
}
"#;
fn next(vm: &mut SessionVm, session: &mut Session) -> RunOutcome {
    for _ in 0..50_000 {
        match vm
            .step(
                session,
                SessionBudget::Exact(SliceBudget {
                    instructions: 1,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap()
        {
            SessionStep::Vm(RunOutcome::BudgetYield(_)) | SessionStep::Progress => {}
            SessionStep::Vm(outcome) => return outcome,
            outcome => panic!("unexpected checkpoint step: {outcome:?}"),
        }
    }
    panic!("checkpoint fixture exceeded its work budget")
}
fn initialized(code: &str, version: u64) -> (Session, SessionVm) {
    let mut story = source::compile_story(code, "checkpoint", version, "main").unwrap();
    story.program = bc::VerifiedProgram::decode(
        &story
            .program
            .encode(Default::default())
            .unwrap()
            .into_bytes(),
        Default::default(),
    )
    .unwrap();
    let initializer = story.initializer;
    let mut session = Session::new(SessionId(333), Epoch(version)).unwrap();
    let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
    vm.start(&session, initializer).unwrap();
    assert!(matches!(
        next(&mut vm, &mut session),
        RunOutcome::Returned(_)
    ));
    vm.discard_delivery(&mut session).unwrap();
    (session, vm)
}
fn native(vm: &SessionVm, ticket: HostTicket) -> &str {
    let p = vm.vm.program().package();
    &p.symbols[p.natives[ticket.native.index()].symbol.index()]
}

#[test]
fn checkpoint_restores_current_root_shared_state_and_current_native_callback_chain() {
    let (mut session, mut old) = initialized(A, 1);
    old.start_root(&session, None).unwrap();
    loop {
        match next(&mut old, &mut session) {
            RunOutcome::HostCall(ticket) => {
                old.dispatch(&mut session, ticket, "en", NativeBudget::Exact(4096))
                    .unwrap();
            }
            RunOutcome::Waiting(_) => break,
            outcome => panic!("expected A dialogue: {outcome:?}"),
        }
    }
    let (task, boundary) = old.flow_boundary().unwrap();
    let mut probe = old.vm.capture_resume(&old.resume, task, boundary).unwrap();
    let mut saved_values = Vec::new();
    while let Some(binding) = probe.next_binding().unwrap() {
        saved_values.push(binding.value);
    }
    drop(probe);
    assert_eq!(saved_values.len(), 2);
    assert!(saved_values[0].same_bits(saved_values[1]));
    let roots = old.vm.heap().stats().roots;
    for work in [0, 1, 6] {
        let mut cancelled = old.capture_checkpoint_begin(&session).unwrap();
        assert!(
            !cancelled
                .step(&mut old, &session, NativeBudget::Exact(work), 1)
                .unwrap()
        );
        assert!(cancelled.finish(&mut old).is_err());
        assert_eq!(old.vm.heap().stats().roots, roots);
    }
    let mut capture = old.capture_checkpoint_begin(&session).unwrap();
    while !capture
        .step(&mut old, &session, NativeBudget::RetryMinimum(1), 1)
        .unwrap()
    {}
    let captured = capture.finish(&mut old).unwrap();
    let mut bytes = Vec::new();
    captured.record().write_to(&mut bytes).unwrap();
    drop(captured);
    drop((old, session));
    let record = Arc::new(CheckpointRecord::decode(&bytes).unwrap());
    assert_eq!(record.root().0, "game");
    for work in [0, 1, 24] {
        let (_, mut candidate) = initialized(B, 2);
        while !matches!(
            candidate
                .prepare_flow_registry(NativeBudget::Exact(1))
                .unwrap(),
            NativeDispatch::Ready { .. }
        ) {}
        let roots = candidate.vm.heap().stats().roots;
        {
            let mut cancelled = candidate
                .restore_checkpoint_data_begin(record.clone())
                .unwrap();
            for _ in 0..work {
                assert!(!cancelled.step(&mut candidate, 1, 1).unwrap());
            }
            assert!(cancelled.finish(&mut candidate).is_err());
        }
        assert_eq!(candidate.vm.heap().stats().roots, roots);
    }
    let (mut session, mut current) = initialized(B, 2);
    while !matches!(
        current
            .prepare_flow_registry(NativeBudget::Exact(1))
            .unwrap(),
        NativeDispatch::Ready { .. }
    ) {}
    let roots = current.vm.heap().stats().roots;
    let mut capture = crate::aside::data::TypedCapture::new(
        current.vm.heap_mut(),
        current.root_owner,
        Value::int(7),
    )
    .unwrap();
    let registry = current.flow.registry.as_ref().unwrap();
    while !capture
        .step(
            current.vm.heap(),
            registry,
            &crate::aside::data::NoReferences,
            1,
            1,
        )
        .unwrap()
        .0
    {}
    let (scalar, scalar_charge) = capture.finish(current.vm.heap_mut()).unwrap();
    let malformed = CheckpointRecord::new(
        record.root().clone(),
        record.continuation().clone(),
        record.bindings().to_vec(),
        scalar,
    )
    .unwrap();
    drop(scalar_charge);
    let mut data = current
        .restore_checkpoint_data_begin(Arc::new(malformed))
        .unwrap();
    while !data.step(&mut current, 1, 1).unwrap() {}
    assert!(data.finish(&mut current).is_err());
    assert_eq!(current.vm.heap().stats().roots, roots);

    let mut data = current.restore_checkpoint_data_begin(record).unwrap();
    while !data.step(&mut current, 1, 1).unwrap() {}
    let restored = data.finish(&mut current).unwrap();
    while !current.prepare_checkpoint_resources(1).unwrap() {}
    let state = restored.root_state(&current.vm).unwrap();
    let state_field = |current: &SessionVm, name: &str| {
        let heap = current.vm.heap();
        (0..heap.instance_field_count(state).unwrap())
            .find_map(|index| {
                let field = heap.instance_field(state, index).unwrap();
                (field.name == name).then(|| heap.cell_read(field.cell).unwrap())
            })
            .unwrap()
    };
    let sprite = state_field(&current, "sprite");
    assert!(sprite.same_bits(state_field(&current, "alias")));
    let id = current.stage.id(current.vm.heap(), sprite).unwrap();
    assert_eq!(current.stage.scene.node(id).unwrap().props.x, 12.);
    current.start_root(&session, Some(state)).unwrap();
    let ticket = loop {
        match next(&mut current, &mut session) {
            RunOutcome::HostCall(ticket) if native(&current, ticket) == "reel::run_flow" => {
                break ticket;
            }
            RunOutcome::HostCall(ticket) => {
                current
                    .dispatch(&mut session, ticket, "en", NativeBudget::Exact(4096))
                    .unwrap();
            }
            outcome => panic!("expected current business boundary: {outcome:?}"),
        }
    };
    assert!(matches!(
        current
            .prepare_flow_entry(&session, ticket, NativeBudget::Exact(4096))
            .unwrap(),
        crate::aside::checkpoint::FlowPreparation::Ready(_)
    ));
    let mut resume = current.resume_checkpoint_begin(ticket, &restored).unwrap();
    let mut budget = SliceBudget {
        instructions: 0,
        work: 1,
        allocation_bytes: 0,
        deadline_ns: None,
    };
    loop {
        match resume
            .step(&mut current.vm, &restored, 1, budget, || 0)
            .unwrap()
        {
            ResumeProgress::BudgetYield(usage) => {
                budget.work = budget.work.max(usage.required_work);
                budget.allocation_bytes = budget.allocation_bytes.max(usage.required_allocation);
            }
            ResumeProgress::Complete(task) => {
                assert_eq!(task, ticket.task);
                break;
            }
        }
    }
    resume.finish(&mut current.vm).unwrap();
    current.issued = None;
    let point = loop {
        match next(&mut current, &mut session) {
            RunOutcome::HostCall(ticket) if native(&current, ticket) == "reel::narrator" => {
                break ticket;
            }
            RunOutcome::HostCall(ticket) => {
                current
                    .dispatch(&mut session, ticket, "en", NativeBudget::Exact(4096))
                    .unwrap();
            }
            outcome => panic!("expected current dialogue: {outcome:?}"),
        }
    };
    assert_eq!(
        crate::aside::text::read_string(
            current.vm.heap(),
            current.vm.host_arguments(point).unwrap()[0],
            1024
        )
        .unwrap(),
        "B current dialogue"
    );
    while !current.adopt_checkpoint_resources(1).unwrap() {}
    current
        .vm
        .respond(point, HostResponse::Ready(Value::NULL))
        .unwrap();
    current.issued = None;
    let result = loop {
        match next(&mut current, &mut session) {
            RunOutcome::HostCall(ticket) => {
                current
                    .dispatch(&mut session, ticket, "en", NativeBudget::Exact(4096))
                    .unwrap();
            }
            RunOutcome::Returned(delivery) => break current.vm.delivery_value(delivery).unwrap(),
            outcome => panic!("current callback chain: {outcome:?}"),
        }
    };
    assert_eq!(result.as_int(), Some(189));
    assert_eq!(current.stage.scene.node(id).unwrap().props.x, 14.);
    assert!(current.flow_boundary().is_none());
    restored.release(&mut current.vm).unwrap();
}
