//! Explicit author focus follows current accepted stage geometry without a native window.
use super::*;
use reel_engine::aside::{
    source,
    window::{WindowCommand, WindowReply},
};
use reel_ui::input::{ActionPhase, FocusVisibility, NavigationAction, NavigationEvent};

fn compiled_runtime(code: &str, encoded: bool) -> StoryRuntime {
    let mut compiled = source::compile_story(code, "focus_player", 1, "main").unwrap();
    if encoded {
        use aside_vm::bytecode as bc;
        let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
        compiled.program = bc::verify(
            bc::decode(&bytes, bc::Limits::default()).unwrap(),
            bc::Limits::default(),
        )
        .unwrap();
    }
    let mut story = StoryRuntime::new(
        AsideSessionUi::from_compiled(session(), compiled, "en", VmLimits::default()).unwrap(),
    );
    story.set_window_control_enabled(true);
    drain(&mut story);
    story
}

#[test]
fn pointer_buttons_skip_navigation_and_update_focusability_source_asb() {
    let code = r#"fn main() {
        let state = model({navigable: false, clicks: 0});
        let first = button("First", width: 100, height: 40);
        let side = button("Side", x: 120, width: 100, height: 40,
            focusable: bind(state.navigable), focus_on_pointer: false,
            on_click: || { state.clicks += 1; });
        let last = button("Last", x: 240, width: 100, height: 40,
            on_click: || { state.navigable = !state.navigable; });
        stage.replace(stack {
            first; side; last;
            text(derive(|| f"{state.clicks}:{side.hover}:{side.pressed}"), y: 60);
        });
        if await stage.focus(side) { throw "pointer-only button accepted focus"; }
    }"#;
    for encoded in [false, true] {
        let mut story = compiled_runtime(code, encoded);
        let (mut bridge, root) = mount(story.driver().ui());
        present(&mut story, &mut bridge, root, 1);
        let node = |label: &str| {
            story.driver().adapter().stage().nodes().find(|node| {
            matches!(&node.kind, NodeKind::Button {label: Some(text), ..} if text == label)
        }).unwrap().id
        };
        let (first, side, last) = (node("First"), node("Side"), node("Last"));
        let request = story
            .driver_mut()
            .unwrap()
            .adapter_mut()
            .take_window_request()
            .unwrap()
            .unwrap();
        assert!(
            !story
                .driver()
                .ui()
                .request_focus(&mut bridge, root, side, FocusVisibility::Show)
        );
        story
            .driver_mut()
            .unwrap()
            .adapter_mut()
            .complete_window_request(request.id, Ok(WindowReply::Focused(false)))
            .unwrap();
        story.wake_input();
        drain(&mut story);
        assert!(
            story
                .driver()
                .ui()
                .request_focus(&mut bridge, root, first, FocusVisibility::Show)
        );
        let navigate = |story: &mut StoryRuntime, bridge: &mut PresentationBridge, action| {
            for phase in [ActionPhase::Pressed, ActionPhase::Released] {
                story
                    .driver_mut()
                    .unwrap()
                    .handle_text(
                        bridge,
                        TextEvent::Navigation(NavigationEvent {
                            action,
                            phase,
                            value: if phase == ActionPhase::Pressed {
                                1.
                            } else {
                                0.
                            },
                        }),
                    )
                    .unwrap();
            }
            story.wake_input();
            drain(story);
        };
        let focus = |bridge: &PresentationBridge, node| {
            assert_eq!(
                bridge.root().focused_widget(),
                SessionUi::projected_node(bridge, root, node)
            );
        };
        navigate(&mut story, &mut bridge, NavigationAction::Right);
        focus(&bridge, last);
        navigate(&mut story, &mut bridge, NavigationAction::Left);
        focus(&bridge, first);
        for state in [keyboard::KeyState::Down, keyboard::KeyState::Up] {
            story
                .driver_mut()
                .unwrap()
                .handle_text(
                    &mut bridge,
                    key(state, keyboard::Key::Named(keyboard::NamedKey::Tab)),
                )
                .unwrap();
        }
        focus(&bridge, last);
        let pointer = |story: &mut StoryRuntime, bridge: &mut PresentationBridge, kind, x| {
            story
                .driver_mut()
                .unwrap()
                .handle_pointer(bridge, mouse(kind, (x, 20.)))
                .unwrap();
            story.wake_input();
            drain(story);
        };
        let state = |story: &StoryRuntime, expected: &str| {
            assert!(
                story.driver().adapter().stage().nodes().any(|node| {
                    matches!(&node.kind, NodeKind::Text(text) if text.as_ref() == expected)
                }),
                "expected {expected}"
            );
        };
        pointer(&mut story, &mut bridge, "move", 160.);
        pointer(&mut story, &mut bridge, "down", 160.);
        state(&story, "0:true:true");
        focus(&bridge, last);
        pointer(&mut story, &mut bridge, "up", 160.);
        state(&story, "1:true:false");
        focus(&bridge, last);
        for kind in ["move", "down", "up"] {
            pointer(&mut story, &mut bridge, kind, 280.);
        }
        present(&mut story, &mut bridge, root, 2);
        assert!(
            story
                .driver()
                .ui()
                .request_focus(&mut bridge, root, first, FocusVisibility::Show)
        );
        navigate(&mut story, &mut bridge, NavigationAction::Right);
        focus(&bridge, side);
        for kind in ["move", "down", "up"] {
            pointer(&mut story, &mut bridge, kind, 280.);
        }
        present(&mut story, &mut bridge, root, 3);
        assert!(
            !story
                .driver()
                .ui()
                .request_focus(&mut bridge, root, side, FocusVisibility::Show)
        );
        focus(&bridge, last);
        pointer(&mut story, &mut bridge, "move", 160.);
        pointer(&mut story, &mut bridge, "down", 160.);
        story
            .driver_mut()
            .unwrap()
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(false))
            .unwrap();
        pointer(&mut story, &mut bridge, "up", 160.);
        state(&story, "1:false:false");
    }
}

