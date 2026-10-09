use reel_engine::*;

fn session() -> Session {
    Session::new(SessionId(7), Epoch(3), SessionLimits::default()).unwrap()
}

fn line(text: &str) -> DialogueLineInput<'_> {
    DialogueLineInput {
        speaker_id: Some("mio"),
        speaker_name: "Mio",
        content_id: "opening.1",
        text,
    }
}

fn choices() -> [ChoiceInput<'static>; 2] {
    [
        ChoiceInput {
            key: "window",
            label: "走到窗边",
        },
        ChoiceInput {
            key: "door",
            label: "留在门口",
        },
    ]
}

fn dialogue(session: &Session) -> &DialogueInteraction {
    match session.view().interaction.unwrap() {
        InteractionView::Dialogue(dialogue) => dialogue,
        InteractionView::Choice(_) => panic!("expected dialogue"),
    }
}

fn choice(session: &Session) -> &ChoiceInteraction {
    match session.view().interaction.unwrap() {
        InteractionView::Choice(choice) => choice,
        InteractionView::Dialogue(_) => panic!("expected choice"),
    }
}

fn event(owner: OwnerHandle, sequence: u64, action: InputAction) -> InputEvent {
    InputEvent {
        source_owner: owner,
        sequence: InputSequence(sequence),
        action,
    }
}

fn advance(
    session: &mut Session,
    owner: OwnerHandle,
    sequence: u64,
    id: InteractionId,
) -> Result<InputOutcome, InputRejection> {
    session.handle_input(event(owner, sequence, InputAction::Advance(id)))
}

#[test]
fn dialogue_owns_full_content_and_history_as_one_group() {
    let mut session = session();
    let root = session.root_owner();
    let id = {
        let mut text = String::from("你听见了吗？");
        let speaker = String::from("Mio");
        let content_id = String::from("chapter01.opening.question");
        let id = session
            .begin_say(
                root,
                &[
                    DialogueLineInput {
                        speaker_id: Some("mio"),
                        speaker_name: &speaker,
                        content_id: &content_id,
                        text: &text,
                    },
                    DialogueLineInput {
                        speaker_id: Some("eri"),
                        speaker_name: "Eri",
                        content_id: "chapter01.opening.reply",
                        text: "在窗外。",
                    },
                ],
                "zh-CN",
            )
            .unwrap();
        text.replace_range(.., "changed caller data");
        id
    };
    let model = dialogue(&session);
    assert_eq!(model.mode(), DialogueMode::Adv);
    assert_eq!(model.id(), id);
    assert_eq!(model.page_locale(), "zh-CN");
    assert_eq!(model.speaker_name(), "Mio");
    assert_eq!(model.text(), "你听见了吗？");
    assert_eq!(model.lines()[0].speaker_id(), Some("mio"));
    assert_eq!(model.lines()[0].content_id(), "chapter01.opening.question");
    assert_eq!(model.lines()[0].fallback_text(), model.text());
    assert_eq!(model.lines()[0].visible_text(), "");
    assert_eq!(model.lines()[1].resolved_text(), "在窗外。");
    assert_eq!(
        model.current_line_ids(),
        &[model.lines()[0].id(), model.lines()[1].id()]
    );
    let history = session.view().history;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].interaction_id(), id);
    assert_eq!(history[0].lines().len(), 2);
    assert_eq!(
        history[0].lines()[0].content().id(),
        "chapter01.opening.question"
    );
    assert_eq!(history[0].text(), model.text());
    assert_eq!(history[0].page_locale(), "zh-CN");
}

