//! Runtime regressions for real source compilation, waits, GC and cancellation.
use super::*;
use crate::{Epoch, InputAction, OwnerLifecycle, SessionId, SessionLimits};
use aside_vm::bytecode as bc;

const STORY: &str =
    "use reel::prelude::*; pub fn opening() { narrator(\"你好\"); narrator(\"again\"); }";
fn session() -> Session {
    Session::new(SessionId(41), Epoch(9), SessionLimits::default()).unwrap()
}
fn next(vm: &mut SessionVm, session: &mut Session) -> RunOutcome {
    for _ in 0..10_000 {
        let outcome = vm
            .step(
                session,
                SliceBudget {
                    instructions: 1,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap();
        if !matches!(outcome, RunOutcome::BudgetYield(_)) {
            return outcome;
        }
    }
    panic!("bounded test steps exhausted")
}
fn setup(source: &str, context: Context) -> (Session, SessionVm) {
    let mut s = session();
    let story = source::compile_story(source, "story", 7, "opening").unwrap();
    let mut vm = SessionVm::load(&s, story.program, VmLimits::default()).unwrap();
    vm.start(&s, story.initializer).unwrap();
    assert!(matches!(next(&mut vm, &mut s), RunOutcome::Returned(_)));
    vm.discard_delivery(&s).unwrap();
    // Tests exercise effective caller restrictions without forging a ticket.
    vm.task = Some(
        vm.vm
            .start(story.entry, &[], vm.root_owner, context)
            .unwrap(),
    );
    (s, vm)
}
fn collect(vm: &mut SessionVm) {
    vm.begin_collection().unwrap();
    while vm.vm.heap().phase() != aside_vm::GcPhase::Idle {
        vm.gc_step(GcBudget {
            units: 1,
            ..Default::default()
        })
        .unwrap();
    }
}

#[test]
fn independent_registration_rejects_package_effect_mutability_abi_and_capability_lies() {
    for alteration in 0..4 {
        let story = source::compile_story(STORY, "story", 7, "opening").unwrap();
        let mut p = story.program.package().clone();
        let signature = p.natives[0].signature.index();
        match alteration {
            0 => p.signatures[signature].effects = Effects(Effects::HOST),
            1 => p.signatures[signature].parameters[0].mutable = true,
            2 => p.natives[0].abi_version += 1,
            _ => {
                p.natives[0].capabilities.clear();
            }
        }
        let program = bc::verify(p, bc::Limits::default()).unwrap();
        assert!(SessionVm::load(&session(), program, VmLimits::default()).is_err());
    }
}

#[test]
fn private_entry_and_optional_parameters_use_real_function_mapping() {
    let story = source::compile_story("fn opening(x = 1) { x }", "story", 7, "opening").unwrap();
    assert_ne!(story.initializer, story.entry);
    assert!(source::compile_story("fn opening(x) { x }", "story", 7, "opening").is_err());
    assert!(
        source::compile_story("fn outer() { fn opening() {} }", "story", 7, "opening").is_err()
    );
}

#[test]
fn entry_mapping_uses_root_declaration_identity_despite_same_named_block_function() {
    let code = "if true { fn opening(required) { required } } fn opening() { 2 }";
    let story = source::compile_story(code, "story", 7, "opening").unwrap();
    let p = story.program.package();
    let function = &p.functions[story.entry.index()];
    assert!(
        p.signatures[function.signature.index()]
            .parameters
            .is_empty()
    );
    let root_start = code.rfind("fn opening()").unwrap() as u32;
    assert!(
        function
            .source_map
            .iter()
            .any(|entry| entry.span.start >= root_start)
    );
}

#[test]
fn malformed_duplicate_choices_leave_no_publication_or_decoder_roots() {
    for options in [
        "[{key: \"a\", label: \"A\"}, {key: \"a\", label: \"again\"}]",
        "[{key: \"a\"}]",
        "[{key: \"a\", label: 1}]",
    ] {
        let code = format!("use reel::prelude::*; fn opening() {{ choose({options}); }}");
        let (mut s, mut vm) = setup(&code, Context::Ordinary);
        let RunOutcome::HostCall(ticket) = next(&mut vm, &mut s) else {
            panic!("choice")
        };
        let before = vm.vm.heap().stats().roots;
        assert!(vm.dispatch(&mut s, ticket, "en", 256).is_err());
        assert!(s.view().interaction.is_none());
        assert!(s.view().history.is_empty());
        assert!(vm.pending.is_none());
        assert!(vm.choice_reader.is_none());
        assert_eq!(vm.vm.heap().stats().roots, before);
    }
}

// This source additionally requires the collection executor increment. The
// shared read-interface dependency alone does not provide MakeArray/MakeTable.
#[test]
fn choice_decode_yields_with_roots_and_returns_original_key_once() {
    let (mut s, mut vm) = setup(
        "use reel::prelude::*; fn opening() {\n\
        let answer = choose([{ key: \"a\", label: \"A\" }, { key: \"b\", label: \"B\" }, { key: \"c\", label: \"C\" }, { key: \"d\", label: \"D\" }]);\n\
        narrator(answer); }",
        Context::Ordinary,
    );
    let RunOutcome::HostCall(ticket) = next(&mut vm, &mut s) else {
        panic!("choice ticket")
    };
    let mut published = false;
    for _ in 0..1000 {
        match vm.dispatch(&mut s, ticket, "en", 1).unwrap() {
            NativeDispatch::Decoding { work } => {
                assert!(work <= 1);
                assert!(s.view().interaction.is_none());
                collect(&mut vm);
            }
            NativeDispatch::Published { work, .. } => {
                assert!(work <= 1);
                published = true;
                break;
            }
        }
    }
    assert!(published);
    let original = vm
        .vm
        .heap()
        .root_value(vm.pending.as_ref().unwrap().keys[3].root)
        .unwrap();
    collect(&mut vm);
    let crate::InteractionView::Choice(choice) = s.view().interaction.unwrap() else {
        panic!("choice")
    };
    let selected = choice.options()[3].id().clone();
    s.handle_local_input(s.root_owner(), InputAction::Select(selected.clone()))
        .unwrap();
    assert!(
        s.handle_local_input(s.root_owner(), InputAction::Select(selected))
            .is_err()
    );
    let RunOutcome::HostCall(next_ticket) = next(&mut vm, &mut s) else {
        panic!("narrator")
    };
    assert!(vm.vm.host_arguments(next_ticket).unwrap()[0].same_bits(original));
    assert!(
        vm.vm
            .complete(ticket.wait_id(), VmCompletion::Ready(original))
            .is_err()
    );
}

#[test]
fn effective_no_wait_has_no_history_or_interaction_publication() {
    for context in [Context::NoWait, Context::Init, Context::Derived] {
        let (mut s, mut vm) = setup(STORY, context);
        let RunOutcome::HostCall(ticket) = next(&mut vm, &mut s) else {
            panic!("native ticket")
        };
        assert!(vm.dispatch_narrator(&mut s, ticket, "en").is_err());
        assert!(s.view().interaction.is_none());
        assert!(s.view().history.is_empty());
        assert_eq!(s.next_interaction, Some(1));
    }
}

#[test]
fn narrator_wait_maps_exact_task_and_resumes_once_across_gc() {
    let (mut s, mut vm) = setup(STORY, Context::Ordinary);
    let RunOutcome::HostCall(ticket) = next(&mut vm, &mut s) else {
        panic!("native ticket")
    };
    let id = vm.dispatch_narrator(&mut s, ticket, "en").unwrap();
    assert_eq!(vm.pending.as_ref().unwrap().task, ticket.task);
    assert_eq!(vm.pending.as_ref().unwrap().wait, ticket.wait_id());
    assert_eq!(vm.pending.as_ref().unwrap().interaction, id);
    assert!(vm.dispatch_narrator(&mut s, ticket, "en").is_err());
    collect(&mut vm);
    let owner = s.root_owner();
    s.handle_local_input(owner, InputAction::Advance(id))
        .unwrap();
    assert!(s.completion.is_none());
    s.handle_local_input(owner, InputAction::Advance(id))
        .unwrap();
    let RunOutcome::HostCall(second) = next(&mut vm, &mut s) else {
        panic!("second native")
    };
    assert_ne!(ticket, second);
    assert!(
        vm.vm
            .complete(ticket.wait_id(), VmCompletion::Ready(Value::NULL))
            .is_err()
    );
    vm.dispatch_narrator(&mut s, second, "en").unwrap();
    assert_eq!(s.view().history.len(), 2);
}

#[test]
fn staged_history_can_be_discarded_without_spending_identity() {
    let mut s = session();
    let owner = s.root_owner();
    let line = DialogueLineInput {
        speaker_id: None,
        speaker_name: "",
        content_id: "source:1",
        text: "hello",
    };
    let staged =
        publication::Publication::stage(&mut s, |s| s.begin_say(owner, &[line], "en")).unwrap();
    let id = staged.id();
    assert!(s.view().interaction.is_none());
    assert!(s.view().history.is_empty());
    assert_eq!(s.history_text_bytes, 0);
    drop(staged); // same rollback path as failed VM Pending acceptance
    assert_eq!(s.begin_say(owner, &[line], "en").unwrap(), id);
}

#[test]
fn failed_pending_response_rolls_back_history_and_releases_new_key_roots() {
    let (mut s, mut vm) = setup(STORY, Context::Ordinary);
    let RunOutcome::HostCall(ticket) = next(&mut vm, &mut s) else {
        panic!("native ticket")
    };
    let owner = s.root_owner();
    let staged = publication::Publication::stage(&mut s, |s| {
        s.begin_say(
            owner,
            &[DialogueLineInput {
                speaker_id: None,
                speaker_name: "",
                content_id: "source",
                text: "text",
            }],
            "en",
        )
    })
    .unwrap();
    let key = vm.vm.heap_mut().alloc_string("key").unwrap();
    let root = vm
        .vm
        .heap_mut()
        .add_root(vm.root_owner, RootKind::Host, key)
        .unwrap();
    vm.vm
        .respond(ticket, HostResponse::Ready(Value::NULL))
        .unwrap();
    assert!(
        vm.accept_wait(
            &mut s,
            ticket,
            staged,
            vec![RetainedKey {
                key: "key".into(),
                root
            }]
        )
        .is_err()
    );
    assert!(vm.pending.is_none());
    assert!(s.view().interaction.is_none());
    assert!(s.view().history.is_empty());
    assert_eq!(s.next_interaction, Some(1));
    assert_eq!(s.history_text_bytes, 0);
    assert!(vm.vm.heap().root_value(root).is_err());
}

#[test]
fn revoked_wait_never_resumes_and_cleanup_only_is_not_a_result() {
    let (mut s, mut vm) = setup(STORY, Context::Ordinary);
    let RunOutcome::HostCall(ticket) = next(&mut vm, &mut s) else {
        panic!("native ticket")
    };
    vm.dispatch_narrator(&mut s, ticket, "en").unwrap();
    vm.revoke(&mut s).unwrap();
    assert!(
        vm.vm
            .complete(ticket.wait_id(), VmCompletion::Ready(Value::NULL))
            .is_err()
    );
    assert!(vm.step(&mut s, SliceBudget::default(), || 0).is_err());
    assert_eq!(
        s.take_completion().unwrap().delivery(),
        CompletionDelivery::CleanupOnly
    );
    for _ in 0..10_000 {
        if s.owner_lifecycle(s.root_owner()) == Some(OwnerLifecycle::Retired) {
            return;
        }
        vm.cleanup_step(
            &mut s,
            1,
            SliceBudget {
                instructions: 1,
                ..Default::default()
            },
            || 0,
        )
        .unwrap();
    }
    panic!("cleanup did not retire")
}

#[test]
fn cancellation_cleans_host_roots_with_no_task_and_counts_holes() {
    let (mut s, mut vm) = setup("pub fn opening() { \"retained\" }", Context::Ordinary);
    assert!(matches!(next(&mut vm, &mut s), RunOutcome::Returned(_)));
    vm.retain_return(&s).unwrap();
    assert!(vm.task().is_none());
    let root = vm.result_root.unwrap();
    let hole = vm
        .vm
        .heap_mut()
        .add_root(vm.root_owner, RootKind::Host, Value::NULL)
        .unwrap();
    vm.vm.heap_mut().release_root(hole).unwrap();
    let end = vm.vm.heap().root_slots();
    vm.revoke(&mut s).unwrap();
    assert_eq!(vm.root_end, end);
    let mut work = 0;
    while s.owner_lifecycle(s.root_owner()) != Some(OwnerLifecycle::Retired) {
        work += vm
            .cleanup_step(&mut s, 1, SliceBudget::default(), || 0)
            .unwrap();
    }
    assert_eq!(work, end);
    assert!(vm.vm.heap().root_value(root).is_err());
    assert!(vm.start(&s, bc::FunctionId(0)).is_err());
}

#[test]
fn text_read_chunks_can_split_utf8_and_accept_64k_without_a_large_heap_read() {
    let limits = aside_vm::HeapLimits {
        max_mutation_bytes: 65536,
        ..Default::default()
    };
    let mut heap = aside_vm::Heap::new(SessionEpoch(1), limits).unwrap();
    let text = format!("{}é{}", "a".repeat(4095), "b".repeat(65536 - 4097));
    let value = heap.alloc_string(&text).unwrap();
    assert_eq!(text::read_string(&heap, value, 65536, 4096).unwrap(), text);
    assert!(text::read_string(&heap, value, 65535, 4096).is_err());
    assert!(text::read_string(&heap, value, 65536, 0).is_err());
    let mut normal = aside_vm::Heap::new(SessionEpoch(2), Default::default()).unwrap();
    let small = normal.alloc_string("é").unwrap();
    assert!(normal.read_bytes(small, 0, &mut [0; 4097]).is_err());
    assert_eq!(text::read_string(&normal, small, 2, 1).unwrap(), "é");
    assert!(normal.alloc_string(&"x".repeat(4096)).is_ok());
    assert!(normal.alloc_string(&"x".repeat(4097)).is_err());
}

#[test]
fn selected_64k_key_is_retained_original_and_one_more_byte_rejects_before_publish() {
    // The fixture explicitly raises the constant-admission limit. Default
    // 4096-byte constant loading is a separate VM boundary, not bypassed by host.
    for len in [65536, 65537] {
        let code = format!(
            "use reel::prelude::*; fn opening() {{ choose([{{key: \"{}\", label: \"\"}}]) }}",
            "k".repeat(len)
        );
        let story = source::compile_story(&code, "story", 7, "opening").unwrap();
        let mut s = session();
        let mut limits = VmLimits::default();
        limits.heap.max_mutation_bytes = len as u32;
        let mut vm = SessionVm::load(&s, story.program, limits).unwrap();
        assert_eq!(vm.read_limit, 4096);
        vm.start(&s, story.initializer).unwrap();
        assert!(matches!(next(&mut vm, &mut s), RunOutcome::Returned(_)));
        vm.discard_delivery(&s).unwrap();
        vm.start(&s, story.entry).unwrap();
        let RunOutcome::HostCall(ticket) = next(&mut vm, &mut s) else {
            panic!("choice")
        };
        let result = vm.dispatch(&mut s, ticket, "en", 64);
        if len > 65536 {
            assert!(result.is_err());
            assert!(s.view().interaction.is_none());
            assert!(vm.pending.is_none());
            assert!(vm.choice_reader.is_none());
            continue;
        }
        assert!(matches!(result.unwrap(), NativeDispatch::Published { .. }));
        let original = vm
            .vm
            .heap()
            .root_value(vm.pending.as_ref().unwrap().keys[0].root)
            .unwrap();
        collect(&mut vm);
        let crate::InteractionView::Choice(choice) = s.view().interaction.unwrap() else {
            panic!("choice")
        };
        let selected = choice.options()[0].id().clone();
        s.handle_local_input(s.root_owner(), InputAction::Select(selected))
            .unwrap();
        assert!(matches!(next(&mut vm, &mut s), RunOutcome::Returned(_)));
        assert!(vm.retain_return(&s).unwrap().same_bits(original));
        collect(&mut vm);
        assert_eq!(vm.vm.heap().info(original).unwrap().len as usize, len);
    }
}
