//! Real portable editor input is replayed through the original layout-aware widget.
use super::*;
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, RenderRootSignal, WindowSizePolicy},
    core::{
        DefaultProperties, TextEvent, Widget, WidgetId,
        keyboard::{Key, Modifiers, NamedKey},
    },
    dpi::PhysicalSize,
    properties::Dimensions,
    widgets::{TextInput, TextSelection},
};
use std::{cell::RefCell, rc::Rc, sync::Arc};

type Events = Rc<RefCell<Vec<TextInputEvent>>>;
struct Editor {
    ui: RenderRoot,
    widget: WidgetId,
    node: NodeId,
    events: Events,
}
impl Editor {
    fn new(vm: &SessionVm, node: NodeId) -> Self {
        let NodeKind::TextInput { value, options, .. } = &vm.stage().node(node).unwrap().kind
        else {
            unreachable!()
        };
        let input = TextInput::new(value.clone())
            .with_style(reel_ui::reel_text::StyleProperty::FontFamily(
                reel_ui::reel_text::FontFamily::from("Reel Weight Fixture").into_owned(),
            ))
            .with_selection(options.selection, options.selection_revision)
            .with_edit_filter(options.filter_edits)
            .with_editing(options.pending_edit, options.replay.clone())
            .prepare()
            .with_props(Dimensions::MAX);
        let widget = input.id();
        let events: Events = Default::default();
        let output = events.clone();
        let mut ui = RenderRoot::new(
            input,
            move |signal| {
                if let RenderRootSignal::Action(action, _) = signal
                    && let Some(event) = action.downcast_ref::<TextInputEvent>()
                {
                    output.borrow_mut().push(event.clone());
                }
            },
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(400, 50),
                scale_factor: 1.,
                default_font: Some(reel_ui::peniko::Blob::new(Arc::new(
                    include_bytes!("../../../../../reel-player/tests/fixtures/font-weight/400.ttf")
                        .to_vec(),
                ))),
            },
        );
        ui.redraw();
        Self {
            ui,
            widget,
            node,
            events,
        }
    }
    fn sync(&mut self, vm: &SessionVm) {
        let NodeKind::TextInput { value, options, .. } = &vm.stage().node(self.node).unwrap().kind
        else {
            unreachable!()
        };
        self.ui.edit_widget(self.widget, |mut widget| {
            let mut input = widget.downcast::<TextInput>();
            TextInput::set_value(&mut input, value.clone());
            TextInput::set_selection(&mut input, options.selection, options.selection_revision);
            TextInput::set_edit_filter(&mut input, options.filter_edits);
            TextInput::set_editing(&mut input, options.pending_edit, options.replay.as_deref());
        });
        self.ui.redraw();
    }
    fn send(&mut self, vm: &mut SessionVm, session: &mut Session) {
        let events = std::mem::take(&mut *self.events.borrow_mut());
        for event in events {
            assert!(
                vm.text_input(
                    session,
                    &NodeText {
                        node: self.node,
                        event
                    }
                )
                .unwrap()
            );
        }
    }
}
fn command(key: &str) -> TextEvent {
    let TextEvent::Keyboard(mut e) = TextEvent::key_down(Key::Character(key.into())) else {
        unreachable!()
    };
    e.modifiers = if cfg!(target_os = "macos") {
        Modifiers::META
    } else {
        Modifiers::CONTROL
    };
    TextEvent::Keyboard(e)
}
fn value(vm: &SessionVm, node: NodeId) -> &str {
    let NodeKind::TextInput { value, .. } = &vm.stage().node(node).unwrap().kind else {
        unreachable!()
    };
    value
}
fn save_restore(vm: &SessionVm, session: &Session) -> (Session, SessionVm) {
    let program = vm.vm.program().clone();
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut image =
        snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(79),
            epoch: Epoch(session.identity().epoch.0 + 1),
        },
        crate::ReadProgress::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    restore.finish().unwrap()
}
#[test]
fn accepted_typing_keeps_order_after_partial_undo_focus_loss_and_restore() {
    let prefix = "A".repeat(300);
    let code = format!(
        r#"fn opening() {{
        let data = model({{body: "{prefix}"}});
        let status = text("status");
        let editor = text_input(value: bind(data.body), on_change: || {{ status.x += 1; }});
        stage.replace(column {{ editor; status; }});
        narrator("wait");
    }}"#
    );
    for encoded in [false, true] {
        for restoring in [false, true] {
            let (mut session, mut vm) = models::prepared(&code, encoded);
            drive(&mut vm, &mut session);
            let node = vm
                .stage()
                .nodes()
                .find(|n| matches!(n.kind, NodeKind::TextInput { .. }))
                .unwrap()
                .id;
            let mut editor = Editor::new(&vm, node);
            editor.ui.focus_on(Some(editor.widget));
            editor
                .ui
                .handle_text_event(TextEvent::key_down(Key::Named(NamedKey::End)));
            editor.send(&mut vm, &mut session);
            editor
                .ui
                .handle_text_event(TextEvent::ClipboardPaste("tail".into()));
            editor.send(&mut vm, &mut session);
            drive(&mut vm, &mut session);
            editor.sync(&vm);
            assert_eq!(value(&vm, node), format!("{prefix}tail"));
            editor.ui.handle_text_event(command("z"));
            editor.send(&mut vm, &mut session);
            // Stop after the first actual undo native slice, with its builder rooted.
            for _ in 0..1000 {
                match vm
                    .step(
                        &mut session,
                        SessionBudget::RetryMinimum(SliceBudget {
                            instructions: 7,
                            ..Default::default()
                        }),
                        || 0,
                    )
                    .unwrap()
                {
                    SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                        vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(64))
                            .unwrap();
                        break;
                    }
                    SessionStep::Vm(RunOutcome::Fault(id)) => panic!("{id:?}"),
                    _ => {}
                }
            }
            assert_eq!(value(&vm, node), format!("{prefix}tail"));
            for event in [
                TextEvent::key_down(Key::Character("A".into())),
                TextEvent::key_down(Key::Character("B".into())),
                TextEvent::ClipboardPaste("xy".into()),
                TextEvent::key_down(Key::Named(NamedKey::Backspace)),
                TextEvent::key_down(Key::Named(NamedKey::ArrowLeft)),
                TextEvent::key_down(Key::Character("C".into())),
                command("a"),
                TextEvent::ClipboardPaste("final".into()),
                TextEvent::Ime(reel_ui::core::Ime::Commit("IME".into())),
                TextEvent::Ime(reel_ui::core::Ime::Preedit("uncommitted".into(), None)),
            ] {
                editor.ui.handle_text_event(event);
            }
            assert!(
                editor
                    .events
                    .borrow()
                    .iter()
                    .all(|e| matches!(e, TextInputEvent::Deferred { .. }))
            );
            editor.send(&mut vm, &mut session);
            editor.ui.focus_on(None); // Accepted text survives losing focus.
            if restoring {
                (session, vm) = save_restore(&vm, &session);
                editor = Editor::new(&vm, node);
            }
            let mut observed = Vec::new();
            for _ in 0..64 {
                editor.send(&mut vm, &mut session);
                drive(&mut vm, &mut session);
                editor.sync(&vm);
                let current = value(&vm, node).to_owned();
                if observed.last() != Some(&current) {
                    observed.push(current);
                }
                let NodeKind::TextInput { options, .. } = &vm.stage().node(node).unwrap().kind
                else {
                    unreachable!()
                };
                if !options.pending_edit && editor.events.borrow().is_empty() {
                    break;
                }
            }
            assert_eq!(
                value(&vm, node),
                "finalIME",
                "ASB={encoded} restore={restoring}: {observed:?}"
            );
            assert!(observed.contains(&format!("{prefix}AB")), "{observed:?}");
            assert!(observed.contains(&format!("{prefix}ABCx")), "{observed:?}");
            assert_eq!(text_x(&vm, "status"), 9.);
            assert!(editor.ui.focused_widget().is_none());
        }
    }
}