#[test]
fn late_next_focus_waits_for_accepted_frame_and_routes_confirm_source_asb() {
    for (visibility, prior_visible) in [("show", false), ("preserve", false), ("preserve", true)] {
        let code = r#"
        let data = model({focused: false, count: 0});
        fn main() {
            let next = button("Next", visible: false, width: 100, height: 40,
                on_click: || { data.count += 1; });
            stage.replace(column(modal: true) {
                next;
                button("Skip", width: 100, height: 40, on_click: || { throw "Skip activated"; });
                text(derive(|| f"result:{data.focused}:{data.count}:{next.focus_visible}"));
            });
            await stage.presented();
            next.visible = true;
            if !await stage.focus(next, visibility: "VISIBILITY") { throw "Next focus declined"; }
            data.focused = true;
        }
    "#
        .replace("VISIBILITY", visibility);
        for encoded in [false, true] {
            let mut story = compiled_runtime(&code, encoded);
            let (mut bridge, root) = mount(story.driver().ui());
            present(&mut story, &mut bridge, root, 1);
            let node = |label: &str| {
                story.driver().adapter().stage().nodes().find(|node| {
            matches!(&node.kind, NodeKind::Button { label: Some(text), .. } if text.as_str() == label)
        }).unwrap().id
            };
            let next = node("Next");
            let skip = node("Skip");
            let skip_widget = SessionUi::projected_node(&bridge, root, skip).unwrap();
            assert_eq!(bridge.root().focused_widget(), Some(skip_widget));
            story.wake_input();
            drain(&mut story);
            assert!(!story.driver().adapter().has_window_requests());
            assert!(
                story
                    .driver_mut()
                    .unwrap()
                    .adapter_mut()
                    .take_window_request()
                    .unwrap()
                    .is_none()
            );
            story.driver_mut().unwrap().sync(&mut bridge, root).unwrap();
            assert!(!story.driver().ui().request_focus(
                &mut bridge,
                root,
                next,
                FocusVisibility::Show
            ));
            let (recording, receipt) = story
                .driver()
                .record(&mut bridge, root, FrameId(2))
                .unwrap()
                .unwrap();
            bridge.resolve(PresentationNotice::new(
                recording.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Skipped,
            ));
            story
                .driver_mut()
                .unwrap()
                .presented(&bridge, &receipt)
                .unwrap();
            assert!(!story.driver().adapter().has_window_requests());
            assert_eq!(bridge.root().focused_widget(), Some(skip_widget));
            present(&mut story, &mut bridge, root, 3);
            assert_eq!(bridge.root().focused_widget(), Some(skip_widget));
            let request = story
                .driver_mut()
                .unwrap()
                .adapter_mut()
                .take_window_request()
                .unwrap()
                .unwrap();
            let WindowCommand::Focus {
                target,
                visibility: policy,
            } = request.command
            else {
                panic!("focus request");
            };
            assert_eq!(target, next);
            assert_eq!(
                policy,
                if visibility == "show" {
                    FocusVisibility::Show
                } else {
                    FocusVisibility::Preserve
                }
            );
            if prior_visible {
                story
                    .driver_mut()
                    .unwrap()
                    .handle_text(
                        &mut bridge,
                        TextEvent::Navigation(NavigationEvent {
                            action: NavigationAction::Up,
                            phase: ActionPhase::Pressed,
                            value: 1.,
                        }),
                    )
                    .unwrap();
            }
            assert!(
                story
                    .driver()
                    .adapter()
                    .focus_target_permits_input(next)
                    .unwrap()
            );
            assert!(
                story
                    .driver()
                    .ui()
                    .request_focus(&mut bridge, root, next, policy)
            );
            story
                .driver_mut()
                .unwrap()
                .adapter_mut()
                .complete_window_request(request.id, Ok(WindowReply::Focused(true)))
                .unwrap();
            story
                .driver_mut()
                .unwrap()
                .sync_input(&mut bridge, root)
                .unwrap();
            story.wake_input();
            drain(&mut story);
            assert_eq!(
                bridge.root().focused_widget(),
                SessionUi::projected_node(&bridge, root, next)
            );
            let expected = format!("result:true:0:{}", visibility == "show" || prior_visible);
            assert!(
                story.driver().adapter().stage().nodes().any(|node| {
                    matches!(&node.kind, NodeKind::Text(value) if value.as_ref() == expected)
                }),
                "{visibility}, prior_visible={prior_visible}"
            );
            present(&mut story, &mut bridge, root, 4);
            for phase in [ActionPhase::Pressed, ActionPhase::Released] {
                story
                    .driver_mut()
                    .unwrap()
                    .handle_text(
                        &mut bridge,
                        TextEvent::Navigation(NavigationEvent {
                            action: NavigationAction::Confirm,
                            phase,
                            value: if phase == ActionPhase::Pressed {
                                1.
                            } else {
                                0.
                            },
                        }),
                    )
                    .unwrap();
            }
            story.wake_input();
            drain(&mut story);
            assert!(story.driver().adapter().stage().nodes().any(|node| {
            matches!(&node.kind, NodeKind::Text(value) if value.as_ref() == "result:true:1:true")
        }));
        }
    }
}