#[test]
fn reveal_uses_graphemes_and_never_changes_full_text_layout_or_history() {
    let mut session = session();
    let root = session.root_owner();
    let text = "e\u{301}👩🏽‍🚀\r\n🇨🇳\0";
    let id = session
        .begin_say(root, &[line(text), line("第二行")], "zh-CN")
        .unwrap();
    let text_pointer = dialogue(&session).lines()[0].resolved_text().as_ptr();
    let boundaries_pointer = dialogue(&session).lines()[0].grapheme_boundaries().as_ptr();
    let before = session.storage_usage();
    assert_eq!(dialogue(&session).lines()[0].reveal().total_count, 5);
    assert!(!session.reveal_more(root, id, 0).unwrap());
    assert!(session.reveal_more(root, id, 1).unwrap());
    let first = &dialogue(&session).lines()[0];
    assert_eq!(first.visible_text(), "e\u{301}");
    assert_eq!(first.reveal().visible_bytes, 0..3);
    assert!(session.reveal_more(root, id, 1).unwrap());
    assert_eq!(dialogue(&session).lines()[0].visible_text(), "e\u{301}👩🏽‍🚀");
    assert_eq!(
        advance(&mut session, root, 1, id),
        Ok(InputOutcome::Revealed)
    );
    let full = &dialogue(&session).lines()[0];
    assert_eq!(full.visible_text(), text);
    assert_eq!(full.reveal().revealed_count, 5);
    assert_eq!(full.resolved_text().as_ptr(), text_pointer);
    assert_eq!(full.grapheme_boundaries().as_ptr(), boundaries_pointer);
    assert_eq!(session.storage_usage(), before);
    assert!(!session.reveal_more(root, id, usize::MAX).unwrap());
    assert_eq!(session.view().history.len(), 1);
    assert_eq!(session.view().history[0].text(), text);
    assert!(session.take_completion().is_none());
    assert_eq!(
        advance(&mut session, root, 2, id),
        Ok(InputOutcome::Completed)
    );
    assert!(session.view().interaction.is_none());
    let completion = session.take_completion().unwrap();
    assert_eq!(completion.outcome(), &CompletionOutcome::Advanced);
    assert_eq!(session.view().history.len(), 1);
}

#[test]
fn terminal_slot_must_be_claimed_and_history_is_written_exactly_once() {
    let mut session = session();
    let root = session.root_owner();
    let ui_owner = session.create_owner().unwrap();
    let id = session.begin_say(root, &[line("first")], "en").unwrap();
    assert_eq!(
        session.begin_choice(root, &choices()),
        Err(SessionError::InteractionActive)
    );
    assert_eq!(
        advance(&mut session, ui_owner, 1, id),
        Ok(InputOutcome::Revealed)
    );
    assert_eq!(
        advance(&mut session, ui_owner, 1, id),
        Err(InputRejection::DuplicateOrOutOfOrder)
    );
    assert_eq!(
        advance(&mut session, ui_owner, 2, id),
        Ok(InputOutcome::Completed)
    );
    assert_eq!(
        session.begin_say(root, &[line("second")], "en"),
        Err(SessionError::CompletionPending)
    );
    assert_eq!(session.end(), Err(SessionError::CompletionPending));
    assert_eq!(session.view().history.len(), 1);
    let completion = session.take_completion().unwrap();
    assert_eq!(completion.interaction_id(), id);
    assert_eq!(completion.owner(), root);
    assert_eq!(completion.delivery(), CompletionDelivery::Resume);
    assert!(session.take_completion().is_none());
    assert_eq!(
        advance(&mut session, ui_owner, 3, id),
        Err(InputRejection::NoActiveInteraction)
    );
    let successor = session.begin_say(root, &[line("second")], "en").unwrap();
    assert_eq!(successor.generation(), id.generation() + 1);
    assert_eq!(
        advance(&mut session, ui_owner, 4, id),
        Err(InputRejection::StaleInteraction)
    );
    assert_eq!(dialogue(&session).reveal().revealed_count, 0);
    assert_eq!(
        session.reveal_more(root, id, 1),
        Err(SessionError::StaleInteraction)
    );
    assert_eq!(
        advance(&mut session, ui_owner, 4, successor),
        Err(InputRejection::DuplicateOrOutOfOrder)
    );
    assert_eq!(
        advance(&mut session, ui_owner, 5, successor),
        Ok(InputOutcome::Revealed)
    );
    assert_eq!(session.view().history.len(), 2);
}