#[test]
fn editing_queue_rejects_before_mutation_and_node_retirement_releases_accepted_payloads() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"fn opening() {
            let editor = text_input();
            if editor.undo().redo() != editor { throw "no-op identity"; }
            if editor.can_undo || editor.can_redo || editor.selection_anchor != 0 || editor.selection_focus != 0 { throw "no-op state"; }
            stage.replace(column { editor; button("close", on_click: || stage.replace(stack())); });
            narrator("wait");
        }"#,
            encoded,
        );
        drive(&mut vm, &mut session);
        vm.set_text_input_editing_limits(reel_ui::widgets::TextInputEditingLimits {
            max_events: 2,
            max_text_bytes: 2,
        });
        let node = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::TextInput { .. }))
            .unwrap()
            .id;
        assert_eq!(
            vm.stage.text_history_entries(node),
            None,
            "unused input has no journal"
        );
        let host = vm.stage.value(vm.vm.heap(), node).unwrap();
        assert!(
            vm.vm
                .bind_host_method(host, "not_registered")
                .unwrap()
                .is_none()
        );
        assert!(vm.vm.bind_host_method(Value::int(1), "undo").is_err());
        let mut editor = Editor::new(&vm, node);
        editor.ui.focus_on(Some(editor.widget));
        editor
            .ui
            .handle_text_event(TextEvent::ClipboardPaste("original".into()));
        editor.send(&mut vm, &mut session);
        drive(&mut vm, &mut session);
        editor.sync(&vm);
        editor.ui.handle_text_event(command("z"));
        editor.send(&mut vm, &mut session);
        for key in ["A", "B"] {
            editor
                .ui
                .handle_text_event(TextEvent::key_down(Key::Character(key.into())));
        }
        editor.send(&mut vm, &mut session);
        let pool = vm.stage.text_queue_budget(node).unwrap();
        assert_eq!(pool.usage().tickets, 2);
        let payload: Arc<str> = "too large".into();
        editor
            .ui
            .handle_text_event(TextEvent::ClipboardPaste(payload.clone()));
        let event = editor.events.borrow_mut().pop().unwrap();
        let TextInputEvent::Deferred { intent, .. } = &event else {
            panic!()
        };
        let reel_ui::widgets::TextInputIntent::Paste(queued) = intent.as_ref() else {
            panic!()
        };
        assert!(
            Arc::ptr_eq(queued, &payload),
            "portable event, intent and replay share one payload"
        );
        assert!(matches!(
            vm.text_input(&mut session, &NodeText { node, event }),
            Err(AdapterError::InputQueue(_))
        ));
        assert_eq!(value(&vm, node), "original");
        assert_eq!(
            pool.usage().tickets,
            2,
            "rejection preserves accepted queue"
        );
        vm.click(&mut session, button(&vm, "close")).unwrap();
        drive(&mut vm, &mut session);
        assert_eq!(vm.stage().nodes().count(), 1);
        assert_eq!(vm.stage.text_history_entries(node), None);
        drop(editor);
        assert_eq!(pool.usage().tickets, 0);
        assert_eq!(pool.usage().bytes, 0);
    }
}

