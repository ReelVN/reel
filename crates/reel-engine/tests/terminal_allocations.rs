use reel_engine::*;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

// This integration test has a single test function, so another test thread
// cannot add unrelated allocator traffic while the region is being measured.
#[test]
fn accepted_interactions_reveal_advance_select_and_cancel_without_allocating() {
    let limits = SessionLimits {
        max_owners: 3,
        max_history_groups: 4,
        ..SessionLimits::default()
    };
    let mut dialogue = Session::new(SessionId(1), Epoch(1), limits).unwrap();
    let root = dialogue.root_owner();
    let ui_owner = dialogue.create_owner().unwrap();
    let id = dialogue
        .begin_say(
            root,
            &[DialogueLineInput {
                speaker_id: Some("mio"),
                speaker_name: "Mio",
                content_id: "opening",
                text: "e\u{301}👩🏽‍🚀 雨水停在玻璃上。",
            }],
            "zh-CN",
        )
        .unwrap();
    let mut choice = Session::new(SessionId(2), Epoch(1), limits).unwrap();
    let choice_root = choice.root_owner();
    choice
        .begin_choice(
            choice_root,
            &[
                ChoiceInput {
                    key: "window",
                    label: "走到窗边",
                },
                ChoiceInput {
                    key: "door",
                    label: "留在门口",
                },
            ],
        )
        .unwrap();
    let option_id = match choice.view().interaction.unwrap() {
        InteractionView::Choice(model) => model.options()[0].id().clone(),
        _ => unreachable!(),
    };
    let mut cancelled = Session::new(SessionId(3), Epoch(1), limits).unwrap();
    let cancelled_root = cancelled.root_owner();
    cancelled
        .begin_say(
            cancelled_root,
            &[DialogueLineInput {
                speaker_id: None,
                speaker_name: "",
                content_id: "cancel",
                text: "cleanup",
            }],
            "en",
        )
        .unwrap();

    let region = Region::new(GLOBAL);
    let revealed = dialogue.reveal_more(root, id, 1);
    let first = dialogue.handle_input(InputEvent {
        source_owner: ui_owner,
        sequence: InputSequence(1),
        action: InputAction::Advance(id),
    });
    let duplicate = dialogue.handle_input(InputEvent {
        source_owner: ui_owner,
        sequence: InputSequence(1),
        action: InputAction::Advance(id),
    });
    let second = dialogue.handle_input(InputEvent {
        source_owner: ui_owner,
        sequence: InputSequence(2),
        action: InputAction::Advance(id),
    });
    let completed = dialogue.take_completion();
    let selected = choice.handle_input(InputEvent {
        source_owner: choice_root,
        sequence: InputSequence(1),
        action: InputAction::Select(option_id),
    });
    let choice_completed = choice.take_completion();
    let cancelled_now = cancelled.revoke_owner(cancelled_root);
    let cancelled_again = cancelled.cancel();
    let cancelled_completion = cancelled.take_completion();
    let metrics = region.change();

    assert_eq!(metrics.allocations, 0, "{metrics:?}");
    assert_eq!(metrics.reallocations, 0, "{metrics:?}");
    assert_eq!(metrics.bytes_allocated, 0, "{metrics:?}");
    assert_eq!(revealed, Ok(true));
    assert_eq!(first, Ok(InputOutcome::Revealed));
    assert_eq!(duplicate, Err(InputRejection::DuplicateOrOutOfOrder));
    assert_eq!(second, Ok(InputOutcome::Completed));
    assert_eq!(completed.unwrap().outcome(), &CompletionOutcome::Advanced);
    assert_eq!(selected, Ok(InputOutcome::Completed));
    assert_eq!(
        choice_completed.unwrap().outcome(),
        &CompletionOutcome::Selected("window".to_owned())
    );
    assert_eq!(cancelled_now, Ok(true));
    assert!(!cancelled_again);
    assert_eq!(
        cancelled_completion.unwrap().delivery(),
        CompletionDelivery::CleanupOnly
    );
    println!(
        "host terminal region: allocations={}, reallocations={}, bytes_allocated={}, deallocations={}",
        metrics.allocations, metrics.reallocations, metrics.bytes_allocated, metrics.deallocations
    );
}