#[test]
fn choices_copy_input_keep_business_key_and_reject_prior_round_ids() {
    let mut session = session();
    let root = session.root_owner();
    let ui_owner = session.create_owner().unwrap();
    let id = {
        let mut key = String::from("window");
        let mut label = String::from("走到窗边");
        let id = session
            .begin_choice(
                root,
                &[ChoiceInput {
                    key: &key,
                    label: &label,
                }],
            )
            .unwrap();
        key.clear();
        label.clear();
        id
    };
    let option_id = choice(&session).options()[0].id().clone();
    assert_eq!(option_id.interaction_id(), id);
    assert_eq!(option_id.key(), "window");
    assert_eq!(choice(&session).options()[0].label(), "走到窗边");
    assert_eq!(
        session.reveal_more(root, id, 1),
        Err(SessionError::WrongInteractionKind)
    );
    assert_eq!(
        advance(&mut session, ui_owner, 1, id),
        Err(InputRejection::WrongInteractionKind)
    );
    assert_eq!(
        session.handle_input(event(ui_owner, 2, InputAction::Select(option_id.clone()))),
        Ok(InputOutcome::Completed)
    );
    assert!(session.view().interaction.is_none());
    assert!(session.view().history.is_empty());
    let completion = session.take_completion().unwrap();
    assert_eq!(
        completion.into_outcome(),
        CompletionOutcome::Selected("window".into())
    );
    let next = session.begin_choice(root, &choices()).unwrap();
    let next_id = choice(&session).options()[0].id().clone();
    assert_ne!(option_id, next_id);
    assert_eq!(option_id.key(), next_id.key());
    assert_eq!(
        session.handle_input(event(ui_owner, 3, InputAction::Select(option_id))),
        Err(InputRejection::StaleInteraction)
    );
    assert_eq!(choice(&session).id(), next);
    assert_eq!(
        session.handle_input(event(ui_owner, 3, InputAction::Select(next_id.clone()))),
        Err(InputRejection::DuplicateOrOutOfOrder)
    );
    assert_eq!(
        session.handle_input(event(ui_owner, 4, InputAction::Select(next_id))),
        Ok(InputOutcome::Completed)
    );
}

#[test]
fn only_root_submits_and_dead_ui_owner_cannot_advance_or_poison_sequence() {
    let mut session = session();
    let root = session.root_owner();
    let retired = session.create_owner().unwrap();
    assert_eq!(
        session.begin_say(retired, &[line("no")], "en"),
        Err(SessionError::MainOwnerRequired)
    );
    assert_eq!(
        session.begin_choice(retired, &choices()),
        Err(SessionError::MainOwnerRequired)
    );
    let id = session.begin_say(root, &[line("main")], "en").unwrap();
    assert_eq!(
        session.reveal_more(retired, id, 1),
        Err(SessionError::MainOwnerRequired)
    );
    assert!(session.revoke_owner(retired).unwrap());
    assert!(!session.revoke_owner(retired).unwrap());
    assert!(!session.is_owner_live(retired));
    assert!(session.is_owner_live(root));
    assert_eq!(
        advance(&mut session, retired, u64::MAX, id),
        Err(InputRejection::OwnerNotLive)
    );
    assert_eq!(
        advance(&mut session, root, 1, id),
        Ok(InputOutcome::Revealed)
    );
    assert_eq!(session.view().history.len(), 1);
}