#[test]
fn replay_generation_survives_replaced_history_old_ack_and_same_widget_restore() {
    for encoded in [false, true] {
        for restoring in [false, true] {
            let (mut session, mut vm) = models::prepared(
                r#"fn opening(){
            let data=model({body:"original"});
            let editor=text_input(value:bind(data.body));
            stage.replace(column{editor;button("replace",on_click:||{data.body="replacement";});});
            narrator("wait");
        }"#,
                encoded,
            );
            drive(&mut vm, &mut session);
            let node = vm
                .stage()
                .nodes()
                .find(|n| matches!(n.kind, NodeKind::TextInput { .. }))
                .unwrap()
                .id;
            let mut editor = Editor::new(&vm, node);
            editor.ui.focus_on(Some(editor.widget));
            editor
                .ui
                .handle_text_event(TextEvent::key_down(Key::Named(NamedKey::End)));
            editor.send(&mut vm, &mut session);
            editor
                .ui
                .handle_text_event(TextEvent::ClipboardPaste("!".into()));
            editor.send(&mut vm, &mut session);
            drive(&mut vm, &mut session);
            editor.sync(&vm);
            editor.ui.handle_text_event(command("z"));
            editor.send(&mut vm, &mut session);
            editor
                .ui
                .handle_text_event(TextEvent::key_down(Key::Character("A".into())));
            editor.send(&mut vm, &mut session);
            drive(&mut vm, &mut session);
            editor.sync(&vm);
            let events = std::mem::take(&mut *editor.events.borrow_mut());
            let old_id = events
                .iter()
                .find_map(|e| {
                    if let TextInputEvent::ReplayCompleted(id) = e {
                        Some(*id)
                    } else {
                        None
                    }
                })
                .unwrap();
            for event in events
                .into_iter()
                .filter(|e| !matches!(e, TextInputEvent::ReplayCompleted(_)))
            {
                assert!(
                    vm.text_input(&mut session, &NodeText { node, event })
                        .unwrap()
                );
            }
            drive(&mut vm, &mut session);
            editor.sync(&vm);
            assert_eq!(value(&vm, node), "originalA");
            vm.click(&mut session, button(&vm, "replace")).unwrap();
            drive(&mut vm, &mut session);
            editor.sync(&vm);
            assert_eq!(vm.stage.text_history_entries(node), None);
            editor
                .ui
                .handle_text_event(TextEvent::key_down(Key::Named(NamedKey::End)));
            editor.send(&mut vm, &mut session);
            editor
                .ui
                .handle_text_event(TextEvent::ClipboardPaste("!".into()));
            editor.send(&mut vm, &mut session);
            drive(&mut vm, &mut session);
            editor.sync(&vm);
            editor.ui.handle_text_event(command("z"));
            editor.send(&mut vm, &mut session);
            editor
                .ui
                .handle_text_event(TextEvent::key_down(Key::Character("B".into())));
            editor.send(&mut vm, &mut session);
            drive(&mut vm, &mut session);
            let replay_id = |vm: &SessionVm| {
                let NodeKind::TextInput { options, .. } = &vm.stage().node(node).unwrap().kind
                else {
                    unreachable!()
                };
                options.replay.as_ref().unwrap().id
            };
            let new_id = replay_id(&vm);
            assert_eq!(old_id.sequence, 1);
            assert_eq!(new_id.sequence, 1);
            assert_ne!(
                old_id.generation, new_id.generation,
                "new queue keeps a unique generation"
            );
            assert!(
                vm.text_input(
                    &mut session,
                    &NodeText {
                        node,
                        event: TextInputEvent::ReplayCompleted(old_id)
                    }
                )
                .unwrap()
            );
            assert_eq!(
                replay_id(&vm),
                new_id,
                "late acknowledgement cannot consume a new queue head"
            );
            if restoring {
                (session, vm) = save_restore(&vm, &session);
            }
            assert_eq!(
                replay_id(&vm),
                new_id,
                "snapshot preserves the complete replay identity"
            );
            editor.sync(&vm);
            editor.send(&mut vm, &mut session);
            drive(&mut vm, &mut session);
            editor.sync(&vm);
            assert_eq!(
                value(&vm, node),
                "replacementB",
                "ASB={encoded} restore={restoring}"
            );
            let NodeKind::TextInput { options, .. } = &vm.stage().node(node).unwrap().kind else {
                unreachable!()
            };
            assert!(!options.pending_edit);
        }
    }
}

