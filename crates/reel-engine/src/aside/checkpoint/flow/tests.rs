//! Real Root source exercises native budget preparation and callback retirement.
use super::*;
use crate::aside::{RunOutcome, SessionBudget, SessionStep, SliceBudget, VmLimits, source};
use crate::{Epoch, InputAction, SessionId};
use aside_vm::{bytecode as bc, executor::FaultKind};

const HEADER: &str = r#"
    use std::resume::{state,root,flow,point};
    class World { let mut score:int=0; }
    state(World,"world"); root(main,"game");
    fn story(value:World) {
        flow("story"); value.score+=1;
        point("line"); narrator("A current custom view");
        value.score+40
    }
"#;
fn prepared(code: &str, encoded: bool) -> (Session, SessionVm, bc::FunctionId) {
    let mut story = source::compile_story(code, "flow", 1, "main").unwrap();
    if encoded {
        let bytes = story
            .program
            .encode(Default::default())
            .unwrap()
            .into_bytes();
        story.program = bc::VerifiedProgram::decode(&bytes, Default::default()).unwrap();
        story.resume = story
            .program
            .verify_resume(
                story.resume.table().clone(),
                crate::aside::source::types(),
                Default::default(),
            )
            .unwrap();
    }
    let (initializer, entry) = (story.initializer, story.entry);
    let mut session = Session::new(SessionId(440), Epoch(1)).unwrap();
    let mut vm = SessionVm::load_story(&session, story, VmLimits::default()).unwrap();
    vm.start(&session, initializer).unwrap();
    assert!(matches!(
        next(&mut vm, &mut session),
        RunOutcome::Returned(_)
    ));
    vm.discard_delivery(&mut session).unwrap();
    vm.start(&session, entry).unwrap();
    (session, vm, entry)
}
fn next(vm: &mut SessionVm, session: &mut Session) -> RunOutcome {
    for _ in 0..50_000 {
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
            other => panic!("unexpected flow step: {other:?}"),
        }
    }
    panic!("flow fixture exceeded its step budget")
}
fn symbol(vm: &SessionVm, ticket: HostTicket) -> &str {
    let p = vm.vm.program().package();
    &p.symbols[p.natives[ticket.native.index()].symbol.index()]
}
fn reach_entry(vm: &mut SessionVm, session: &mut Session) -> HostTicket {
    loop {
        match next(vm, session) {
            RunOutcome::HostCall(ticket) if symbol(vm, ticket) == "reel::run_flow" => {
                return ticket;
            }
            RunOutcome::HostCall(ticket) => {
                vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(1))
                    .unwrap();
            }
            other => panic!("expected run_flow: {other:?}"),
        }
    }
}
fn enter(vm: &mut SessionVm, session: &mut Session, ticket: HostTicket) -> usize {
    let mut slices = 0;
    loop {
        slices += 1;
        match vm
            .dispatch(session, ticket, "en", NativeBudget::Exact(1))
            .unwrap()
        {
            NativeDispatch::Decoding { work } => assert_eq!(work, 1),
            NativeDispatch::BudgetYield { required_work, .. } => {
                assert!(required_work > 1);
                assert!(matches!(
                    vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(1))
                        .unwrap(),
                    NativeDispatch::Ready { .. }
                ));
                break;
            }
            NativeDispatch::Ready { .. } => break,
            other => panic!("unexpected flow entry preparation: {other:?}"),
        }
        assert_eq!(vm.issued, Some(ticket));
        assert!(vm.flow.active.is_none());
        assert!(slices < 10_000);
    }
    slices
}
fn reach_wait(vm: &mut SessionVm, session: &mut Session) -> aside_vm::executor::WaitId {
    loop {
        match next(vm, session) {
            RunOutcome::HostCall(ticket) => {
                vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(1))
                    .unwrap();
            }
            RunOutcome::Waiting(wait) => return wait,
            other => panic!("expected narrative wait: {other:?}"),
        }
    }
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
fn finish(vm: &mut SessionVm, session: &mut Session) -> RunOutcome {
    loop {
        match next(vm, session) {
            RunOutcome::HostCall(ticket) => {
                vm.dispatch(session, ticket, "en", NativeBudget::RetryMinimum(1))
                    .unwrap();
            }
            terminal @ (RunOutcome::Returned(_) | RunOutcome::Fault(_)) => return terminal,
            other => panic!("expected flow completion: {other:?}"),
        }
    }
}