#[test]
fn owner_slots_reuse_storage_but_never_identity() {
    let mut session = Session::new(
        SessionId(1),
        Epoch(1),
        SessionLimits {
            max_owners: 2,
            ..SessionLimits::default()
        },
    )
    .unwrap();
    let root = session.root_owner();
    let first = session.create_owner().unwrap();
    let usage = session.storage_usage();
    assert_eq!(first.owner_id(), OwnerId(2));
    assert_eq!(
        session.create_owner(),
        Err(SessionError::Capacity(CapacityKind::Owners))
    );
    assert_eq!(session.storage_usage(), usage);
    session.revoke_owner(first).unwrap();
    let second = session.create_owner().unwrap();
    assert_eq!(second.owner_id(), OwnerId(3));
    assert_eq!(
        second.scope(),
        Scope::Session {
            session_id: SessionId(1),
            epoch: Epoch(1),
            owner_id: OwnerId(3)
        }
    );
    assert_ne!(first, second);
    assert!(!session.is_owner_live(first));
    assert!(session.is_owner_live(second));
    assert_eq!(session.storage_usage(), usage);
    assert!(session.is_owner_live(root));
}

#[test]
fn new_epoch_and_new_session_reject_old_owner_interaction_and_reveal() {
    let mut old = session();
    let old_root = old.root_owner();
    let old_id = old.begin_say(old_root, &[line("old")], "en").unwrap();
    for identity in [(SessionId(7), Epoch(4)), (SessionId(8), Epoch(3))] {
        let mut current = Session::new(identity.0, identity.1, SessionLimits::default()).unwrap();
        let root = current.root_owner();
        let id = current.begin_say(root, &[line("current")], "en").unwrap();
        assert_eq!(id.generation(), old_id.generation());
        assert_ne!(id, old_id);
        assert_eq!(
            advance(&mut current, old_root, u64::MAX, old_id),
            Err(InputRejection::ForeignOwner)
        );
        assert_eq!(
            current.reveal_more(old_root, old_id, 1),
            Err(SessionError::ForeignOwner)
        );
        assert_eq!(
            current.revoke_owner(old_root),
            Err(SessionError::ForeignOwner)
        );
        assert_eq!(
            advance(&mut current, root, 1, old_id),
            Err(InputRejection::StaleInteraction)
        );
        assert_eq!(
            current.reveal_more(root, old_id, 1),
            Err(SessionError::StaleInteraction)
        );
        assert_eq!(dialogue(&current).reveal().revealed_count, 0);
        assert_eq!(
            advance(&mut current, root, 2, id),
            Ok(InputOutcome::Revealed)
        );
    }
}

#[test]
fn dialogue_capacity_failures_preserve_storage_history_and_generation() {
    let input = line("x");
    // en + mio + Mio + opening.1 + x = 18 copied source bytes.
    let exact_bytes = "en".len() + "mio".len() + "Mio".len() + "opening.1".len() + 1;
    let mut session = Session::new(
        SessionId(1),
        Epoch(1),
        SessionLimits {
            max_lines_per_dialogue: 1,
            max_history_groups: 1,
            max_history_text_bytes: exact_bytes,
            max_interaction_text_bytes: exact_bytes,
            ..SessionLimits::default()
        },
    )
    .unwrap();
    let root = session.root_owner();
    let before = session.storage_usage();
    assert_eq!(
        session.begin_say(root, &[input, input], "en"),
        Err(SessionError::Capacity(CapacityKind::DialogueLines))
    );
    assert_eq!(
        session.begin_say(root, &[line("xx")], "en"),
        Err(SessionError::Capacity(CapacityKind::InteractionTextBytes))
    );
    assert_eq!(
        session.begin_say(root, &[input], ""),
        Err(SessionError::EmptyLocale)
    );
    let invalid = DialogueLineInput {
        content_id: "",
        ..input
    };
    assert_eq!(
        session.begin_say(root, &[invalid], "en"),
        Err(SessionError::EmptyContentId)
    );
    assert_eq!(session.storage_usage(), before);
    assert!(session.view().history.is_empty());
    assert!(session.view().interaction.is_none());
    let id = session.begin_say(root, &[input], "en").unwrap();
    assert_eq!(id.generation(), 1);
    assert_eq!(
        session.storage_usage().retained_history_text_bytes,
        exact_bytes
    );
    assert!(session.storage_usage().allocated_capacity_bytes > exact_bytes * 2);
    advance(&mut session, root, 1, id).unwrap();
    advance(&mut session, root, 2, id).unwrap();
    session.take_completion().unwrap();
    let after = session.storage_usage();
    assert_eq!(
        session.begin_say(root, &[input], "en"),
        Err(SessionError::Capacity(CapacityKind::HistoryGroups))
    );
    assert_eq!(session.storage_usage(), after);
    assert_eq!(
        session.begin_choice(root, &choices()),
        Err(SessionError::Capacity(CapacityKind::InteractionTextBytes))
    );
    let next = session
        .begin_choice(
            root,
            &[ChoiceInput {
                key: "k",
                label: "l",
            }],
        )
        .unwrap();
    assert_eq!(next.generation(), 2);
}

