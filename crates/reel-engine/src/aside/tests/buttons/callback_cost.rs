use super::*;
use reel_ui::{
    core::keyboard::{Code, Key, KeyboardEvent},
    input::ActionPhase,
    kurbo::{Point, Vec2},
    stage::{
        ControlValue, InteractionState, NodeChange, NodeEvent, NodeInteraction, NodeKey, NodeLink,
        NodePointer, NodeText, NodeWheel, PointerPhase,
    },
    widgets::{SliderRange, TextInputEvent},
};

#[derive(Clone, Copy, Debug)]
enum Input {
    Click,
    Wheel,
    Pointer,
    Key,
    Interaction,
    Slider,
    Toggle,
    TextEdit,
    TextSubmit,
    Link,
}
const INPUTS: [Input; 10] = [
    Input::Click,
    Input::Wheel,
    Input::Pointer,
    Input::Key,
    Input::Interaction,
    Input::Slider,
    Input::Toggle,
    Input::TextEdit,
    Input::TextSubmit,
    Input::Link,
];

fn code(input: Input, event: bool, waiting: bool) -> String {
    let check = if !event {
        ""
    } else {
        match input {
            Input::Click => "if event.target.label != \"click\" { throw \"click target\"; }",
            Input::Wheel => {
                "if event.x != 12 || event.y != 8 || event.delta_x != 2 || event.delta_y != 40 { throw \"wheel fields\"; }"
            }
            Input::Pointer => {
                "if event.x != 12 || event.y != 8 || event.phase != \"moved\" { throw \"pointer fields\"; }"
            }
            Input::Key => {
                "if event.code != \"KeyA\" || event.key != \"a\" || event.phase != \"pressed\" || event.shift || event.ctrl || event.alt || event.meta { throw \"key fields\"; }"
            }
            Input::Interaction => {
                "if !event.hover || event.pressed || event.focused || event.disabled || event.focus_visible || !event.target.hover { throw \"interaction fields\"; }"
            }
            Input::Slider => {
                "if event.value != 0.5 || event.target.value != event.value { throw \"slider fields\"; }"
            }
            Input::Toggle => {
                "if event.value != true || event.target.value != event.value { throw \"toggle fields\"; }"
            }
            Input::TextEdit => {
                "if event.value != \"updated\" || event.target.value != event.value { throw \"text edit fields\"; }"
            }
            Input::TextSubmit => {
                "if event.value != \"initial\" || event.target.value != event.value { throw \"text submit fields\"; }"
            }
            Input::Link => "if event.id != \"test\" { throw \"link fields\"; }",
        }
    };
    format!(
        r#"fn opening() {{
        let output = text("calls");
        fn handler({parameter}) {{
            {check}
            output.x += 1;
            {wait}
        }}
        let controls = column {{
            button("click", on_click: handler, on_interaction: handler);
            input(on_pointer: handler, on_wheel: handler, on_key: handler);
            slider(value: 0, min: 0, max: 1, on_change: handler);
            toggle(on_change: handler);
            text_input(value: "initial", on_change: handler, on_submit: handler);
            text(rich("[link=test]link[/link]"), on_link: handler);
        }};
        stage.replace(column {{ output; controls;
            button("retire", on_click: || controls.remove());
        }});
        narrator("callback fixture");
    }}"#,
        parameter = if event { "event" } else { "" },
        wait = if waiting {
            "await run(delay(1)); output.y += 1;"
        } else {
            ""
        }
    )
}