#[test]
fn flow_entry_registers_once_and_returns_in_its_current_reading_task_source_asb() {
    let code = format!(
        r#"{HEADER}
        fn main(state:World=model(World())){{
            let host=stack();stage.replace(host);
            let result=reading(body:|page|{{
                let dialogue=game.dialogue();
                page.add(host,text(value:derive(||dialogue.text)));
                run_flow(story,state)
            }});
            if result!=41{{throw "flow result";}} true
        }}"#
    );
    for encoded in [false, true] {
        let (mut session, mut vm, _) = prepared(&code, encoded);
        let ticket = reach_entry(&mut vm, &mut session);
        assert_eq!(vm.task, Some(ticket.task));
        assert!(matches!(
            vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(0))
                .unwrap(),
            NativeDispatch::BudgetYield {
                work: 0,
                required_work: 1
            }
        ));
        assert!(vm.flow.registry_init.is_none());
        assert!(enter(&mut vm, &mut session, ticket) > 3);
        assert_eq!(vm.flow_boundary(), Some((ticket.task, ticket.frame)));
        assert_eq!(vm.flow_root_key().unwrap().0, "game");
        let state = vm.flow_root_state().unwrap().unwrap();
        let root = vm.flow.active.as_ref().unwrap().root_state;
        assert!(vm.flow_registry().is_some());
        assert!(vm.tasks.is_empty());
        let wait = reach_wait(&mut vm, &mut session);
        assert_eq!(vm.task, Some(ticket.task));
        assert!(vm.current_scope(ticket.task).unwrap().is_some());
        assert_eq!(vm.narrative_contexts.frames.len(), 1);
        assert!(vm.flow_root_state().unwrap().unwrap().same_bits(state));
        assert!(vm.vm.wait_is_pending(wait));
        advance(&mut vm, &mut session);
        let RunOutcome::Returned(delivery) = finish(&mut vm, &mut session) else {
            panic!()
        };
        assert_eq!(
            vm.vm.delivery_value(delivery).unwrap().as_bool(),
            Some(true)
        );
        vm.discard_delivery(&mut session).unwrap();
        assert!(vm.flow_boundary().is_none());
        assert!(vm.flow_root_key().is_none());
        assert!(vm.flow_root_state().unwrap().is_none());
        assert!(vm.vm.heap().root_value(root).is_err());
        assert!(vm.narrative_contexts.frames.is_empty());
        assert!(matches!(
            vm.prepare_flow_registry(NativeBudget::Exact(0)).unwrap(),
            NativeDispatch::Ready { work: 0 }
        ));
        vm.revoke(&mut session).unwrap();
        assert!(vm.flow.registry.is_none());
        assert!(vm.flow.registry_init.is_none());
    }
}

#[test]
fn flow_fault_forwards_and_releases_its_independent_state_root() {
    let code = format!(
        r#"{HEADER}
        fn fail(value:World){{flow("fail");throw "flow fault";}}
        fn main(state:World=model(World())){{
            run_flow(fail,state)
        }}"#
    );
    let (mut session, mut vm, _) = prepared(&code, false);
    let entry = reach_entry(&mut vm, &mut session);
    enter(&mut vm, &mut session, entry);
    let root = vm.flow.active.as_ref().unwrap().root_state;
    let RunOutcome::HostCall(callback) = next(&mut vm, &mut session) else {
        panic!()
    };
    assert_eq!(callback.task, entry.task);
    assert_eq!(callback.frame, entry.frame);
    assert_eq!(callback.input, NativeInput::CallbackFault);
    assert!(matches!(
        vm.dispatch(&mut session, callback, "en", NativeBudget::Exact(0))
            .unwrap(),
        NativeDispatch::BudgetYield {
            work: 0,
            required_work: 1
        }
    ));
    assert!(vm.flow_root_state().unwrap().is_some());
    vm.dispatch(&mut session, callback, "en", NativeBudget::Exact(1))
        .unwrap();
    assert!(vm.flow_boundary().is_none());
    assert!(vm.vm.heap().root_value(root).is_err());
    let RunOutcome::Fault(delivery) = finish(&mut vm, &mut session) else {
        panic!()
    };
    assert_eq!(
        vm.vm.delivery_fault(delivery).unwrap().unwrap().kind,
        FaultKind::Thrown
    );
    vm.discard_delivery(&mut session).unwrap();
}

