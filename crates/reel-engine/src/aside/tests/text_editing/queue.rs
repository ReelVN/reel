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
    widgets::TextInput,
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
fn save_restore(vm: SessionVm, session: Session) -> (Session, SessionVm) {
    let program = vm.vm.program().clone();
    let mut capture = vm.snapshot_begin(&session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut image =
        snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(79),
            epoch: Epoch(22),
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
                (session, vm) = save_restore(vm, session);
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
                "final",
                "ASB={encoded} restore={restoring}: {observed:?}"
            );
            assert!(observed.contains(&format!("{prefix}AB")), "{observed:?}");
            assert!(observed.contains(&format!("{prefix}ABCx")), "{observed:?}");
            assert_eq!(text_x(&vm, "status"), 8.);
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
