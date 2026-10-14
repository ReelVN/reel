//! These tests verify the Rust lifecycle ledger. Reported cursors and category
//! confirmations are test data; no task execution, root release or GC is run.

use reel_engine::*;

fn session() -> Session {
    Session::new(SessionId(11), Epoch(4)).unwrap()
}

fn requirements() -> OwnerRetirementRequirements {
    OwnerRetirementRequirements {
        root_slots_end: 5,
        tasks_pending: true,
        external_resources_pending: true,
    }
}

fn begin_dialogue(session: &mut Session) -> InteractionId {
    session
        .begin_say(
            session.root_owner(),
            &[DialogueLineInput {
                speaker_id: None,
                speaker_name: "",
                content_id: "retirement",
                text: ("retained history").into(),
            }],
            "en",
        )
        .unwrap()
}

#[test]
fn revoked_slot_cannot_be_reused_until_all_categories_complete() {
    let mut session = session();
    let root = session.root_owner();
    let owner = session.create_owner().unwrap();
    let id = begin_dialogue(&mut session);
    let capacity = session.storage_usage().allocated_capacity_bytes;
    assert_eq!(session.owner_lifecycle(owner), Some(OwnerLifecycle::Live));
    assert!(session.owner_retirement(owner).is_none());
    assert!(
        session
            .revoke_owner_with_retirement(owner, requirements())
            .unwrap()
    );
    assert_eq!(
        session.owner_lifecycle(owner),
        Some(OwnerLifecycle::Revoked)
    );
    assert!(!session.is_owner_live(owner));
    assert!(session.is_owner_live(root));
    let fresh = session.create_owner().unwrap();
    assert!(session.is_owner_live(fresh));
    assert_eq!(
        session.owner_lifecycle(owner),
        Some(OwnerLifecycle::Revoked)
    );
    let usage = session.storage_usage();
    assert_eq!(
        usage.reserved_retirement_records,
        usage.reserved_owner_slots
    );
    assert_eq!(usage.pending_owner_retirements, 1);
    assert_eq!(usage.allocated_capacity_bytes, capacity);
    assert_eq!(
        session.handle_input(InputEvent {
            source_owner: owner,
            sequence: InputSequence(u64::MAX),
            action: InputAction::Advance(id),
        }),
        Err(InputRejection::OwnerNotLive)
    );
    assert_eq!(
        session.reveal_more(owner, id, 1),
        Err(SessionError::OwnerNotLive)
    );
    assert_eq!(
        session.begin_choice(
            owner,
            &[ChoiceInput {
                key: "k",
                label: "l"
            }]
        ),
        Err(SessionError::OwnerNotLive)
    );
    assert_eq!(
        session.handle_input(InputEvent {
            source_owner: root,
            sequence: InputSequence(1),
            action: InputAction::Advance(id),
        }),
        Ok(InputOutcome::Revealed)
    );

    let before = session.owner_retirement(owner).unwrap();
    assert_eq!(
        session.report_owner_root_progress(owner, 2, 1),
        Err(SessionError::RetirementBudgetExceeded)
    );
    assert_eq!(
        session.report_owner_root_progress(owner, 6, 10),
        Err(SessionError::RetirementCursorOutOfRange)
    );
    assert_eq!(session.owner_retirement(owner), Some(before));
    // This cursor report charges both visited slots, even if one was a hole.
    assert_eq!(
        session.report_owner_root_progress(owner, 2, 2),
        Ok(OwnerLifecycle::Revoked)
    );
    assert_eq!(
        session.confirm_owner_task_cleanup(owner),
        Ok(OwnerLifecycle::Revoked)
    );
    assert_eq!(
        session.confirm_owner_resources_released(owner),
        Ok(OwnerLifecycle::Revoked)
    );
    assert_eq!(
        session.report_owner_root_progress(owner, 1, 10),
        Err(SessionError::RetirementCursorRegression)
    );
    assert_eq!(
        session.report_owner_root_progress(owner, 2, 0),
        Ok(OwnerLifecycle::Revoked)
    );
    assert_eq!(
        session.report_owner_root_progress(owner, 5, 3),
        Ok(OwnerLifecycle::Retired)
    );
    assert_eq!(session.storage_usage().pending_owner_retirements, 0);
    assert_eq!(
        session.confirm_owner_task_cleanup(owner),
        Ok(OwnerLifecycle::Retired)
    );
    assert_eq!(
        session.confirm_owner_resources_released(owner),
        Ok(OwnerLifecycle::Retired)
    );
    let replacement = session.create_owner().unwrap();
    assert_eq!(replacement.owner_id(), OwnerId(4));
    assert!(session.is_owner_live(replacement));
    assert_eq!(session.owner_lifecycle(owner), None);
    assert_eq!(
        session.report_owner_root_progress(owner, 5, 0),
        Err(SessionError::OwnerUnknown)
    );
    assert_eq!(
        session.confirm_owner_task_cleanup(owner),
        Err(SessionError::OwnerUnknown)
    );
    assert_eq!(
        session.confirm_owner_resources_released(owner),
        Err(SessionError::OwnerUnknown)
    );
    assert_eq!(
        session.owner_lifecycle(replacement),
        Some(OwnerLifecycle::Live)
    );
}