#[test]
fn direct_concurrent_undo_rejects_without_spinning_and_the_accepted_queue_still_finishes() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"fn opening(){
            let status=text("status");
            let editor=text_input(value:"original");
            stage.replace(column{editor;status;
                button("concurrent",on_click:||{
                    let before=editor.value;
                    let mut rejected=0;
                    try{editor.undo();}catch _{rejected+=1;}
                    try{editor.redo();}catch _{rejected+=1;}
                    if rejected!=2||editor.value!=before{throw "concurrent edit changed state";}
                    status.x=rejected;
                });
            });narrator("wait");
        }"#,
            encoded,
        );
        drive(&mut vm, &mut session);
        let node = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::TextInput { .. }))
            .unwrap()
            .id;
        let mut editor = Editor::new(&vm, node);
        editor.ui.focus_on(Some(editor.widget));
        editor
            .ui
            .handle_text_event(TextEvent::key_down(Key::Named(NamedKey::End)));
        editor.send(&mut vm, &mut session);
        editor
            .ui
            .handle_text_event(TextEvent::ClipboardPaste("!".into()));
        editor.send(&mut vm, &mut session);
        drive(&mut vm, &mut session);
        editor.sync(&vm);
        editor.ui.handle_text_event(command("z"));
        editor.send(&mut vm, &mut session);
        editor
            .ui
            .handle_text_event(TextEvent::key_down(Key::Character("A".into())));
        editor.send(&mut vm, &mut session);
        drive(&mut vm, &mut session);
        assert_eq!(value(&vm, node), "original");
        vm.click(&mut session, button(&vm, "concurrent")).unwrap();
        drive(&mut vm, &mut session);
        assert_eq!(text_x(&vm, "status"), 2.);
        assert_eq!(value(&vm, node), "original");
        editor.sync(&vm);
        editor.send(&mut vm, &mut session);
        drive(&mut vm, &mut session);
        editor.sync(&vm);
        assert_eq!(value(&vm, node), "originalA");
    }
}

#[test]
fn accepted_explicit_selection_maps_once_and_late_old_geometry_is_rejected() {
    use reel_ui::widgets::{TextInputIntent, TextSelection};
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"fn opening(){stage.replace(text_input(value:"abcdef"));narrator("wait");}"#,
            encoded,
        );
        drive(&mut vm, &mut session);
        let node = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::TextInput { .. }))
            .unwrap()
            .id;
        let mut editor = Editor::new(&vm, node);
        editor.ui.focus_on(Some(editor.widget));
        editor
            .ui
            .handle_text_event(TextEvent::key_down(Key::Named(NamedKey::End)));
        editor.send(&mut vm, &mut session);
        editor
            .ui
            .handle_text_event(TextEvent::ClipboardPaste("XYZ".into()));
        editor.send(&mut vm, &mut session);
        drive(&mut vm, &mut session);
        editor.sync(&vm);
        editor.ui.handle_text_event(command("z"));
        editor.send(&mut vm, &mut session);
        let select = |anchor, focus, revision| NodeText {
            node,
            event: TextInputEvent::Deferred {
                intent: Arc::new(TextInputIntent::Selection {
                    selection: TextSelection { anchor, focus },
                }),
                revision,
            },
        };
        assert!(vm.text_input(&mut session, &select(8, 9, 0)).unwrap());
        drive(&mut vm, &mut session);
        editor.sync(&vm);
        editor.send(&mut vm, &mut session);
        drive(&mut vm, &mut session);
        editor.sync(&vm);
        let NodeKind::TextInput { options, .. } = &vm.stage().node(node).unwrap().kind else {
            unreachable!()
        };
        assert_eq!(
            options.selection,
            TextSelection {
                anchor: 6,
                focus: 6
            }
        );
        assert_eq!(options.selection_revision, 1);
        assert!(
            !vm.text_input(&mut session, &select(1, 2, 0)).unwrap(),
            "old displayed byte coordinates are rejected before acceptance"
        );
        assert!(
            vm.text_input(
                &mut session,
                &NodeText {
                    node,
                    event: TextInputEvent::Deferred {
                        intent: Arc::new(TextInputIntent::Character {
                            text: "A".into(),
                            modifiers: 0,
                            repeat: false
                        }),
                        revision: 0
                    }
                }
            )
            .unwrap(),
            "relative accepted input still resolves after preceding edits"
        );
        editor.sync(&vm);
        editor.send(&mut vm, &mut session);
        drive(&mut vm, &mut session);
        editor.sync(&vm);
        assert_eq!(value(&vm, node), "abcdefA");
        assert!(
            vm.text_input(&mut session, &select(0, 2, 1)).unwrap(),
            "current explicit byte selection is accepted"
        );
        editor.sync(&vm);
        editor.send(&mut vm, &mut session);
        drive(&mut vm, &mut session);
        editor.sync(&vm);
        let NodeKind::TextInput { options, .. } = &vm.stage().node(node).unwrap().kind else {
            unreachable!()
        };
        assert_eq!(
            options.selection,
            TextSelection {
                anchor: 0,
                focus: 2
            }
        );
    }
}

