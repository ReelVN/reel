//! Headless host: compile a real story, advance dialogue, select a choice, retire.
use reel_engine::aside::{
    GcBudget, NativeDispatch, RunOutcome, SessionVm, SliceBudget, VmLimits, source,
};
use reel_engine::{Epoch, InputAction, InteractionView, Session, SessionId};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let story = source::compile_story(include_str!("story_contract.aside"), "story", 7, "main")?;
    let mut session = Session::new(SessionId(1), Epoch(1))?;
    let mut vm = SessionVm::load(&session, story.program, VmLimits::default())?;
    let mut entry = Some(story.entry);
    vm.start(&session, story.initializer)?;
    let mut finished = false;
    let mut lines = Vec::new();
    for _ in 0..10_000 {
        match vm.step(
            &mut session,
            SliceBudget {
                instructions: 1,
                ..Default::default()
            },
            || 0,
        )? {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::HostCall(ticket) => {
                if matches!(
                    vm.dispatch(&mut session, ticket, "en", 1)?,
                    NativeDispatch::Published { .. }
                ) {
                    let action = match session.view().interaction.expect("published interaction") {
                        InteractionView::Dialogue(dialogue) => {
                            println!("{}", dialogue.text());
                            lines.push(dialogue.text().to_owned());
                            InputAction::Advance(dialogue.id())
                        }
                        InteractionView::Choice(choice) => {
                            InputAction::Select(choice.options()[0].id().clone())
                        }
                    };
                    let owner = session.root_owner();
                    if let InputAction::Advance(id) = action {
                        session.handle_local_input(owner, InputAction::Advance(id))?;
                        session.handle_local_input(owner, InputAction::Advance(id))?;
                    } else {
                        session.handle_local_input(owner, action)?;
                    }
                }
            }
            RunOutcome::Returned(_) => {
                vm.discard_delivery(&session)?;
                if let Some(function) = entry.take() {
                    vm.start(&session, function)?;
                } else {
                    vm.finish(&mut session)?;
                    finished = true;
                    break;
                }
            }
            other => return Err(format!("unexpected story outcome: {other:?}").into()),
        }
    }
    assert!(finished, "story exceeded bounded turn count");
    assert_eq!(lines, ["A short story.", "continue"]);
    for _ in 0..10_000 {
        if !vm.cleanup_pending() {
            break;
        }
        vm.cleanup_step(&mut session, 1, SliceBudget::default(), || 0)?;
        vm.gc_step(GcBudget {
            units: 1,
            ..Default::default()
        })?;
    }
    assert!(!vm.cleanup_pending(), "owner retirement did not finish");
    println!("Story finished; owner retired.");
    Ok(())
}