fn dispatch(vm: &mut SessionVm, session: &mut Session, input: Input) {
    let node = match input {
        Input::Click | Input::Interaction => button(vm, "click"),
        _ => {
            vm.stage()
                .nodes()
                .find(|n| match input {
                    Input::Wheel | Input::Pointer | Input::Key => {
                        matches!(n.kind, NodeKind::Input { .. })
                    }
                    Input::Slider => matches!(n.kind, NodeKind::Slider { .. }),
                    Input::Toggle => matches!(n.kind, NodeKind::Toggle { .. }),
                    Input::TextEdit | Input::TextSubmit => {
                        matches!(n.kind, NodeKind::TextInput { .. })
                    }
                    Input::Link => {
                        matches!(&n.kind, NodeKind::Text(text) if !text.links().is_empty())
                    }
                    _ => unreachable!(),
                })
                .unwrap()
                .id
        }
    };
    let accepted = match input {
        Input::Click => vm.click(session, node),
        Input::Wheel => vm.input_wheel(
            session,
            &NodeWheel {
                node,
                position: Point::new(12., 8.),
                delta: Vec2::new(2., 40.),
            },
        ),
        Input::Pointer => vm.input_pointer(
            session,
            &NodePointer {
                node,
                position: Point::new(12., 8.),
                phase: PointerPhase::Moved,
            },
        ),
        Input::Key => vm.input_key(
            session,
            &NodeKey {
                node,
                phase: ActionPhase::Pressed,
                event: KeyboardEvent {
                    code: Code::KeyA,
                    key: Key::Character("a".into()),
                    ..Default::default()
                },
            },
        ),
        Input::Interaction => vm.interactions(
            session,
            &[NodeEvent::Interaction(NodeInteraction {
                node,
                state: InteractionState {
                    hover: true,
                    ..Default::default()
                },
            })],
        ),
        Input::Slider | Input::Toggle => vm.change(
            session,
            &NodeChange {
                node,
                value: if matches!(input, Input::Slider) {
                    ControlValue::Number(0.5)
                } else {
                    ControlValue::Bool(true)
                },
                range: matches!(input, Input::Slider)
                    .then(|| SliderRange::new(0., 1., None).unwrap()),
            },
        ),
        Input::TextEdit | Input::TextSubmit => vm.text_input(
            session,
            &NodeText {
                node,
                event: if matches!(input, Input::TextEdit) {
                    TextInputEvent::replace_all("initial", "updated", 0)
                } else {
                    TextInputEvent::Submitted {
                        value: "initial".into(),
                        replay: None,
                    }
                },
            },
        ),
        Input::Link => {
            let NodeKind::Text(text) = &vm.stage().node(node).unwrap().kind else {
                unreachable!()
            };
            let link = NodeLink {
                node,
                text: text.clone(),
                id: "test".into(),
            };
            vm.text_link(session, &link)
        }
    }
    .unwrap();
    assert!(accepted, "{input:?}");
}

#[test]
fn zero_argument_callback_payloads_are_not_allocated() {
    for encoded in [false, true] {
        for input in INPUTS {
            let mut counts = [0; 2];
            let mut bytes = [0; 2];
            for event in [false, true] {
                let (mut session, mut vm) = models::prepared(&code(input, event, false), encoded);
                pump(&mut vm, &mut session);
                collect(&mut vm);
                let before = vm.vm.heap().stats();
                dispatch(&mut vm, &mut session, input);
                let after = vm.vm.heap().stats();
                counts[usize::from(event)] = after.live_objects - before.live_objects;
                bytes[usize::from(event)] = after.charged_bytes - before.charged_bytes;
                collect(&mut vm);
                pump(&mut vm, &mut session);
                assert_eq!(text_x(&vm, "calls"), 1., "{input:?}");
            }
            eprintln!(
                "{input:?} ASB={encoded}: zero-argument={} objects/{} bytes, event={} objects/{} bytes",
                counts[0], bytes[0], counts[1], bytes[1]
            );
            // Editing text still allocates the committed value. Other inputs use
            // immediate values and can schedule their callback without VM payloads.
            assert_eq!(
                counts[0],
                if matches!(input, Input::TextEdit) {
                    1
                } else {
                    0
                },
                "{input:?}"
            );
            assert!(counts[1] > counts[0], "{input:?}");
        }
    }
}

#[test]
fn lazy_callback_payloads_keep_gc_roots_and_node_cancellation() {
    for encoded in [false, true] {
        for input in INPUTS {
            for event in [false, true] {
                let (mut session, mut vm) = models::prepared(&code(input, event, true), encoded);
                pump(&mut vm, &mut session);
                dispatch(&mut vm, &mut session, input);
                collect(&mut vm);
                pump(&mut vm, &mut session);
                assert_eq!(text_x(&vm, "calls"), 1.);
                let retire = button(&vm, "retire");
                vm.click(&mut session, retire).unwrap();
                pump(&mut vm, &mut session);
                collect(&mut vm);
                super::super::activities::advance_clock(&mut vm, &session, 2., 1);
                pump(&mut vm, &mut session);
                let output = vm
                    .stage()
                    .nodes()
                    .find(|n| n.kind == NodeKind::Text("calls".into()))
                    .unwrap();
                assert_eq!(output.props.y, 0., "{input:?} event={event} ASB={encoded}");
                assert!(vm.tasks.is_empty());
            }
        }
    }
}