#[test]
fn snapshot_after_script_value_write_discards_the_invalidated_journal_before_projection() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"fn opening(){
            let editor=text_input(value:"initial");
            stage.replace(column{editor;button("replace",on_click:||{editor.value="é"; if !editor.can_redo {throw "readonly outputs advanced before maintenance";} editor.undo().redo(); if editor.can_undo || editor.can_redo {throw "native edit did not see invalidation";} });});
            narrator("wait");
        }"#,
            encoded,
        );
        drive(&mut vm, &mut session);
        let node = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::TextInput { .. }))
            .unwrap()
            .id;
        let mut editor = Editor::new(&vm, node);
        editor.ui.focus_on(Some(editor.widget));
        editor
            .ui
            .handle_text_event(TextEvent::key_down(Key::Named(NamedKey::ArrowRight)));
        editor.send(&mut vm, &mut session);
        editor
            .ui
            .handle_text_event(TextEvent::ClipboardPaste("!".into()));
        editor.send(&mut vm, &mut session);
        drive(&mut vm, &mut session);
        editor.sync(&vm);
        editor.ui.handle_text_event(command("z"));
        editor.send(&mut vm, &mut session);
        editor
            .ui
            .handle_text_event(TextEvent::key_down(Key::Character("A".into())));
        editor.send(&mut vm, &mut session);
        drive(&mut vm, &mut session);
        let old_id = {
            let NodeKind::TextInput { options, .. } = &vm.stage().node(node).unwrap().kind else {
                unreachable!()
            };
            options.replay.as_ref().unwrap().id
        };
        let host = vm.stage.value(vm.vm.heap(), node).unwrap();
        vm.click(&mut session, button(&vm, "replace")).unwrap();
        let mut found = false;
        for _ in 0..1000 {
            vm.step(
                &mut session,
                SessionBudget::RetryMinimum(SliceBudget {
                    instructions: 1,
                    ..Default::default()
                }),
                || 0,
            )
            .unwrap();
            let current = vm
                .vm
                .heap()
                .host_property(aside_vm::PropertyId { host, field: 0 })
                .unwrap();
            if text::read_string(vm.vm.heap(), current, 4096).unwrap() == "é" {
                found = true;
                break;
            }
        }
        assert!(found);
        assert_ne!(
            value(&vm, node),
            "é",
            "capture the real script budget boundary before stage maintenance"
        );
        let (mut restored_session, mut restored_vm) = save_restore(&vm, &session);
        // Save once more after native reset publication, before the callback continues.
        // Restoring nodes already projects authoritative values; it must not publish
        // a second selection revision or advance the interrupted script's outputs.
        let restored_host = restored_vm
            .stage
            .value(restored_vm.vm.heap(), node)
            .unwrap();
        let fields = restored_vm.vm.heap().host_fields(restored_host).unwrap();
        let redo_field = (0..restored_vm.vm.heap().table_len(fields).unwrap())
            .find(|&index| {
                let (key, _) = restored_vm.vm.heap().table_entry(fields, index).unwrap();
                text::read_string(restored_vm.vm.heap(), key, 4096).unwrap() == "can_redo"
            })
            .unwrap();
        let mut reset_published = false;
        for _ in 0..1000 {
            let step = restored_vm
                .step(
                    &mut restored_session,
                    SessionBudget::RetryMinimum(SliceBudget {
                        instructions: 1,
                        ..Default::default()
                    }),
                    || 0,
                )
                .unwrap();
            if let SessionStep::Vm(RunOutcome::HostCall(ticket)) = step {
                restored_vm
                    .dispatch(
                        &mut restored_session,
                        ticket,
                        "en",
                        NativeBudget::RetryMinimum(1),
                    )
                    .unwrap();
            }
            if restored_vm
                .vm
                .heap()
                .host_property(aside_vm::PropertyId {
                    host: restored_host,
                    field: redo_field,
                })
                .unwrap()
                .as_bool()
                == Some(false)
            {
                reset_published = true;
                break;
            }
        }
        assert!(reset_published);
        let (mut published_session, mut published_vm) =
            save_restore(&restored_vm, &restored_session);
        drive(&mut vm, &mut session);
        drive(&mut restored_vm, &mut restored_session);
        drive(&mut published_vm, &mut published_session);
        for candidate in [&restored_vm, &published_vm] {
            assert_eq!(
                vm.stage().node(node).unwrap().kind,
                candidate.stage().node(node).unwrap().kind,
                "restore equals direct continuation"
            );
        }
        for (session, vm) in [
            (&mut session, &mut vm),
            (&mut restored_session, &mut restored_vm),
            (&mut published_session, &mut published_vm),
        ] {
            assert_eq!(value(vm, node), "é");
            assert_eq!(vm.stage.text_history_entries(node), None);
            let NodeKind::TextInput { options, .. } = &vm.stage().node(node).unwrap().kind else {
                unreachable!()
            };
            assert_eq!(
                options.selection,
                reel_ui::widgets::TextSelection::default(),
                "invalid UTF-8 midpoint normalizes to the preceding boundary"
            );
            let host = vm.stage.value(vm.vm.heap(), node).unwrap();
            let fields = vm.vm.heap().host_fields(host).unwrap();
            for name in ["can_undo", "can_redo"] {
                let field = (0..vm.vm.heap().table_len(fields).unwrap())
                    .find(|&index| {
                        let (key, _) = vm.vm.heap().table_entry(fields, index).unwrap();
                        text::read_string(vm.vm.heap(), key, 4096).unwrap() == name
                    })
                    .unwrap();
                assert_eq!(
                    vm.vm
                        .heap()
                        .host_property(aside_vm::PropertyId { host, field })
                        .unwrap()
                        .as_bool(),
                    Some(false)
                );
            }
            let mut editor = Editor::new(vm, node);
            editor.ui.focus_on(Some(editor.widget));
            editor
                .ui
                .handle_text_event(TextEvent::ClipboardPaste("B".into()));
            editor.send(vm, session);
            drive(vm, session);
            editor.sync(vm);
            editor.ui.handle_text_event(command("z"));
            editor.send(vm, session);
            editor
                .ui
                .handle_text_event(TextEvent::key_down(Key::Character("C".into())));
            editor.send(vm, session);
            drive(vm, session);
            let new_id = {
                let NodeKind::TextInput { options, .. } = &vm.stage().node(node).unwrap().kind
                else {
                    unreachable!()
                };
                options.replay.as_ref().unwrap().id
            };
            assert_eq!(new_id.sequence, old_id.sequence);
            assert_ne!(new_id.generation, old_id.generation);
            assert!(
                vm.text_input(
                    session,
                    &NodeText {
                        node,
                        event: TextInputEvent::ReplayCompleted(old_id)
                    }
                )
                .unwrap()
            );
            editor.sync(vm);
            editor.send(vm, session);
            drive(vm, session);
            editor.sync(vm);
            assert_eq!(
                value(vm, node),
                "Cé",
                "invalidated old input is gone; new input survives an old acknowledgement"
            );
        }
    }
}

