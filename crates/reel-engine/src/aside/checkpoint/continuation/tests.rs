use super::*;
use crate::aside::{
    NativeBudget, RunOutcome, SessionBudget, SessionStep, SessionVm, SliceBudget, VmLimits,
    data::NoReferences, source,
};
use crate::{Epoch, Session, SessionId};
use aside_vm::{
    Value, bytecode as bc,
    executor::HostResponse,
    resume::ResumeProgress,
    typed_data::{Identities, Registry},
};

const A: &str = r#"
    use std::resume::{flow, point, return_point};
    class Profile { let mut score:int=0; }
    fn child(state:Profile) {
        flow("child");
        point("line"); narrator("A dialogue");
        state.score
    }
    fn opening() {
        flow("root");
        let profile=Profile(); profile.score=41;
        return_point("child-result"); let result=child(profile);
        profile.score+result
    }
"#;
const B: &str = r#"
    use std::resume::{flow, point, return_point};
    let mut prefixes=0;
    fn unrelated(){900}
    class Profile {
        let extra:array=[5,6];
        let mut score:int=0;
        fn boost(self){self.score+self.extra[0]}
    }
    fn current_class(){Profile}
    fn opening() {
        flow("root"); prefixes+=100;
        let profile=Profile(); profile.score=1000;
        return_point("child-result"); let result=child(profile);
        if prefixes!=0 {throw "business prefix replayed";}
        profile.score+result+100
    }
    fn child(state:Profile) {
        flow("child"); prefixes+=100;
        point("line"); narrator("B current dialogue");
        state.score+=1;
        state.boost()
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
            outcome => panic!("unexpected continuation step: {outcome:?}"),
        }
    }
    panic!("continuation fixture exhausted its work budget")
}
fn initialized(code: &str, version: u64, entry: &str) -> (Session, SessionVm, bc::FunctionId) {
    let mut story = source::compile_story(code, "checkpoint", version, entry).unwrap();
    // Exercise runtime compact bytecode in both packages.
    story.program = bc::VerifiedProgram::decode(
        &story
            .program
            .encode(Default::default())
            .unwrap()
            .into_bytes(),
        Default::default(),
    )
    .unwrap();
    let (initializer, entry) = (story.initializer, story.entry);
    let mut session = Session::new(SessionId(333), Epoch(version)).unwrap();
    let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
    vm.start(&session, initializer).unwrap();
    assert!(matches!(
        next(&mut vm, &mut session),
        RunOutcome::Returned(_)
    ));
    vm.discard_delivery(&mut session).unwrap();
    (session, vm, entry)
}
fn registry(vm: &mut SessionVm, class: Value) -> Registry {
    let mut registry =
        Registry::new(vm.vm.heap_mut(), vm.root_owner, Identities::default()).unwrap();
    let index = registry.begin_class(vm.vm.heap_mut(), class).unwrap();
    while !registry.step_class(vm.vm.heap(), index, 1).unwrap() {}
    registry
}

