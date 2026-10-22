use super::*;

#[test]
fn repeated_node_removal_is_idempotent_while_retired_fields_and_mutation_still_fail() {
    let source = r#"
        fn opening() {
            let child=stack();
            let node=stack() {child;};
            stage.replace(node);
            let count=model({value:0});
            let worker=spawn(||{await stage.presented(owner:child);count.value+=1;},owner:child);
            let remove=node.remove;
            node.remove();node.remove();remove();node["remove"]();
            let mut rejected=0;
            try {node.x;} catch error {rejected+=1;}
            try {node["x"];} catch error {rejected+=1;}
            try {node.x=12;} catch error {rejected+=1;}
            try {node.add(child);} catch error {rejected+=1;}
            try {node.clear();} catch error {rejected+=1;}
            await delay(0.01).run();
            if rejected!=5 || count.value!=0 || worker.pending {throw "retired node boundary";}
            narrator("removed");
        }
    "#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(source, encoded);
        let mut time = 0.0;
        for step in 0..10_000 {
            if matches!(session.view().interaction,
                Some(crate::InteractionView::Dialogue(dialogue)) if dialogue.text()=="removed")
            {
                break;
            }
            match vm
                .step(
                    &mut session,
                    SessionBudget::RetryMinimum(SliceBudget {
                        instructions: 1,
                        ..Default::default()
                    }),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    if let Err(error) =
                        vm.dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(8))
                    {
                        vm.reject_native(&session, ticket, error).unwrap();
                    }
                }
                SessionStep::Vm(RunOutcome::Fault(id)) => {
                    panic!("retired node fault: {:?}", vm.vm.delivery_fault(id))
                }
                SessionStep::Vm(RunOutcome::Returned(_)) => {
                    vm.discard_delivery(&mut session).unwrap()
                }
                SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                    if !vm.has_runnable_tasks() =>
                {
                    vm.sync_stage(&session).unwrap();
                    vm.accept_presentation(&session, vm.presentation_request())
                        .unwrap();
                    time += 0.01;
                    vm.advance_time(&session, time, 4096).unwrap();
                }
                _ => {}
            }
            assert!(step < 9_999, "retired node cleanup must complete");
        }
        assert!(matches!(session.view().interaction,
            Some(crate::InteractionView::Dialogue(dialogue)) if dialogue.text()=="removed"));
    }
}