#[test]
fn external_value_write_then_native_noop_resets_once_without_a_prior_journal() {
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(
            r#"fn opening() {
            let editor=text_input(value:"before");
            stage.replace(editor);
            editor.value="after";
            if editor.undo().redo() != editor {throw "identity";}
            if editor.can_undo || editor.can_redo {throw "no history";}
            narrator("wait");
        }"#,
            encoded,
        );
        drive(&mut vm, &mut session);
        let node = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::TextInput { .. }))
            .unwrap();
        assert_eq!(value(&vm, node.id), "after");
        assert_eq!(vm.stage.text_history_entries(node.id), None);
        let NodeKind::TextInput { options, .. } = &node.kind else {
            unreachable!()
        };
        assert_eq!(
            options.selection_revision, 1,
            "reset published exactly once"
        );
    }
}

impl Editor {
    fn settle(&mut self, vm: &mut SessionVm, session: &mut Session) {
        for _ in 0..32 {
            self.send(vm, session);
            drive(vm, session);
            self.sync(vm);
            let NodeKind::TextInput { options, .. } = &vm.stage().node(self.node).unwrap().kind
            else {
                unreachable!()
            };
            if !options.pending_edit && self.events.borrow().is_empty() {
                return;
            }
        }
        panic!("filtered edit queue did not settle");
    }
}

