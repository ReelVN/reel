use super::*;
use crate::{DialogueLineInput, Epoch, HistoryInput, InputAction, SessionId};

fn source() -> Session {
    let mut session = Session::new(SessionId(23), Epoch(1)).unwrap();
    session
        .begin_say(
            session.root_owner(),
            &[DialogueLineInput {
                speaker_id: Some("speaker-旧身份"),
                speaker_name: "旧名字",
                content_id: "old/stable/content",
                text: "正文 A👩‍🚀B".into(),
                history: HistoryInput::Display,
            }],
            "zh-CN",
            true,
        )
        .unwrap();
    session
}

#[test]
fn restored_history_uses_current_configuration_before_projecting_rows() {
    let mut source = Session::new(SessionId(1), Epoch(1)).unwrap();
    source.set_history_limits(crate::HistoryLimits {
        max_lines: 500,
        max_bytes: 1024 * 1024,
    });
    for _ in 0..500 {
        source
            .record_history(
                &[DialogueLineInput {
                    speaker_id: None,
                    speaker_name: "",
                    content_id: "recorded",
                    text: "history".into(),
                    history: Default::default(),
                }],
                "en",
            )
            .unwrap();
    }
    let mut capture = ReadingCapture::new(&source, None);
    while !capture.step(&source, None, 256, 4096).unwrap() {}
    let archived = capture.finish().unwrap();
    let encoded = postcard::to_allocvec(&archived).unwrap();
    for limit in [500, 40, 0] {
        let mut candidate = Session::new(SessionId(1), Epoch(2)).unwrap();
        candidate.set_history_limits(crate::HistoryLimits {
            max_lines: limit,
            max_bytes: 1024 * 1024,
        });
        let restored: SavedReading = postcard::from_bytes(&encoded).unwrap();
        assert_eq!(restored.data.history.len(), 500);
        let mut install = restored.install_begin(candidate.identity());
        while !install.step(256).unwrap() {}
        install.finish(&mut candidate).unwrap();
        assert!(candidate.view().history.len() <= limit);
        if limit == 500 {
            assert_eq!(candidate.view().history.len(), 500);
        }
        if limit != 0 {
            assert_eq!(
                candidate
                    .view()
                    .history
                    .last()
                    .unwrap()
                    .interaction_id()
                    .generation(),
                500
            );
        }
    }
}

fn saved(session: &Session, page: Option<&DisplayPage>) -> SavedReading {
    let mut cursor = ReadingCapture::new(session, page);
    let mut previous = cursor.retained_bytes();
    assert!(!cursor.step(session, page, 0, 1).unwrap());
    for _ in 0..10_000 {
        let complete = cursor.step(session, page, 1, 1).unwrap();
        assert!(cursor.retained_bytes() >= previous);
        previous = cursor.retained_bytes();
        if complete {
            let mut saved = cursor.finish().unwrap();
            assert_eq!(saved.retained_bytes(), previous);
            saved.retained = 0;
            assert_eq!(
                saved.retained_bytes(),
                previous,
                "incremental accounting differs from detached data"
            );
            return saved;
        }
    }
    panic!("reading capture did not finish")
}

#[test]
fn detached_capture_tracks_allocations_and_keeps_latest_reveal_without_projection() {
    let mut session = source();
    let Interaction::Dialogue(dialogue) = session.reading_data.active.as_ref().unwrap() else {
        unreachable!()
    };
    let page = DisplayPage {
        lines: dialogue.lines.clone(),
        cleared: false,
    };
    let id = dialogue.id;
    session.reveal_more(session.root_owner(), id, 3).unwrap();
    let saved = saved(&session, Some(&page));
    assert_eq!(saved.display.lines[0].reveal().revealed_count, 3);
    assert_eq!(saved.display.lines[0].content.id(), "old/stable/content");
    assert_eq!(
        saved.data.history[0].lines[0].speaker_id.as_deref(),
        Some("speaker-旧身份")
    );
}

#[test]
fn installation_rebases_every_identity_and_retains_current_controls() {
    let session = source();
    let saved = saved(&session, None);
    let old_next = saved.next_interaction;
    let mut current = Session::new(SessionId(99), Epoch(2)).unwrap();
    current.pause_reading(true);
    current.set_auto_condition(false);
    current.reserve_interaction_generations(&saved).unwrap();
    assert_eq!(current.next_interaction, old_next);
    let mut install = saved.install_begin(current.identity());
    assert!(!install.step(0).unwrap());
    let mut units = 0;
    while !install.step(1).unwrap() {
        units += 1;
        assert!(units < 1_000);
    }
    assert!(units > 5);
    let display = install.finish(&mut current).unwrap();
    assert!(current.reading().paused());
    assert!(!current.reading().auto_ready());
    let Interaction::Dialogue(dialogue) = current.reading_data.active.as_ref().unwrap() else {
        unreachable!()
    };
    for id in [
        dialogue.id,
        dialogue.lines[0].id.interaction_id,
        dialogue.current_line_ids[0].interaction_id,
        current.reading_data.history[0].interaction_id,
        current.reading_data.history[0].lines[0].id.interaction_id,
        display.lines[0].id.interaction_id,
    ] {
        assert_eq!(id.identity, current.identity());
    }
}

#[test]
fn completed_wait_keeps_final_visible_row_without_a_projection_tick() {
    let mut session = source();
    let Interaction::Dialogue(dialogue) = session.reading_data.active.as_ref().unwrap() else {
        unreachable!()
    };
    let page = DisplayPage {
        lines: dialogue.lines.clone(),
        cleared: false,
    };
    let id = dialogue.id;
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    let saved = saved(&session, Some(&page));
    assert!(saved.data.active.is_none());
    assert!(matches!(
        saved.data.completion.as_ref().unwrap().outcome,
        crate::CompletionOutcome::Advanced
    ));
    assert!(saved.display.lines[0].playback_complete());
    assert_eq!(
        saved.display.lines[0].visible_text(),
        page.lines[0].fallback_text()
    );
}