#[test]
fn outer_page_finish_retires_flow_even_when_its_native_callback_is_skipped() {
    let code = format!(
        r#"{HEADER}
        fn main(state:World=model(World())){{
            reading(body:|page|{{
                stage.replace(button("Leave",on_click:||page.finish(77)));
                run_flow(story,state)
            }})
        }}"#
    );
    let (mut session, mut vm, _) = prepared(&code, true);
    let entry = reach_entry(&mut vm, &mut session);
    enter(&mut vm, &mut session, entry);
    let root = vm.flow.active.as_ref().unwrap().root_state;
    let wait = reach_wait(&mut vm, &mut session);
    vm.sync_stage(&session).unwrap();
    let leave = vm
        .stage()
        .nodes()
        .find_map(|node| match &node.kind {
            reel_ui::stage::NodeKind::Button {
                label: Some(label), ..
            } if label == "Leave" => Some(node.id),
            _ => None,
        })
        .unwrap();
    assert!(vm.click(&mut session, leave).unwrap());
    let RunOutcome::Returned(delivery) = finish(&mut vm, &mut session) else {
        panic!()
    };
    assert_eq!(vm.vm.delivery_value(delivery).unwrap().as_int(), Some(77));
    assert!(!vm.vm.wait_is_pending(wait));
    assert!(vm.flow_boundary().is_none());
    assert!(vm.vm.heap().root_value(root).is_err());
    assert!(vm.narrative_contexts.frames.is_empty());
    vm.discard_delivery(&mut session).unwrap();
}

#[test]
fn native_callback_cancellation_releases_state_and_invalidates_the_old_wait() {
    let code = format!(
        r#"{HEADER}
        fn main(state:World=model(World())){{
            run_flow(story,state)
        }}"#
    );
    let (mut session, mut vm, _) = prepared(&code, false);
    let entry = reach_entry(&mut vm, &mut session);
    enter(&mut vm, &mut session, entry);
    let root = vm.flow.active.as_ref().unwrap().root_state;
    let wait = reach_wait(&mut vm, &mut session);
    assert!(vm.vm.cancel_host_callback(entry.task, entry.frame).unwrap());
    assert!(!vm.vm.wait_is_pending(wait));
    let RunOutcome::Fault(delivery) = finish(&mut vm, &mut session) else {
        panic!()
    };
    assert_eq!(
        vm.vm.delivery_fault(delivery).unwrap().unwrap().kind,
        FaultKind::Cancelled
    );
    assert!(vm.flow_boundary().is_none());
    assert!(vm.vm.heap().root_value(root).is_err());
    vm.discard_delivery(&mut session).unwrap();
}

#[test]
fn flow_checks_declared_entry_and_root_state_and_cancels_registry_preparation() {
    for body in [
        "run_flow(ordinary,model(World()))",
        "run_flow(story,model(Other()))",
    ] {
        let code = format!(
            r#"{HEADER}
            class Other{{let mut score:int=0;}}state(Other,"other");
            fn ordinary(value){{123}}
            fn main(state:World=model(World())){{{body}}}"#
        );
        let (mut session, mut vm, _) = prepared(&code, false);
        let ticket = reach_entry(&mut vm, &mut session);
        loop {
            match vm.dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(1)) {
                Ok(NativeDispatch::Decoding { .. }) | Ok(NativeDispatch::BudgetYield { .. }) => {}
                Err(error) => {
                    assert!(
                        error.to_string().contains(if body.contains("ordinary") {
                            "declared current flow"
                        } else {
                            "Root State"
                        }),
                        "{error}"
                    );
                    break;
                }
                other => panic!("invalid flow accepted: {other:?}"),
            }
        }
        assert!(vm.flow_boundary().is_none());
        assert_eq!(vm.issued, Some(ticket));
        vm.revoke(&mut session).unwrap();
        assert!(vm.flow.registry.is_none());
    }
    let code =
        format!(r#"{HEADER}fn main(state:World=model(World())){{run_flow(story,model(World()))}}"#);
    let (mut session, mut vm, _) = prepared(&code, false);
    let ticket = reach_entry(&mut vm, &mut session);
    let roots = vm.vm.heap().stats().roots;
    assert!(matches!(
        vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(1))
            .unwrap(),
        NativeDispatch::Decoding { work: 1 }
    ));
    assert!(vm.flow.registry_init.is_some());
    assert_eq!(vm.vm.heap().stats().roots, roots + 1);
    vm.revoke(&mut session).unwrap();
    assert!(vm.flow.registry_init.is_none());
    assert_eq!(vm.vm.heap().stats().roots, roots);
}