#[test]
fn history_byte_limit_is_independent_and_atomic() {
    let mut session = Session::new(
        SessionId(1),
        Epoch(1),
        SessionLimits {
            max_history_text_bytes: 1,
            ..SessionLimits::default()
        },
    )
    .unwrap();
    let root = session.root_owner();
    let before = session.storage_usage();
    assert_eq!(
        session.begin_say(root, &[line("bounded")], "en"),
        Err(SessionError::Capacity(CapacityKind::HistoryTextBytes))
    );
    assert_eq!(session.storage_usage(), before);
    assert!(session.view().history.is_empty());
    let id = session.begin_choice(root, &choices()).unwrap();
    assert_eq!(id.generation(), 1);
}

#[test]
fn choice_validation_is_atomic_and_duplicate_keys_are_not_normalized() {
    let mut session = Session::new(
        SessionId(1),
        Epoch(1),
        SessionLimits {
            max_choices: 2,
            max_interaction_text_bytes: 32,
            ..SessionLimits::default()
        },
    )
    .unwrap();
    let root = session.root_owner();
    let before = session.storage_usage();
    let option = ChoiceInput {
        key: "k",
        label: "label",
    };
    assert_eq!(
        session.begin_choice(root, &[]),
        Err(SessionError::EmptyChoices)
    );
    assert_eq!(
        session.begin_choice(root, &[ChoiceInput { key: "", ..option }]),
        Err(SessionError::EmptyChoiceKey)
    );
    assert_eq!(
        session.begin_choice(root, &[option, option]),
        Err(SessionError::DuplicateChoiceKey)
    );
    assert_eq!(
        session.begin_choice(root, &[option, option, option]),
        Err(SessionError::Capacity(CapacityKind::Choices))
    );
    assert_eq!(
        session.begin_choice(
            root,
            &[ChoiceInput {
                key: "k",
                label: "a label longer than the permitted byte budget"
            }]
        ),
        Err(SessionError::Capacity(CapacityKind::InteractionTextBytes))
    );
    let oversized_duplicate = ChoiceInput {
        key: "duplicate key longer than the permitted byte budget",
        label: "",
    };
    assert_eq!(
        session.begin_choice(root, &[oversized_duplicate, oversized_duplicate]),
        Err(SessionError::Capacity(CapacityKind::InteractionTextBytes))
    );
    assert_eq!(session.storage_usage(), before);
    assert!(session.view().history.is_empty());
    // Exact UTF-8 sequence is the key contract, not implicit normalization.
    let id = session
        .begin_choice(
            root,
            &[
                ChoiceInput {
                    key: "é",
                    label: "one",
                },
                ChoiceInput {
                    key: "e\u{301}",
                    label: "two",
                },
            ],
        )
        .unwrap();
    assert_eq!(id.generation(), 1);
    assert_ne!(
        choice(&session).options()[0].id(),
        choice(&session).options()[1].id()
    );
}