#[test]
fn edit_filter_uses_real_typing_paste_and_ime_with_one_history_publication() {
    let code = r#"use reel::types::TextEdit;
    fn opening() {
        let data=model({body:""});
        let status=text("status");
        let editor=text_input(value:bind(data.body), edit_filter:|e:TextEdit| {
            status.y+=1;
            if e.value != e.previous.slice_bytes(0..e.start)+e.text+e.previous.slice_bytes(e.end..e.previous.byte_len()) {throw "proposal";}
            if e.text=="reject" {return null;}
            if e.text=="xy" {return "XY";}
            if e.text=="中" {return "語";}
            return e.text;
        },on_change:|e| {if data.body!=e.value {throw "publication";} status.x+=1;});
        stage.replace(column{editor;status;
            button("author",on_click:||{editor.value="author";});
        });
        narrator("wait");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        drive(&mut vm, &mut session);
        let node = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::TextInput { .. }))
            .unwrap()
            .id;
        let mut editor = Editor::new(&vm, node);
        editor.ui.focus_on(Some(editor.widget));
        // A second character in the same presentation already uses Deferred.
        editor
            .ui
            .handle_text_event(TextEvent::key_down(Key::Character("a".into())));
        editor
            .ui
            .handle_text_event(TextEvent::key_down(Key::Character("b".into())));
        assert!(matches!(
            editor.events.borrow()[1],
            TextInputEvent::Deferred { .. }
        ));
        editor.settle(&mut vm, &mut session);
        assert_eq!(value(&vm, node), "ab");
        editor
            .ui
            .handle_text_event(TextEvent::ClipboardPaste("xy".into()));
        editor.settle(&mut vm, &mut session);
        assert_eq!(value(&vm, node), "abXY");
        let NodeKind::TextInput { options, .. } = &vm.stage().node(node).unwrap().kind else {
            unreachable!()
        };
        assert_eq!(
            options.selection,
            TextSelection {
                anchor: 4,
                focus: 4
            }
        );
        editor
            .ui
            .handle_text_event(TextEvent::ClipboardPaste("reject".into()));
        editor.settle(&mut vm, &mut session);
        assert_eq!(value(&vm, node), "abXY");
        assert_eq!(text_x(&vm, "status"), 3.);
        editor
            .ui
            .handle_text_event(TextEvent::Ime(reel_ui::core::Ime::Preedit(
                "preedit".into(),
                None,
            )));
        editor.send(&mut vm, &mut session);
        assert_eq!(node_y(&vm, "status"), 4.);
        editor
            .ui
            .handle_text_event(TextEvent::Ime(reel_ui::core::Ime::Commit("中".into())));
        editor.settle(&mut vm, &mut session);
        assert_eq!(value(&vm, node), "abXY語");
        assert_eq!(text_x(&vm, "status"), 4.);
        assert_eq!(node_y(&vm, "status"), 5.);
        editor.ui.handle_text_event(command("z"));
        editor.settle(&mut vm, &mut session);
        assert_eq!(value(&vm, node), "abXY");
        editor.ui.handle_text_event(command("y"));
        editor.settle(&mut vm, &mut session);
        assert_eq!(value(&vm, node), "abXY語");
        assert_eq!(node_y(&vm, "status"), 5., "undo/redo bypass filtering");
        vm.click(&mut session, button(&vm, "author")).unwrap();
        drive(&mut vm, &mut session);
        assert_eq!(value(&vm, node), "author");
        assert_eq!(
            node_y(&vm, "status"),
            5.,
            "script assignment bypasses filtering"
        );
        let NodeKind::TextInput { options, .. } = &vm.stage().node(node).unwrap().kind else {
            unreachable!()
        };
        let revision = options.selection_revision;
        assert!(
            vm.text_input(
                &mut session,
                &NodeText {
                    node,
                    event: TextInputEvent::Changed {
                        previous: "author".into(),
                        value: "Qauthor".into(),
                        edit: reel_ui::widgets::TextEdit {
                            range: 0..0,
                            before: TextSelection {
                                anchor: 0,
                                focus: 0
                            },
                            after: TextSelection {
                                anchor: 1,
                                focus: 0
                            },
                            kind: reel_ui::widgets::TextEditKind::Transaction,
                            revision,
                            replay: None,
                        }
                    }
                }
            )
            .unwrap()
        );
        drive(&mut vm, &mut session);
        let NodeKind::TextInput { options, .. } = &vm.stage().node(node).unwrap().kind else {
            unreachable!()
        };
        assert_eq!(
            options.selection,
            TextSelection {
                anchor: 1,
                focus: 0
            },
            "unchanged replacement preserves the proposal selection direction"
        );
    }
}