#[test]
fn root_revoke_conflict_is_transactional_until_all_live_peers_declared() {
    let mut session = session();
    let root = session.root_owner();
    let first = session.create_owner().unwrap();
    let second = session.create_owner().unwrap();
    let id = begin_dialogue(&mut session);
    let before = session.storage_usage();
    assert_eq!(
        session.revoke_owner_with_retirement(root, requirements()),
        Err(SessionError::PeerOwnerRequirementsMissing)
    );
    assert_eq!(session.status(), SessionStatus::Running);
    assert!(session.is_owner_live(root));
    assert!(session.owner_retirement(root).is_none());
    assert_eq!(session.storage_usage(), before);
    assert_eq!(session.view().interaction.unwrap().id(), id);
    assert!(session.take_completion().is_none());

    session
        .revoke_owner_with_retirement(first, requirements())
        .unwrap();
    let first_record = session.owner_retirement(first);
    assert_eq!(
        session.revoke_owner_with_retirement(root, requirements()),
        Err(SessionError::PeerOwnerRequirementsMissing)
    );
    assert_eq!(session.owner_retirement(first), first_record);
    session
        .revoke_owner_with_retirement(second, OwnerRetirementRequirements::default())
        .unwrap();
    assert_eq!(
        session.owner_lifecycle(second),
        Some(OwnerLifecycle::Retired)
    );
    assert!(
        session
            .revoke_owner_with_retirement(root, requirements())
            .unwrap()
    );
    assert_eq!(session.status(), SessionStatus::Cancelled);
    assert_eq!(session.owner_lifecycle(root), Some(OwnerLifecycle::Revoked));
    assert_eq!(session.owner_retirement(first), first_record);
    let completion = session.take_completion().unwrap();
    assert_eq!(completion.delivery(), CompletionDelivery::CleanupOnly);
    assert_eq!(
        completion.outcome(),
        &CompletionOutcome::Cancelled(CancellationReason::OwnerRevoked)
    );
    assert_eq!(session.view().history.len(), 1);
    assert_eq!(session.view().history[0].text(), "retained history");
    assert!(!session.cancel());
    assert_eq!(
        session.confirm_owner_task_cleanup(root),
        Ok(OwnerLifecycle::Revoked)
    );
    assert_eq!(
        session.report_owner_root_progress(root, 5, 5),
        Ok(OwnerLifecycle::Revoked)
    );
    assert_eq!(
        session.confirm_owner_resources_released(root),
        Ok(OwnerLifecycle::Retired)
    );
    assert_eq!(
        session.owner_lifecycle(first),
        Some(OwnerLifecycle::Revoked)
    );
    assert!(session.take_completion().is_none());
}

#[test]
fn duplicate_revoke_never_replaces_or_shortcuts_pending_requirements() {
    let mut session = session();
    let owner = session.create_owner().unwrap();
    session
        .revoke_owner_with_retirement(owner, requirements())
        .unwrap();
    session.report_owner_root_progress(owner, 2, 2).unwrap();
    let original = session.owner_retirement(owner);
    assert!(
        !session
            .revoke_owner_with_retirement(owner, OwnerRetirementRequirements::default())
            .unwrap()
    );
    assert!(!session.revoke_owner(owner).unwrap());
    assert_eq!(session.owner_retirement(owner), original);
    assert_eq!(
        session.owner_lifecycle(owner),
        Some(OwnerLifecycle::Revoked)
    );
    let fresh = session.create_owner().unwrap();
    assert!(session.is_owner_live(fresh));
    assert_eq!(
        session.owner_lifecycle(owner),
        Some(OwnerLifecycle::Revoked)
    );
}