#[test]
fn detached_record_restores_shared_current_state_and_two_current_call_frames() {
    let (mut old_session, mut old, entry) = initialized(A, 1, "opening");
    let task = old.start(&old_session, entry).unwrap();
    loop {
        match next(&mut old, &mut old_session) {
            RunOutcome::HostCall(ticket) => {
                old.dispatch(&mut old_session, ticket, "en", NativeBudget::Exact(64))
                    .unwrap();
            }
            RunOutcome::Waiting(_) => break,
            outcome => panic!("expected A dialogue: {outcome:?}"),
        }
    }
    let mut capture = old.vm.capture_resume(&old.resume, task).unwrap();
    let descriptor = capture.descriptor().clone();
    let mut keys = vec![Vec::new(); descriptor.frames.len()];
    let mut values = Vec::new();
    while let Some(binding) = capture.next_binding().unwrap() {
        keys[binding.frame].push(binding.key.clone());
        values.push(binding.value);
    }
    drop(capture);
    assert_eq!(values.len(), 2);
    assert!(
        values[0].same_bits(values[1]),
        "parent and child share the same State object"
    );
    let class = Registry::instance_class(old.vm.heap(), values[0]).unwrap();
    let old_registry = registry(&mut old, class);
    let roots = old.vm.heap().stats().roots;
    for work in [0, 1, 6] {
        let mut cancelled = old
            .capture_continuation_begin(task, &old_registry, &NoReferences)
            .unwrap();
        assert!(!cancelled.step(work, 1).unwrap());
        assert!(cancelled.finish().is_err());
        assert_eq!(
            old.vm.heap().stats().roots,
            roots,
            "incomplete capture releases every temporary root"
        );
    }
    let mut capture = old
        .capture_continuation_begin(task, &old_registry, &NoReferences)
        .unwrap();
    let mut slices = 0;
    while !capture.step(1, 1).unwrap() {
        slices += 1;
    }
    assert!(slices > values.len());
    let captured = capture.finish().unwrap();
    let record = captured.record();
    assert_eq!(record.continuation(), &descriptor);
    assert_eq!(record.bindings(), keys);
    let mut bytes = Vec::new();
    record.write_to(&mut bytes).unwrap();
    drop(captured);
    old_registry.release(old.vm.heap_mut()).unwrap();
    drop((old, old_session));

    let record = Arc::new(ContinuationRecord::decode(&bytes).unwrap());
    let (mut session, mut current, entry) = initialized(B, 2, "current_class");
    current.start(&session, entry).unwrap();
    let RunOutcome::Returned(delivery) = next(&mut current, &mut session) else {
        panic!("current class factory")
    };
    let class = current.vm.delivery_value(delivery).unwrap();
    let current_registry = registry(&mut current, class);
    current.discard_delivery(&mut session).unwrap();
    let roots = current.vm.heap().stats().roots;
    for steps in [0, 1, 24] {
        let mut cancelled = current
            .restore_continuation_begin(record.clone(), &current_registry, &NoReferences)
            .unwrap();
        for _ in 0..steps {
            assert!(matches!(
                cancelled.step(1, 1, SliceBudget::default(), || 0).unwrap(),
                ResumeProgress::BudgetYield(_)
            ));
        }
        assert!(cancelled.finish().is_err());
        assert_eq!(
            current.vm.heap().stats().roots,
            roots,
            "cancelled restoration releases temporary roots"
        );
    }
    let mut restore = current
        .restore_continuation_begin(record, &current_registry, &NoReferences)
        .unwrap();
    let mut budget = SliceBudget {
        instructions: 0,
        work: 1,
        allocation_bytes: 0,
        deadline_ns: None,
    };
    let mut slices = 0;
    loop {
        slices += 1;
        assert!(slices < 20_000);
        match restore.step(1, 1, budget, || 0).unwrap() {
            ResumeProgress::BudgetYield(usage) => {
                budget.work = budget.work.max(usage.required_work);
                budget.allocation_bytes = budget.allocation_bytes.max(usage.required_allocation);
            }
            ResumeProgress::Complete(_) => break,
        }
    }
    assert!(slices > 24);
    let resumed = restore.finish().unwrap();
    current_registry.release(current.vm.heap_mut()).unwrap();
    assert_eq!(current.task, Some(resumed));
    let RunOutcome::HostCall(ticket) = next(&mut current, &mut session) else {
        panic!("current B dialogue")
    };
    assert_eq!(
        crate::aside::text::read_string(
            current.vm.heap(),
            current.vm.host_arguments(ticket).unwrap()[0],
            1024
        )
        .unwrap(),
        "B current dialogue"
    );
    current
        .vm
        .respond(ticket, HostResponse::Ready(Value::NULL))
        .unwrap();
    current.issued = None;
    let RunOutcome::Returned(delivery) = next(&mut current, &mut session) else {
        panic!("current B return chain")
    };
    assert_eq!(
        current.vm.delivery_value(delivery).unwrap().as_int(),
        Some(189)
    );
}
