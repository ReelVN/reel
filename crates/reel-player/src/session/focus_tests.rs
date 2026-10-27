//! Explicit author focus follows current accepted stage geometry without a native window.
use super::*;
use reel_engine::aside::{
    source,
    window::{WindowCommand, WindowReply},
};
use reel_ui::input::{ActionPhase, FocusVisibility, NavigationAction, NavigationEvent};

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
            let mut compiled = source::compile_story(&code, "focus_player", 1, "main").unwrap();
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
                AsideSessionUi::from_compiled(session(), compiled, "en", VmLimits::default())
                    .unwrap(),
            );
            story.set_window_control_enabled(true);
            drain(&mut story);
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