#[test]
fn pure_rust_shortcuts_preserve_previously_declared_pending_records() {
    for shortcut in [0, 1, 2] {
        let mut session = session();
        let root = session.root_owner();
        let owner = session.create_owner().unwrap();
        session
            .revoke_owner_with_retirement(owner, requirements())
            .unwrap();
        let before = session.owner_retirement(owner);
        match shortcut {
            0 => {
                assert!(session.cancel());
            }
            1 => {
                session.end().unwrap();
            }
            _ => {
                assert!(session.revoke_owner(root).unwrap());
            }
        }
        assert_eq!(session.owner_lifecycle(root), Some(OwnerLifecycle::Retired));
        assert_eq!(
            session.owner_lifecycle(owner),
            Some(OwnerLifecycle::Revoked)
        );
        assert_eq!(session.owner_retirement(owner), before);
        assert!(!session.is_owner_live(owner));
        assert_eq!(
            session.report_owner_root_progress(owner, 5, 5),
            Ok(OwnerLifecycle::Revoked)
        );
        assert_eq!(
            session.confirm_owner_resources_released(owner),
            Ok(OwnerLifecycle::Revoked)
        );
        assert_eq!(
            session.confirm_owner_task_cleanup(owner),
            Ok(OwnerLifecycle::Retired)
        );
        assert!(!session.is_owner_live(owner));
    }
}

#[test]
fn cleanup_reports_reject_live_and_foreign_owners_without_mutation() {
    let mut session = session();
    let root = session.root_owner();
    let before = session.storage_usage();
    assert_eq!(
        session.report_owner_root_progress(root, 0, 0),
        Err(SessionError::OwnerNotRevoked)
    );
    assert_eq!(
        session.confirm_owner_task_cleanup(root),
        Err(SessionError::OwnerNotRevoked)
    );
    assert_eq!(
        session.confirm_owner_resources_released(root),
        Err(SessionError::OwnerNotRevoked)
    );
    let foreign = Session::new(SessionId(12), Epoch(4)).unwrap().root_owner();
    assert_eq!(
        session.revoke_owner_with_retirement(foreign, requirements()),
        Err(SessionError::ForeignOwner)
    );
    assert_eq!(
        session.report_owner_root_progress(foreign, 0, 0),
        Err(SessionError::ForeignOwner)
    );
    assert_eq!(
        session.confirm_owner_task_cleanup(foreign),
        Err(SessionError::ForeignOwner)
    );
    assert_eq!(
        session.confirm_owner_resources_released(foreign),
        Err(SessionError::ForeignOwner)
    );
    assert_eq!(session.storage_usage(), before);
    assert!(session.is_owner_live(root));
}

#[test]
fn cursor_budget_and_bound_checks_use_differences_without_overflow() {
    let mut session = session();
    let owner = session.create_owner().unwrap();
    session
        .revoke_owner_with_retirement(
            owner,
            OwnerRetirementRequirements {
                root_slots_end: usize::MAX,
                tasks_pending: false,
                external_resources_pending: false,
            },
        )
        .unwrap();
    assert_eq!(
        session.report_owner_root_progress(owner, usize::MAX, usize::MAX - 1),
        Err(SessionError::RetirementBudgetExceeded)
    );
    assert_eq!(session.owner_retirement(owner).unwrap().root_cursor, 0);
    assert_eq!(
        session.report_owner_root_progress(owner, usize::MAX - 1, usize::MAX - 1),
        Ok(OwnerLifecycle::Revoked)
    );
    assert_eq!(
        session.report_owner_root_progress(owner, usize::MAX, 1),
        Ok(OwnerLifecycle::Retired)
    );
    assert_eq!(
        session.report_owner_root_progress(owner, 0, usize::MAX),
        Err(SessionError::RetirementCursorRegression)
    );
    assert_eq!(
        session.report_owner_root_progress(owner, usize::MAX, 0),
        Ok(OwnerLifecycle::Retired)
    );
}

#[test]
fn already_completed_main_result_becomes_cleanup_only_on_extended_revoke() {
    let mut session = session();
    let root = session.root_owner();
    let id = session.begin_say(root, &[], "en").unwrap();
    session
        .handle_input(InputEvent {
            source_owner: root,
            sequence: InputSequence(1),
            action: InputAction::Advance(id),
        })
        .unwrap();
    session
        .revoke_owner_with_retirement(root, requirements())
        .unwrap();
    let completion = session.take_completion().unwrap();
    assert_eq!(completion.outcome(), &CompletionOutcome::Advanced);
    assert_eq!(completion.delivery(), CompletionDelivery::CleanupOnly);
    assert!(session.take_completion().is_none());
    assert_eq!(session.owner_lifecycle(root), Some(OwnerLifecycle::Revoked));
    assert_eq!(session.create_owner(), Err(SessionError::SessionNotRunning));
}