#[test]
fn edit_filter_pending_callback_restores_queue_and_retires_without_committing() {
    let code = r#"fn opening(){
        let status=text("status");
        let editor=text_input(value:"old",edit_filter:|e:TextEdit|{
            status.y+=1;
            let mut i=0;while i<300 {i+=1;}
            if e.text.byte_len()>32 {return e.text+"!";}
            return e.text;
        },on_change:||{status.x+=1;});
        stage.replace(column{editor;status;});
        narrator("wait");
    }"#;
    for encoded in [false, true] {
        for cancel in [false, true] {
            let (mut session, mut vm) = models::prepared(code, encoded);
            drive(&mut vm, &mut session);
            let node = vm
                .stage()
                .nodes()
                .find(|n| matches!(n.kind, NodeKind::TextInput { .. }))
                .unwrap()
                .id;
            let mut editor = Editor::new(&vm, node);
            editor.ui.focus_on(Some(editor.widget));
            editor
                .ui
                .handle_text_event(TextEvent::key_down(Key::Named(NamedKey::End)));
            editor.send(&mut vm, &mut session);
            editor
                .ui
                .handle_text_event(TextEvent::ClipboardPaste("B".repeat(256).into()));
            editor
                .ui
                .handle_text_event(TextEvent::key_down(Key::Character("q".into())));
            editor.send(&mut vm, &mut session);
            for _ in 0..3000 {
                match vm
                    .step(
                        &mut session,
                        SessionBudget::RetryMinimum(SliceBudget {
                            instructions: 7,
                            ..Default::default()
                        }),
                        || 0,
                    )
                    .unwrap()
                {
                    SessionStep::Vm(RunOutcome::HostCall(t)) => {
                        vm.dispatch(&mut session, t, "en", NativeBudget::RetryMinimum(64))
                            .unwrap();
                    }
                    SessionStep::Vm(RunOutcome::Fault(id)) => panic!("{id:?}"),
                    _ => {}
                }
                let status = vm
                    .stage()
                    .nodes()
                    .find(|n| matches!(&n.kind, NodeKind::Text(text) if text.as_ref()=="status"))
                    .unwrap()
                    .id;
                let status = vm.stage.value(vm.vm.heap(), status).unwrap();
                let started = vm
                    .vm
                    .heap()
                    .host_property(aside_vm::PropertyId {
                        host: status,
                        field: 2 + stage::NodeProperty::Y.index() as u32,
                    })
                    .unwrap();
                if started.as_float() == Some(1.) || started.as_int() == Some(1) {
                    break;
                }
            }
            let target = vm.stage.value(vm.vm.heap(), node).unwrap();
            let committed = vm
                .vm
                .heap()
                .host_property(aside_vm::PropertyId {
                    host: target,
                    field: 0,
                })
                .unwrap();
            assert_eq!(
                text::read_string(vm.vm.heap(), committed, 1024).unwrap(),
                "old",
                "filter has not published midway"
            );
            (session, vm) = save_restore(&vm, &session);
            if cancel {
                let host = vm.stage.value(vm.vm.heap(), node).unwrap();
                vm.stage.remove_node(vm.vm.heap_mut(), host).unwrap();
                drive(&mut vm, &mut session);
                assert!(vm.stage().node(node).is_none());
                assert_eq!(
                    text_x(&vm, "status"),
                    0.,
                    "retirement cancels before publication"
                );
                assert_eq!(vm.stage.text_history_entries(node), None);
                assert!(vm.tasks.is_empty());
            } else {
                editor = Editor::new(&vm, node);
                editor.settle(&mut vm, &mut session);
                assert_eq!(value(&vm, node), format!("old{}!q", "B".repeat(256)));
                assert_eq!(text_x(&vm, "status"), 2.);
                assert_eq!(node_y(&vm, "status"), 2.);
            }
        }
    }
}

#[test]
fn edit_filter_fault_and_wait_rejection_preserve_value_and_history() {
    for body in [
        "throw \"filter failed\";",
        "await delay(0.1);return e.text;",
    ] {
        let code = format!(
            r#"fn opening(){{let editor=text_input(value:"old",edit_filter:|e:TextEdit|{{{body}}});stage.replace(editor);narrator("wait");}}"#
        );
        let (mut session, mut vm) = models::prepared(&code, false);
        drive(&mut vm, &mut session);
        let node = vm
            .stage()
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::TextInput { .. }))
            .unwrap()
            .id;
        assert!(
            vm.text_input(
                &mut session,
                &NodeText {
                    node,
                    event: TextInputEvent::replace_all("old", "new", 0)
                }
            )
            .unwrap()
        );
        let mut faulted = false;
        for _ in 0..3000 {
            match vm
                .step(
                    &mut session,
                    SessionBudget::RetryMinimum(SliceBudget {
                        instructions: 7,
                        ..Default::default()
                    }),
                    || 0,
                )
                .unwrap()
            {
                SessionStep::Vm(RunOutcome::HostCall(t)) => {
                    vm.dispatch(&mut session, t, "en", NativeBudget::RetryMinimum(64))
                        .unwrap();
                }
                SessionStep::Vm(RunOutcome::Fault(_)) => {
                    faulted = true;
                    vm.discard_delivery(&mut session).unwrap();
                    break;
                }
                _ => {}
            }
        }
        assert!(faulted);
        drive(&mut vm, &mut session);
        assert_eq!(value(&vm, node), "old");
        assert_eq!(vm.stage.text_history_entries(node), Some(0));
        let NodeKind::TextInput { options, .. } = &vm.stage().node(node).unwrap().kind else {
            unreachable!()
        };
        assert!(!options.pending_edit);
    }
}
