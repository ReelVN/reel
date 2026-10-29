use super::*;
use crate::{Epoch, SessionId};
use aside_vm::{GcBudget, GcPhase, HeapLimits, RootKind, RootOwner, SessionEpoch};
use std::time::Instant;

fn collect(heap: &mut Heap) {
    heap.begin_collection().unwrap();
    while heap.phase() != GcPhase::Idle {
        let progress = heap.gc_step(GcBudget::default()).unwrap();
        assert!(progress.units != 0 || progress.cycle_complete);
    }
}

fn retained_history(rows: usize) {
    let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
    for i in 0..rows {
        let text = format!("line {i}: a distinct recorded paragraph retained only by history");
        let id = format!("recorded:{i}");
        session
            .record_history(
                &[crate::DialogueLineInput {
                    speaker_id: None,
                    speaker_name: "",
                    content_id: &id,
                    text: text.as_str().into(),
                    history: Default::default(),
                }],
                "en",
            )
            .unwrap();
    }
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let mut history = History::new(&mut heap).unwrap();
    let roots = heap.alloc_array(&[history.value]).unwrap();
    heap.add_root(RootOwner(1), RootKind::Host, roots).unwrap();
    history
        .sync(&mut heap, &session, None, Value::NULL, roots, 4096)
        .unwrap();
    collect(&mut heap);
    let before = heap.stats();
    let session_before = session.storage_usage();
    let original = history.value;
    let group = heap.sequence_get(original, 0).unwrap();
    let lines = heap
        .host_property(PropertyId {
            host: group,
            field: HistoryGroupField::Lines as u32,
        })
        .unwrap();
    let line = heap.sequence_get(lines, 0).unwrap();
    let text = heap
        .host_property(PropertyId {
            host: line,
            field: HistoryLineField::Text as u32,
        })
        .unwrap();
    // Handles do not keep the deleted graph alive.
    let removed = [group, lines, line, text].map(|value| heap.handle(value).unwrap());
    let tail: Vec<_> = (rows - 37..rows)
        .map(|i| heap.sequence_get(original, i as u32).unwrap())
        .collect();
    let version = heap.info(original).unwrap().content_version;

    let start = Instant::now();
    session.clear_history(37);
    let trim = start.elapsed();
    let start = Instant::now();
    history
        .sync(&mut heap, &session, None, Value::NULL, roots, 4096)
        .unwrap();
    let sync = start.elapsed();
    assert!(history.value.same_bits(original));
    assert_eq!(heap.info(original).unwrap().content_version, version + 1);
    assert_eq!(heap.sequence_len(original).unwrap(), 37);
    for (i, value) in tail.iter().enumerate() {
        assert!(
            heap.sequence_get(original, i as u32)
                .unwrap()
                .same_bits(*value)
        );
    }
    assert_eq!(session.view().history.len(), 37);
    assert!(heap.collection_due());
    let start = Instant::now();
    collect(&mut heap);
    let gc = start.elapsed();
    assert!(
        removed
            .iter()
            .all(|&handle| heap.import_handle(handle).is_err())
    );
    assert!(tail.iter().all(|&value| heap.info(value).is_ok()));
    let after = heap.stats();
    let session_after = session.storage_usage();
    assert!(after.live_objects < before.live_objects / 2);
    assert!(after.payload_bytes < before.payload_bytes / 2);
    assert_eq!(after.pending_bytes, 0);
    assert_eq!(session_after.reserved_history_slots, 37);
    eprintln!(
        "history rows={rows} keep=37: trim={trim:?} sync={sync:?} gc={gc:?}; \
         VM objects {} -> {}, payload {} -> {} bytes, charged {} -> {} bytes; \
         Session backing {} -> {} bytes, retained text {} -> {} bytes",
        before.live_objects,
        after.live_objects,
        before.payload_bytes,
        after.payload_bytes,
        before.charged_bytes,
        after.charged_bytes,
        session_before.allocated_capacity_bytes,
        session_after.allocated_capacity_bytes,
        session_before.retained_history_text_bytes,
        session_after.retained_history_text_bytes,
    );
}