#[test]
fn cancel_keeps_history_and_commits_cleanup_once() {
    let mut session = session();
    let root = session.root_owner();
    let ui_owner = session.create_owner().unwrap();
    let id = session
        .begin_say(root, &[line("before cancel")], "en")
        .unwrap();
    assert!(session.cancel());
    assert!(!session.cancel());
    assert_eq!(session.status(), SessionStatus::Cancelled);
    assert!(!session.is_owner_live(root));
    assert!(!session.is_owner_live(ui_owner));
    assert!(session.view().interaction.is_none());
    assert_eq!(session.view().history.len(), 1);
    assert_eq!(session.view().history[0].text(), "before cancel");
    let completion = session.take_completion().unwrap();
    assert_eq!(completion.interaction_id(), id);
    assert_eq!(completion.delivery(), CompletionDelivery::CleanupOnly);
    assert_eq!(
        completion.outcome(),
        &CompletionOutcome::Cancelled(CancellationReason::SessionCancelled)
    );
    assert!(session.take_completion().is_none());
    assert_eq!(
        advance(&mut session, root, 1, id),
        Err(InputRejection::SessionNotRunning)
    );
    assert_eq!(
        session.reveal_more(root, id, 1),
        Err(SessionError::SessionNotRunning)
    );
    assert_eq!(
        session.begin_choice(root, &choices()),
        Err(SessionError::SessionNotRunning)
    );
}

#[test]
fn root_revoke_is_cleanup_and_pending_success_is_not_settled_twice() {
    let mut revoked = session();
    let root = revoked.root_owner();
    revoked.begin_choice(root, &choices()).unwrap();
    assert!(revoked.revoke_owner(root).unwrap());
    assert!(!revoked.revoke_owner(root).unwrap());
    let completion = revoked.take_completion().unwrap();
    assert_eq!(completion.delivery(), CompletionDelivery::CleanupOnly);
    assert_eq!(
        completion.outcome(),
        &CompletionOutcome::Cancelled(CancellationReason::OwnerRevoked)
    );
    assert!(revoked.take_completion().is_none());

    let mut completed = session();
    let root = completed.root_owner();
    let id = completed.begin_say(root, &[], "en").unwrap();
    advance(&mut completed, root, 1, id).unwrap();
    assert!(completed.cancel());
    let completion = completed.take_completion().unwrap();
    assert_eq!(completion.outcome(), &CompletionOutcome::Advanced);
    assert_eq!(completion.delivery(), CompletionDelivery::CleanupOnly);
    assert!(completed.take_completion().is_none());
    assert_eq!(completed.view().history.len(), 1);
}

#[test]
fn claimed_record_is_not_permanent_owner_delivery_permission() {
    let mut session = session();
    let root = session.root_owner();
    let id = session.begin_say(root, &[], "en").unwrap();
    advance(&mut session, root, 1, id).unwrap();
    let claimed = session.take_completion().unwrap();
    assert_eq!(claimed.delivery(), CompletionDelivery::Resume);
    assert!(session.is_owner_live(claimed.owner()));
    session.revoke_owner(root).unwrap();
    assert!(!session.is_owner_live(claimed.owner()));
    // Claimed data cannot change; future adapters must recheck the host before delivery.
    assert_eq!(claimed.delivery(), CompletionDelivery::Resume);
    assert!(session.take_completion().is_none());
}

