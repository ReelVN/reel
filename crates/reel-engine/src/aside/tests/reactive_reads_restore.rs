use super::*;
use aside_vm::snapshot::SnapshotBudget;
use reel_ui::stage::NodeKind;

const SOURCE: &str = r#"
fn slow(value) { let mut i=0; while i<12 {i+=1;} value }
fn opening(){
    let state=model({index:1,other:0});
    let order=derive(||f"{state.other}:{state.index}");
    let visible=derive(||slow(state.index>0));
    let result=derive(||if state.other>=0 && visible.value {["first"][state.index-1]} else {"hidden"});
    stage.replace(column(){
        text(value:order); text(value:result);
        button("hide",on_click:||{state.other+=1;state.index=0;});
    }); narrator("ready");
}"#;

fn restore(session: &Session, vm: &SessionVm) -> (Session, SessionVm) {
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut image =
        snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        vm.vm.program().clone(),
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(41),
            epoch: Epoch(session.identity().epoch.0 + 1),
        },
        Default::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    restore.finish().unwrap()
}

#[test]
fn nested_reactive_read_restores_and_collects_after_each_nested_budget_boundary() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(SOURCE, encoded);
        models::drive(&mut vm, &mut session, false);
        let button = vm
            .stage()
            .nodes()
            .find(|n| matches!(&n.kind,NodeKind::Button{label:Some(s),..} if s=="hide"))
            .unwrap()
            .id;
        vm.click(&mut session, button).unwrap();
        let mut nested = false;
        let mut captures = 0;
        let mut work = 1;
        for _ in 0..20_000 {
            let step = vm
                .step(
                    &mut session,
                    SessionBudget::Exact(SliceBudget {
                        instructions: 1,
                        work,
                        ..Default::default()
                    }),
                    || 0,
                )
                .unwrap();
            match step {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    if let (Some(task), Some(output)) =
                        (vm.derived_task, vm.vm.reactive_output(ticket).unwrap())
                    {
                        nested |= vm
                            .vm
                            .reactive_task_output(task)
                            .unwrap()
                            .is_some_and(|outer| !outer.same_bits(output));
                    }
                    vm.dispatch(&mut session, ticket, "en", 1).unwrap();
                }
                SessionStep::Vm(RunOutcome::BudgetYield(usage))
                    if nested && vm.derived_task.is_some() =>
                {
                    work = usage.required_work.max(1);
                    collect(&mut vm);
                    (session, vm) = restore(&session, &vm);
                    captures += 1;
                }
                SessionStep::Vm(RunOutcome::Returned(_)) => {
                    vm.discard_delivery(&mut session).unwrap()
                }
                SessionStep::Vm(RunOutcome::Fault(id)) => {
                    panic!("refresh fault: {:?}", vm.vm.delivery_fault(id).unwrap())
                }
                SessionStep::Vm(RunOutcome::BudgetYield(usage)) => {
                    work = usage.required_work.max(1);
                }
                _ => {}
            }
            if !vm.has_runnable_tasks()
                && vm.derived_task.is_none()
                && !vm.vm.heap().derived_pending()
            {
                vm.sync_stage(&session).unwrap();
                break;
            }
        }
        assert!(
            nested,
            "fixture must refresh an upstream inside a downstream task"
        );
        assert!(
            captures > 2,
            "exercise multiple independently restored continuation stages"
        );
        assert!(!vm.vm.heap().reactive_evaluating());
        assert!(
            vm.stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("hidden".into()))
        );
    }
}

#[test]
fn cancelling_nested_reactive_read_releases_both_capture_levels() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(SOURCE, encoded);
        models::drive(&mut vm, &mut session, false);
        let button = vm
            .stage()
            .nodes()
            .find(|n| matches!(&n.kind,NodeKind::Button{label:Some(s),..} if s=="hide"))
            .unwrap()
            .id;
        vm.click(&mut session, button).unwrap();
        let mut nested = false;
        let mut stopped = false;
        let mut work = 1;
        for _ in 0..20_000 {
            match vm
                .step(
                    &mut session,
                    SessionBudget::Exact(SliceBudget {
                        instructions: 1,
                        work,
                        ..Default::default()
                    }),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    if let (Some(task), Some(output)) =
                        (vm.derived_task, vm.vm.reactive_output(ticket).unwrap())
                    {
                        nested |= vm
                            .vm
                            .reactive_task_output(task)
                            .unwrap()
                            .is_some_and(|outer| !outer.same_bits(output));
                    }
                    vm.dispatch(&mut session, ticket, "en", 1).unwrap();
                }
                SessionStep::Vm(RunOutcome::BudgetYield(_)) if nested => {
                    (session, vm) = restore(&session, &vm);
                    collect(&mut vm);
                    vm.revoke(&mut session).unwrap();
                    stopped = true;
                    break;
                }
                SessionStep::Vm(RunOutcome::Returned(_)) => {
                    vm.discard_delivery(&mut session).unwrap()
                }
                SessionStep::Vm(RunOutcome::Fault(id)) => {
                    panic!("refresh fault: {:?}", vm.vm.delivery_fault(id).unwrap())
                }
                SessionStep::Vm(RunOutcome::BudgetYield(usage)) => {
                    work = usage.required_work.max(1);
                }
                _ => {}
            }
        }
        assert!(
            stopped,
            "cancel an admitted nested producer after restoration"
        );
        for _ in 0..20_000 {
            if session.owner_lifecycle(session.root_owner()) == Some(OwnerLifecycle::Retired) {
                break;
            }
            vm.cleanup_step(
                &mut session,
                1,
                SessionBudget::Exact(SliceBudget::default()),
                || 0,
            )
            .unwrap();
        }
        assert_eq!(
            session.owner_lifecycle(session.root_owner()),
            Some(OwnerLifecycle::Retired)
        );
        assert!(!vm.vm.heap().reactive_evaluating());
        collect(&mut vm);
    }
}
