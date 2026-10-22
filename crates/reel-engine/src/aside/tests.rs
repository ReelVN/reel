//! Runtime regressions for real source compilation, waits, GC and cancellation.
use super::*;
use crate::{Epoch, InputAction, OwnerLifecycle, SessionId};
use aside_vm::bytecode as bc;
mod activities;
mod auto_condition;
mod box_style;
mod buttons;
mod clock_batches;
mod component_cost;
mod conditions;
mod content_identity;
mod curves;
mod dialogue_append;
mod dialogue_history;
mod documentation;
mod dynamic_stage;
mod frame_animation;
mod geometry;
mod host_budget;
mod localization;
mod masks;
mod media_positions;
mod models;
mod particles;
mod property_configuration;
mod result_use;
mod results;
mod retired_nodes;
mod rich_text;
mod scopes;
mod snapshots;
mod source_inline;
mod source_types;
mod speech_flow;
mod tasks;
mod text_fade;
mod transitions;
mod video;
mod virtual_lists;
mod window;

const STORY: &str =
    "use reel::prelude::*; pub fn opening() { narrator(\"你好\"); narrator(\"again\"); }";
fn session() -> Session {
    Session::new(SessionId(41), Epoch(9)).unwrap()
}
fn next(vm: &mut SessionVm, session: &mut Session) -> RunOutcome {
    for _ in 0..10_000 {
        let outcome = vm
            .step(
                session,
                SessionBudget::Exact(SliceBudget {
                    instructions: 1,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap();
        let SessionStep::Vm(outcome) = outcome else {
            continue;
        };
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
    vm.discard_delivery(&mut s).unwrap();
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
fn default_reel_environment_respects_author_scopes_and_explicit_aliases() {
    let code = r#"use reel::prelude::text as reel_text;
        use std::random as standard_random;
        fn text(value) { value + " author" }
        fn opening() {
            let rng = random(seed: 42);
            let same = standard_random(seed: 42);
            if rng.int(0, 1000) != same.int(0, 1000) { throw "standard prelude"; }
            let set = rng.pick;
            if set(["Reel"]) != "Reel" { throw "standard native dispatch"; }
            if text("module") != "module author" { throw "module precedence"; }
            let image = |value| value + " local";
            if image("binding") != "binding local" { throw "local precedence"; }
            let picture = reel_text("Reel");
            stage.replace(column() { picture; row() { reel_text("nested"); } });
            narrator("ready");
            true
        }"#;
    let (mut session, mut vm) = setup(code, Context::Ordinary);
    let mut waits = 0;
    loop {
        match next(&mut vm, &mut session) {
            RunOutcome::HostCall(ticket) => {
                if let NativeDispatch::Published { interaction, .. } = vm
                    .dispatch(&mut session, ticket, "en", NativeBudget::Exact(8))
                    .unwrap()
                {
                    vm.sync_stage(&session).unwrap();
                    assert!(vm.stage().nodes().any(|node| {
                        node.kind == reel_ui::stage::NodeKind::Text("Reel".into())
                    }));
                    waits += 1;
                    let owner = session.root_owner();
                    session
                        .handle_local_input(owner, InputAction::Advance(interaction))
                        .unwrap();
                    session
                        .handle_local_input(owner, InputAction::Advance(interaction))
                        .unwrap();
                }
            }
            RunOutcome::Returned(_) => break,
            other => panic!("default environment script: {other:?}"),
        }
    }
    assert_eq!(waits, 1);
    assert_eq!(vm.retain_return(&session).unwrap().as_bool(), Some(true));
    vm.finish(&mut session).unwrap();
    assert!(
        !aside_compiler::analyze_module(
            "fn opening() { narrator(\"standalone\"); }",
            &aside_compiler::AnalysisOptions::default(),
            &mut aside_compiler::semantic::MemoryResolver::default(),
        )
        .unwrap()
        .is_ok()
    );
}

#[test]
fn image_nodes_validate_before_writing_and_preserve_identity_and_origin() {
    use reel_ui::{properties::ObjectFit, stage::NodeKind};
    let code = r#"use reel::prelude::*; fn dynamic(value) {value}
        fn opening() {
            let p = image("original.png", width: 200, opacity: 0.5, rotation: 450, scale_x: -2, scale_y: 0, anchor_x: 1.5, anchor_y: -1);
            let mut errors = 0;
            try { p.source = ""; } catch e { errors += 1; }
            try { p.fit = "unknown"; } catch e { errors += 1; }
            try { p.source = dynamic(42); } catch e { errors += 1; }
            try { p.rotation = dynamic("invalid"); } catch e { errors += 1; }
            try { p.scale_x = dynamic(null); } catch e { errors += 1; }
            try { p.anchor_y = dynamic(false); } catch e { errors += 1; }
            if errors != 6 || p.source != "original.png" || p.fit != "contain" { throw "bad validation"; }
            stage.replace(stack() { p; });
            narrator("first");
            p.source = "replacement.jpg";
            p.fit = "cover";
            p.height = 160;
            p.rotation = -90;
            p.scale_x = 0.5;
            p.scale_y = 3;
            p.anchor_x = 0.5;
            p.anchor_y = 0;
            narrator("second");
            stage.replace(stack());
            try { p.source = "retired.png"; return false; } catch e { return true; }
        }"#;
    let (mut session, mut vm) = setup(code, Context::Ordinary);
    let mut identity = None;
    let mut waits = 0;
    loop {
        match next(&mut vm, &mut session) {
            RunOutcome::HostCall(ticket) => {
                if let NativeDispatch::Published { interaction, .. } = vm
                    .dispatch(&mut session, ticket, "en", NativeBudget::Exact(8))
                    .unwrap()
                {
                    collect(&mut vm);
                    vm.sync_stage(&session).unwrap();
                    let node = vm
                        .stage()
                        .nodes()
                        .find(|node| matches!(node.kind, NodeKind::Image { .. }))
                        .unwrap();
                    assert_eq!(*identity.get_or_insert(node.id), node.id);
                    let NodeKind::Image {
                        source: source_name,
                        fit,
                        origin,
                        ..
                    } = &node.kind
                    else {
                        unreachable!()
                    };
                    assert_eq!(origin.as_deref(), Some("story:3:21"));
                    assert_eq!(
                        source_name.key(),
                        if waits == 0 {
                            "original.png"
                        } else {
                            "replacement.jpg"
                        }
                    );
                    assert_eq!(
                        *fit,
                        if waits == 0 {
                            ObjectFit::Contain
                        } else {
                            ObjectFit::Cover
                        }
                    );
                    assert_eq!(node.props.width, Some(200.));
                    assert_eq!(
                        [
                            node.props.rotation,
                            node.props.scale_x,
                            node.props.scale_y,
                            node.props.anchor_x,
                            node.props.anchor_y
                        ],
                        if waits == 0 {
                            [450., -2., 0., 1.5, -1.]
                        } else {
                            [-90., 0.5, 3., 0.5, 0.]
                        }
                    );
                    assert_eq!(
                        node.props.height,
                        if waits == 0 { None } else { Some(160.) }
                    );
                    waits += 1;
                    let owner = session.root_owner();
                    session
                        .handle_local_input(owner, InputAction::Advance(interaction))
                        .unwrap();
                    session
                        .handle_local_input(owner, InputAction::Advance(interaction))
                        .unwrap();
                }
            }
            RunOutcome::Returned(_) => break,
            other => panic!("image script: {other:?}"),
        }
    }
    assert_eq!(waits, 2);
    assert_eq!(vm.retain_return(&session).unwrap().as_bool(), Some(true));
    vm.finish(&mut session).unwrap();
}

#[test]
fn known_host_contract_errors_are_rejected_at_the_author_expression() {
    use aside_compiler::DiagnosticCode as D;
    for (body, expected, location) in [
        (
            "let timer=clock(); timer.ticks=5;",
            D::ImmutableBinding,
            "timer.ticks",
        ),
        (
            "let result=completion(); result.pending=false;",
            D::ImmutableBinding,
            "result.pending",
        ),
        (
            "let node=image(\"x.png\"); node.source=42;",
            D::TypeMismatch,
            "42",
        ),
        (
            "let node=text(\"x\"); node.text_wrap=0;",
            D::TypeMismatch,
            "0",
        ),
        (
            "let value=rich(\"x\"); value.plain=\"changed\";",
            D::ImmutableBinding,
            "value.plain",
        ),
        ("narrator(\"x\",history:[]);", D::TypeMismatch, "history"),
        ("mask.alpha(\"x.png\",x:{});", D::TypeMismatch, "x:"),
        (
            "let root=column(); let add=root.add; add(receiver:root,child:stack());",
            D::InvalidCall,
            "receiver",
        ),
    ] {
        let code = format!("fn opening() {{{body}}}");
        let Err(source::SourceError::Analysis(error)) =
            source::compile_module(&code, "invalid", "invalid.aside", 1)
        else {
            panic!("expected a source contract diagnostic for {body}");
        };
        assert!(
            error
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == expected
                    && code[aside_compiler::byte_range(diagnostic.range)].contains(location)),
            "{body}: {:?}",
            error.diagnostics
        );
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
            1 => {
                p.signatures[signature]
                    .parameters
                    .last_mut()
                    .unwrap()
                    .mutable = true
            }
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
        assert!(
            vm.dispatch(&mut s, ticket, "en", NativeBudget::Exact(256))
                .is_err()
        );
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
        match vm
            .dispatch(&mut s, ticket, "en", NativeBudget::Exact(1))
            .unwrap()
        {
            NativeDispatch::Ready { .. } | NativeDispatch::BudgetYield { .. } => {
                panic!("choice must wait")
            }
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
    let original = vm.pending.as_ref().unwrap().keys.as_ref().unwrap().entries[3].value;
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
        match next(&mut vm, &mut s) {
            RunOutcome::HostCall(ticket) if matches!(context, Context::NoWait | Context::Init) => {
                assert!(
                    vm.dispatch_narrator(&mut s, ticket, "en", NativeBudget::Exact(32))
                        .is_err()
                );
            }
            RunOutcome::Fault(id) if context == Context::Derived => {
                assert_eq!(
                    vm.vm.delivery_fault(id).unwrap().unwrap().kind,
                    FaultKind::Effect
                );
            }
            other => panic!("unexpected restricted native outcome: {other:?}"),
        }
        assert!(s.view().interaction.is_none());
        assert!(s.view().history.is_empty());
        assert_eq!(s.next_interaction, Some(1));
    }
}

#[test]
fn narrator_wait_maps_exact_task_and_resumes_once_across_gc() {
    for source in [
        STORY,
        "use reel::prelude::*; pub fn opening() { let say=narrator; say(\"你好\"); say?(\"again\"); }",
    ] {
        let (mut s, mut vm) = setup(source, Context::Ordinary);
        let RunOutcome::HostCall(ticket) = next(&mut vm, &mut s) else {
            panic!("native ticket")
        };
        let NativeDispatch::Published {
            interaction: id, ..
        } = vm
            .dispatch_narrator(&mut s, ticket, "en", NativeBudget::Exact(32))
            .unwrap()
        else {
            panic!("narrator publication");
        };
        assert_eq!(vm.pending.as_ref().unwrap().task, ticket.task);
        assert_eq!(vm.pending.as_ref().unwrap().wait, ticket.wait_id());
        assert_eq!(vm.pending.as_ref().unwrap().interaction, id);
        assert!(
            vm.dispatch_narrator(&mut s, ticket, "en", NativeBudget::Exact(32))
                .is_err()
        );
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
        vm.dispatch_narrator(&mut s, second, "en", NativeBudget::Exact(32))
            .unwrap();
        assert_eq!(s.view().history.len(), 2);
    }
}

#[test]
fn staged_history_can_be_discarded_without_spending_identity() {
    let mut s = session();
    let owner = s.root_owner();
    let line = DialogueLineInput {
        history: Default::default(),
        speaker_id: None,
        speaker_name: "",
        content_id: "source:1",
        text: ("hello").into(),
    };
    let staged =
        publication::Publication::stage(&mut s, |s| s.begin_say(owner, &[line], "en", true))
            .unwrap();
    let id = staged.id();
    assert!(s.view().interaction.is_none());
    assert!(s.view().history.is_empty());
    assert_eq!(s.storage_usage().retained_history_text_bytes, 0);
    drop(staged); // same rollback path as failed VM Pending acceptance
    assert_eq!(s.begin_say(owner, &[line], "en", true).unwrap(), id);
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
                history: Default::default(),
                speaker_id: None,
                speaker_name: "",
                content_id: "source",
                text: ("text").into(),
            }],
            "en",
            true,
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
            Some(RetainedKeys {
                root,
                entries: vec![RetainedKey {
                    key: "key".into(),
                    value: key
                }],
            })
        )
        .is_err()
    );
    assert!(vm.pending.is_none());
    assert!(s.view().interaction.is_none());
    assert!(s.view().history.is_empty());
    assert_eq!(s.next_interaction, Some(1));
    assert_eq!(s.storage_usage().retained_history_text_bytes, 0);
    assert!(vm.vm.heap().root_value(root).is_err());
}

#[test]
fn revoked_wait_never_resumes_and_cleanup_only_is_not_a_result() {
    let (mut s, mut vm) = setup(STORY, Context::Ordinary);
    let RunOutcome::HostCall(ticket) = next(&mut vm, &mut s) else {
        panic!("native ticket")
    };
    vm.dispatch_narrator(&mut s, ticket, "en", NativeBudget::Exact(32))
        .unwrap();
    vm.revoke(&mut s).unwrap();
    assert!(
        vm.vm
            .complete(ticket.wait_id(), VmCompletion::Ready(Value::NULL))
            .is_err()
    );
    assert!(
        vm.step(&mut s, SessionBudget::Exact(SliceBudget::default()), || 0)
            .is_err()
    );
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
            SessionBudget::Exact(SliceBudget {
                instructions: 1,
                ..Default::default()
            }),
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
            .cleanup_step(
                &mut s,
                1,
                SessionBudget::Exact(SliceBudget::default()),
                || 0,
            )
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
    assert_eq!(text::read_string(&heap, value, 4096).unwrap(), text);
    assert!(text::read_string(&heap, value, 0).is_err());
    let mut normal = aside_vm::Heap::new(SessionEpoch(2), Default::default()).unwrap();
    let small = normal.alloc_string("é").unwrap();
    assert!(normal.read_bytes(small, 0, &mut [0; 4097]).is_err());
    assert_eq!(text::read_string(&normal, small, 1).unwrap(), "é");
    assert!(normal.alloc_string(&"x".repeat(4096)).is_ok());
    assert!(normal.alloc_string(&"x".repeat(4097)).is_ok());
}

#[test]
fn long_story_and_many_choices_keep_complete_text_and_original_key_across_gc() {
    let long = "雨".repeat(22_000);
    let mut rows: Vec<_> = (0..1024)
        .map(|i| format!("{{key: \"key-{i}\", label: \"Option\"}}"))
        .collect();
    rows.push(format!("{{key: \"{long}\", label: \"Last option\"}}"));
    let code = format!(
        "use reel::prelude::*; fn opening() {{ let mut count = 0; while count < 300 {{ narrator(\"continued\"); count += 1; }} narrator(\"{long}\"); choose([{}]) }}",
        rows.join(",")
    );
    let (mut session, mut vm) = setup(&code, Context::Ordinary);
    let mut original = None;
    let mut choice_roots = None;
    let mut finished = false;
    for _ in 0..20_000 {
        let SessionStep::Vm(outcome) = vm
            .step(
                &mut session,
                SessionBudget::Exact(SliceBudget::default()),
                || 0,
            )
            .unwrap()
        else {
            panic!("expected main task progress");
        };
        match outcome {
            RunOutcome::BudgetYield(_) => {}
            RunOutcome::HostCall(ticket) => {
                let p = vm.vm.program().package();
                if p.symbols[p.natives[ticket.native.index()].symbol.index()] == "reel::choose"
                    && choice_roots.is_none()
                {
                    choice_roots = Some(vm.vm.heap().stats().roots);
                }
                match vm
                    .dispatch(&mut session, ticket, "en", NativeBudget::Exact(128))
                    .unwrap()
                {
                    NativeDispatch::Published { interaction, .. } => {
                        match session.view().interaction.unwrap() {
                            crate::InteractionView::Dialogue(_) => {
                                session
                                    .handle_local_input(
                                        session.root_owner(),
                                        InputAction::Advance(interaction),
                                    )
                                    .unwrap();
                                session
                                    .handle_local_input(
                                        session.root_owner(),
                                        InputAction::Advance(interaction),
                                    )
                                    .unwrap();
                            }
                            crate::InteractionView::Choice(choice) => {
                                assert_eq!(choice.options().len(), 1025);
                                let selected = choice.options()[1024].id().clone();
                                original = Some(
                                    vm.pending.as_ref().unwrap().keys.as_ref().unwrap().entries
                                        [1024]
                                        .value,
                                );
                                assert!(vm.vm.heap().stats().roots <= choice_roots.unwrap() + 2);
                                collect(&mut vm);
                                session
                                    .handle_local_input(
                                        session.root_owner(),
                                        InputAction::Select(selected),
                                    )
                                    .unwrap();
                            }
                        }
                    }
                    NativeDispatch::Decoding { .. } => collect(&mut vm),
                    NativeDispatch::Ready { .. } | NativeDispatch::BudgetYield { .. } => {
                        panic!("story native must wait")
                    }
                }
            }
            RunOutcome::Returned(_) => {
                assert!(
                    vm.retain_return(&session)
                        .unwrap()
                        .same_bits(original.unwrap())
                );
                finished = true;
                break;
            }
            other => panic!("unexpected large story outcome: {other:?}"),
        }
    }
    assert!(finished);
    assert_eq!(session.view().history.len(), 301);
    assert_eq!(session.view().history[0].text(), "continued");
    assert_eq!(session.view().history[300].text(), long);
    collect(&mut vm);
    assert_eq!(
        vm.vm.heap().info(original.unwrap()).unwrap().len as usize,
        long.len()
    );
}
#[test]
fn authored_components_keep_node_identity_across_gc_updates_and_replacement() {
    use reel_ui::stage::NodeKind;
    let code = r##"
        use reel::prelude::*;
        fn dynamic(value) {value}
        fn card(children = []) {
            column(gap: 8, padding: 12, background: "#203040") {
                for child in children { child; }
            }
        }
        fn mount() {
            let heading = text("Before", font_size: 30, color: "#ff8040");
            stage.replace(stack(width: 640, height: 480) {
                card() {
                    heading;
                    if true { text("Conditional"); }
                    for caption in ["One", "Two", "Three"] { text(caption); }
                }
            });
            heading
        }
        fn unmounted_child() {
            let child = text("Detached");
            let component = column() { child; };
            child
        }
        fn opening() {
            let detached = unmounted_child();
            narrator("Detached");
            detached.value = "Still held";
            let heading = mount();
            narrator("First");
            let mut caught = 0;
            try { heading.opacity = dynamic("wrong"); } catch error { caught += 1; }
            try { heading["color"] = "#gggggg"; } catch error { caught += 1; }
            try { heading.width = -1; } catch error { caught += 1; }
            let unchanged = heading.opacity == 1 && heading.color == "#ff8040" && heading.width == null;
            heading.visible = false;
            narrator("Hidden");
            heading.visible = true;
            heading.value = "After";
            heading.x = 24;
            heading.opacity = 0.5;
            narrator("Second");
            stage.replace(text("Replacement"));
            try { stage.replace(heading); } catch error { caught += 1; }
            try { heading.value = "Retired"; return false; }
            catch error { return caught == 4 && unchanged && detached.value == "Still held"; }
        }
    "##;
    let (mut session, mut vm) = setup(code, Context::Ordinary);
    let mut first_root = None;
    let mut heading_id = None;
    let mut publications = 0;
    let mut retained_old_root = None;
    let mut old_child = None;
    loop {
        match next(&mut vm, &mut session) {
            RunOutcome::HostCall(ticket) => {
                let package = vm.vm.program().package();
                if retained_old_root.is_none()
                    && package.symbols[package.natives[ticket.native.index()].symbol.index()]
                        == "reel::stage::replace"
                {
                    let root = vm.vm.host_arguments(ticket).unwrap()[0];
                    let children = vm.vm.heap().host_payload(root, 1).unwrap();
                    let card = vm.vm.heap().sequence_get(children, 0).unwrap();
                    let children = vm.vm.heap().host_payload(card, 1).unwrap();
                    let child = vm.vm.heap().sequence_get(children, 4).unwrap();
                    old_child = Some(vm.vm.heap().downgrade(child).unwrap());
                    // A retained old parent must not retain its retired children.
                    retained_old_root = Some(
                        vm.vm
                            .heap_mut()
                            .add_root(vm.root_owner, RootKind::Host, root)
                            .unwrap(),
                    );
                }
                let dispatch = match vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(8))
                {
                    Ok(dispatch) => dispatch,
                    Err(error) => {
                        assert!(matches!(error, AdapterError::Heap(HeapError::RetiredHost)));
                        let kind = error.native_fault_kind().unwrap();
                        assert_eq!(kind, FaultKind::InvalidState);
                        vm.reject_native(&session, ticket, error).unwrap();
                        continue;
                    }
                };
                if let NativeDispatch::Published { interaction, .. } = dispatch {
                    vm.sync_stage(&session).unwrap();
                    if publications == 0 {
                        assert!(vm.stage().root().is_none());
                        assert_eq!(vm.stage().nodes().count(), 2);
                        collect(&mut vm);
                        vm.sync_stage(&session).unwrap();
                        // The child returned from its component factory keeps
                        // the otherwise unreferenced parent alive through GC.
                        assert_eq!(vm.stage().nodes().count(), 2);
                    } else if publications == 1 {
                        let root = vm.stage().root().unwrap();
                        first_root = Some(root);
                        let card = vm
                            .stage()
                            .node(vm.stage().node(root).unwrap().children[0])
                            .unwrap();
                        assert_eq!(card.props.gap, 8.);
                        assert_eq!(card.children.len(), 5);
                        heading_id = Some(card.children[0]);
                        assert_eq!(
                            vm.stage().node(card.children[4]).unwrap().kind,
                            NodeKind::Text("Three".into())
                        );
                        collect(&mut vm);
                        vm.sync_stage(&session).unwrap();
                        assert_eq!(
                            vm.stage().node(heading_id.unwrap()).unwrap().kind,
                            NodeKind::Text("Before".into())
                        );
                    } else if publications == 2 {
                        assert_eq!(vm.stage().root(), first_root);
                        let heading = vm.stage().node(heading_id.unwrap()).unwrap();
                        assert!(!heading.props.visible);
                        assert_eq!(heading.props.opacity, 1.);
                        assert_eq!(heading.props.color, [255, 128, 64, 255]);
                        assert_eq!(heading.props.width, None);
                    } else {
                        let root = vm.stage().root().unwrap();
                        assert_eq!(Some(root), first_root);
                        let heading = vm.stage().node(heading_id.unwrap()).unwrap();
                        assert!(heading.props.visible);
                        assert_eq!(heading.kind, NodeKind::Text("After".into()));
                        assert_eq!(heading.props.x, 24.);
                        assert_eq!(heading.props.opacity, 0.5);
                    }
                    publications += 1;
                    let owner = session.root_owner();
                    session
                        .handle_local_input(owner, InputAction::Advance(interaction))
                        .unwrap();
                    session
                        .handle_local_input(owner, InputAction::Advance(interaction))
                        .unwrap();
                }
            }
            RunOutcome::Returned(_) => break,
            other => panic!("unexpected scene outcome: {other:?}"),
        }
    }
    assert_eq!(publications, 4);
    assert_eq!(vm.retain_return(&session).unwrap().as_bool(), Some(true));
    collect(&mut vm);
    vm.sync_stage(&session).unwrap();
    assert_eq!(vm.stage().nodes().count(), 1);
    assert!(vm.vm.heap().root_value(retained_old_root.unwrap()).is_ok());
    assert!(
        vm.vm
            .heap_mut()
            .upgrade(old_child.unwrap())
            .unwrap()
            .is_none()
    );
    vm.vm
        .heap_mut()
        .release_root(retained_old_root.unwrap())
        .unwrap();
    assert!(vm.stage().node(first_root.unwrap()).is_none());
    assert_eq!(
        vm.stage().node(vm.stage().root().unwrap()).unwrap().kind,
        NodeKind::Text("Replacement".into())
    );
    vm.finish(&mut session).unwrap();
    assert!(vm.stage().root().is_none());

    // An uncaught property error names the actual assignment, with the same
    // behavior for a not-yet-mounted node and a one-instruction VM budget.
    for property in ["opacity", "rotation"] {
        let code = format!(
            "fn invalid(node) {{ node.{property} = \"invalid\"; }} fn opening() {{ invalid(text(\"Fault\")); }}"
        );
        let (mut session, mut vm) = setup(&code, Context::Ordinary);
        loop {
            match next(&mut vm, &mut session) {
                RunOutcome::HostCall(ticket) => {
                    vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(8))
                        .unwrap();
                }
                RunOutcome::Fault(delivery) => {
                    let fault = vm.vm.delivery_fault(delivery).unwrap().unwrap();
                    assert_eq!(fault.kind, FaultKind::Type);
                    let span = fault.location.source.unwrap();
                    assert_eq!(
                        &code[span.start as usize..span.end as usize],
                        format!("node.{property}")
                    );
                    vm.discard_delivery(&mut session).unwrap();
                    vm.finish(&mut session).unwrap();
                    break;
                }
                other => panic!("expected property assignment fault: {other:?}"),
            }
        }
    }
}

mod keyed_lists;
mod reactive_reads_restore;
mod reactive_views;
mod text_spacing;

mod capture_included;