#[test]
fn empty_group_projects_zero_and_end_requires_an_idle_claimed_session() {
    let mut session = session();
    let root = session.root_owner();
    let id = session.begin_say(root, &[], "en").unwrap();
    assert_eq!(dialogue(&session).speaker_name(), "");
    assert_eq!(dialogue(&session).text(), "");
    assert_eq!(
        dialogue(&session).reveal(),
        Reveal {
            revealed_count: 0,
            total_count: 0,
            visible_bytes: 0..0
        }
    );
    assert!(dialogue(&session).current_line_ids().is_empty());
    assert!(!session.reveal_more(root, id, 1).unwrap());
    assert_eq!(session.end(), Err(SessionError::InteractionActive));
    assert_eq!(
        advance(&mut session, root, 0, id),
        Ok(InputOutcome::Completed)
    );
    assert_eq!(session.end(), Err(SessionError::CompletionPending));
    session.take_completion().unwrap();
    session.end().unwrap();
    assert_eq!(session.status(), SessionStatus::Ended);
    assert!(!session.is_owner_live(root));
    assert!(!session.cancel());
    assert_eq!(session.end(), Err(SessionError::SessionNotRunning));
    assert_eq!(session.create_owner(), Err(SessionError::SessionNotRunning));
    assert_eq!(session.view().history.len(), 1);
}

#[test]
fn invalid_and_impossible_storage_limits_fail_explicitly() {
    assert_eq!(
        Session::new(
            SessionId(1),
            Epoch(1),
            SessionLimits {
                max_owners: 0,
                ..SessionLimits::default()
            }
        )
        .unwrap_err(),
        SessionError::InvalidLimits
    );
    assert_eq!(
        Session::new(
            SessionId(1),
            Epoch(1),
            SessionLimits {
                max_owners: usize::MAX,
                ..SessionLimits::default()
            }
        )
        .unwrap_err(),
        SessionError::AllocationFailed
    );
}

#[test]
fn input_sequence_high_water_is_shared_by_all_live_ui_owners() {
    let mut session = session();
    let root = session.root_owner();
    let first = session.create_owner().unwrap();
    let second = session.create_owner().unwrap();
    let id = session.begin_say(root, &[line("one click")], "en").unwrap();
    assert_eq!(
        advance(&mut session, first, 10, id),
        Ok(InputOutcome::Revealed)
    );
    assert_eq!(
        advance(&mut session, second, 9, id),
        Err(InputRejection::DuplicateOrOutOfOrder)
    );
    assert_eq!(
        advance(&mut session, second, 10, id),
        Err(InputRejection::DuplicateOrOutOfOrder)
    );
    assert_eq!(
        advance(&mut session, second, 11, id),
        Ok(InputOutcome::Completed)
    );
    session.take_completion().unwrap();
    let next = session.begin_say(root, &[line("next")], "en").unwrap();
    assert_eq!(
        advance(&mut session, first, 10, next),
        Err(InputRejection::DuplicateOrOutOfOrder)
    );
    assert_eq!(dialogue(&session).reveal().revealed_count, 0);
    assert_eq!(
        advance(&mut session, first, 12, next),
        Ok(InputOutcome::Revealed)
    );
}

#[test]
fn new_local_actions_and_stamped_replays_share_one_session_sequence() {
    let mut session = session();
    let root = session.root_owner();
    let local_owner = session.create_owner().unwrap();
    let first = session.begin_say(root, &[line("first")], "en").unwrap();
    assert_eq!(
        session.handle_local_input(local_owner, InputAction::Advance(first)),
        Ok(InputOutcome::Revealed)
    );
    assert_eq!(
        advance(&mut session, root, 1, first),
        Err(InputRejection::DuplicateOrOutOfOrder)
    );
    assert_eq!(
        advance(&mut session, root, 100, first),
        Ok(InputOutcome::Completed)
    );
    assert_eq!(
        session.take_completion().unwrap().outcome(),
        &CompletionOutcome::Advanced
    );
    let next = session.begin_say(root, &[line("next")], "en").unwrap();
    assert_eq!(
        advance(&mut session, root, 101, first),
        Err(InputRejection::StaleInteraction)
    );
    assert_eq!(
        session.handle_local_input(local_owner, InputAction::Advance(next)),
        Ok(InputOutcome::Revealed)
    );
    assert_eq!(
        advance(&mut session, root, 102, next),
        Err(InputRejection::DuplicateOrOutOfOrder)
    );
    assert!(session.take_completion().is_none());
    assert_eq!(
        session.handle_local_input(local_owner, InputAction::Advance(next)),
        Ok(InputOutcome::Completed)
    );
    assert_eq!(
        session.take_completion().unwrap().outcome(),
        &CompletionOutcome::Advanced
    );
    assert!(session.take_completion().is_none());
    assert_eq!(session.view().history.len(), 2);
}