#[test]
fn retaining_history_releases_prefix_edges_with_one_array_commit() {
    retained_history(200);
}

#[test]
fn omitted_history_appends_keep_the_existing_text_graph() {
    use super::super::super::rich_text;
    use crate::{DialogueLineInput, HistoryInput};
    use reel_core::text::Text;

    for source in [Text::from("原文é"), Text::parse("[b]原文é[/b]").unwrap()] {
        for chunk in [1, 4096] {
            let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
            let owner = session.root_owner();
            let id = session
                .begin_open_say(
                    owner,
                    &[DialogueLineInput {
                        speaker_id: None,
                        speaker_name: "",
                        content_id: "initial",
                        text: (&source).into(),
                        history: HistoryInput::Display,
                    }],
                    "zh-CN",
                    false,
                )
                .unwrap();
            let story =
                crate::aside::source::compile_story("pub fn opening() {}", "story", 1, "opening")
                    .unwrap();
            let mut runtime = crate::aside::SessionVm::load(
                &session,
                story.program,
                crate::aside::VmLimits::default(),
            )
            .unwrap();
            let heap = runtime.vm.heap_mut();
            let mut history = History::new(heap).unwrap();
            let roots = heap.alloc_array(&[history.value]).unwrap();
            heap.add_root(RootOwner(1), RootKind::Host, roots).unwrap();
            history
                .sync(heap, &session, None, Value::NULL, roots, chunk)
                .unwrap();
            // The active model's lines are unused when display and history differ.
            let fields = heap.alloc_table().unwrap();
            let active = heap.alloc_array(&[]).unwrap();
            let model = heap.alloc_immutable_host(fields, &[active]).unwrap();
            heap.array_push(roots, model).unwrap();
            let group = heap.sequence_get(history.value, 0).unwrap();
            let lines = heap
                .host_property(PropertyId {
                    host: group,
                    field: HistoryGroupField::Lines as u32,
                })
                .unwrap();
            let row = heap.sequence_get(lines, 0).unwrap();
            let property = PropertyId {
                host: row,
                field: HistoryLineField::Text as u32,
            };
            let original = heap.host_property(property).unwrap();
            let before = heap.stats();
            for i in 0..32 {
                session
                    .append_dialogue(
                        owner,
                        id,
                        &Text::from("追加"),
                        &format!("part:{i}"),
                        HistoryInput::Omit,
                    )
                    .unwrap();
                history
                    .update_dialogue(
                        heap,
                        &session,
                        session.open_dialogue(id).unwrap(),
                        model,
                        chunk,
                    )
                    .unwrap();
                assert!(heap.host_property(property).unwrap().same_bits(original));
            }
            let after = heap.stats();
            assert_eq!(after.live_objects, before.live_objects);
            assert_eq!(after.charged_bytes, before.charged_bytes);
            assert!(rich_text::matches(heap, original, &source, chunk).unwrap());
            session
                .append_dialogue(
                    owner,
                    id,
                    &Text::from("正文"),
                    "changed",
                    HistoryInput::Text("历史".into()),
                )
                .unwrap();
            history
                .update_dialogue(
                    heap,
                    &session,
                    session.open_dialogue(id).unwrap(),
                    model,
                    chunk,
                )
                .unwrap();
            let changed = heap.host_property(property).unwrap();
            assert!(!changed.same_bits(original));
            assert!(
                rich_text::matches(
                    heap,
                    changed,
                    session.view().history[0].lines()[0].content().styled(),
                    chunk
                )
                .unwrap()
            );
            history
                .update_dialogue(
                    heap,
                    &session,
                    session.open_dialogue(id).unwrap(),
                    model,
                    chunk,
                )
                .unwrap();
            assert!(heap.host_property(property).unwrap().same_bits(changed));
        }
    }
}

#[test]
#[ignore = "release history trim/sync/GC measurement; no UI or process-RSS claim"]
fn history_retention_cost() {
    for rows in [10_000, 100_000] {
        retained_history(rows);
    }
}