#[test]
fn local_input_exhaustion_never_wraps_and_dead_or_foreign_sources_cannot_poison_it() {
    let mut session = session();
    let root = session.root_owner();
    let dead = session.create_owner().unwrap();
    session.revoke_owner(dead).unwrap();
    let mut foreign = Session::new(SessionId(99), Epoch(1), SessionLimits::default()).unwrap();
    let foreign_owner = foreign.root_owner();
    let foreign_interaction = foreign
        .begin_say(foreign_owner, &[line("foreign")], "en")
        .unwrap();
    let id = session.begin_say(root, &[line("live")], "en").unwrap();
    assert_eq!(
        advance(&mut session, dead, u64::MAX, id),
        Err(InputRejection::OwnerNotLive)
    );
    assert_eq!(
        advance(&mut session, foreign_owner, u64::MAX, id),
        Err(InputRejection::ForeignOwner)
    );
    assert_eq!(
        session.handle_local_input(root, InputAction::Advance(id)),
        Ok(InputOutcome::Revealed)
    );
    assert_eq!(
        advance(&mut session, root, u64::MAX, foreign_interaction),
        Err(InputRejection::StaleInteraction)
    );
    assert_eq!(
        session.handle_local_input(dead, InputAction::Advance(id)),
        Err(InputRejection::OwnerNotLive)
    );
    assert_eq!(
        session.handle_local_input(foreign_owner, InputAction::Advance(id)),
        Err(InputRejection::ForeignOwner)
    );
    let before = session.storage_usage();
    assert_eq!(
        session.handle_local_input(root, InputAction::Advance(id)),
        Err(InputRejection::SequenceExhausted)
    );
    assert_eq!(session.storage_usage(), before);
    assert_eq!(dialogue(&session).lines()[0].visible_text(), "live");
    assert_eq!(session.view().history.len(), 1);
    assert!(session.take_completion().is_none());
    assert_eq!(
        advance(&mut session, root, 1, id),
        Err(InputRejection::DuplicateOrOutOfOrder)
    );
    assert!(session.cancel());
    assert_eq!(
        session.take_completion().unwrap().delivery(),
        CompletionDelivery::CleanupOnly
    );
    assert!(session.take_completion().is_none());
    assert_eq!(
        session.handle_local_input(root, InputAction::Advance(id)),
        Err(InputRejection::SessionNotRunning)
    );
}

#[test]
fn zero_line_dialogue_has_one_history_group_and_one_advance_completion() {
    let mut session = session();
    let root = session.root_owner();
    let id = session.begin_say(root, &[], "en").unwrap();
    assert!(dialogue(&session).lines().is_empty());
    assert_eq!(dialogue(&session).text(), "");
    assert!(!session.reveal_more(root, id, 1).unwrap());
    assert_eq!(session.view().history.len(), 1);
    assert!(session.view().history[0].lines().is_empty());
    assert_eq!(
        session.handle_local_input(root, InputAction::Advance(id)),
        Ok(InputOutcome::Completed)
    );
    let completion = session.take_completion().unwrap();
    assert_eq!(completion.interaction_id(), id);
    assert_eq!(completion.outcome(), &CompletionOutcome::Advanced);
    assert_eq!(completion.delivery(), CompletionDelivery::Resume);
    assert!(session.take_completion().is_none());
    assert_eq!(
        session.handle_local_input(root, InputAction::Advance(id)),
        Err(InputRejection::NoActiveInteraction)
    );
    assert!(session.take_completion().is_none());
    assert_eq!(session.view().history.len(), 1);
}
