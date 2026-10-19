use super::*;
use reel_ui::core::SelectionGroupId;
use reel_ui::input::{ActionPhase as P, NavigationAction as A, NavigationEvent};
use reel_ui::stage::VirtualRow;

#[test]
fn radio_group_deferred_selection_requires_current_anchor_and_accepted_frame() {
    for cancellation in ["none", "focus", "clear", "retire", "modal", "binding"] {
        let (mut f, list, first) = virtual_fixture(2);
        let group = SelectionGroupId(1, 0);
        let radio = |selected, group| NodeKind::Radio {
            label: None,
            selected,
            group: Some(group),
            revision: 0,
        };
        let props = f.stage.node(first).unwrap().props.clone();
        f.stage.update(first, radio(true, group), props).unwrap();
        let initial = f.record();
        f.accept(&initial);
        let nav = |action, phase| {
            TextEvent::Navigation(NavigationEvent {
                action,
                phase,
                value: 1.,
            })
        };
        f.ui.handle_text(&mut f.bridge, nav(A::Down, P::Pressed))
            .unwrap();
        f.ui.handle_text(&mut f.bridge, nav(A::Down, P::Released))
            .unwrap();
        f.ui.handle_text(&mut f.bridge, nav(A::Down, P::Pressed))
            .unwrap();
        f.ui.handle_text(&mut f.bridge, nav(A::Down, P::Released))
            .unwrap();
        // A confirm started before the destination exists must not select twice.
        f.ui.handle_text(&mut f.bridge, nav(A::Confirm, P::Pressed))
            .unwrap();
        let second = f
            .stage
            .create(
                radio(false, group),
                NodeProps {
                    height: Some(40.),
                    ..Default::default()
                },
                vec![],
            )
            .unwrap();
        f.stage.add(list, second, 1).unwrap();
        let node = f.stage.node(list).unwrap();
        let NodeKind::VirtualList(mut view) = node.kind.clone() else {
            panic!()
        };
        let props = node.props.clone();
        view.revision += 1;
        view.rows = Arc::from([
            VirtualRow {
                node: first,
                index: 0,
                offset: 0.,
            },
            VirtualRow {
                node: second,
                index: 1,
                offset: 42.,
            },
        ]);
        view.navigation = Some(VirtualListNavigation {
            request: 1,
            ready: true,
            index: 1,
            resolved_index: 1,
            forward: true,
        });
        if cancellation == "retire" {
            f.stage.remove(first).unwrap();
            view.rows = Arc::from([VirtualRow {
                node: second,
                index: 1,
                offset: 42.,
            }]);
        }
        if cancellation == "modal" {
            f.edit(f.root, |props| props.modal = true);
        }
        if cancellation == "binding" {
            let props = f.stage.node(first).unwrap().props.clone();
            f.stage
                .update(first, radio(true, SelectionGroupId(2, 0)), props)
                .unwrap();
        }
        f.stage
            .update(list, NodeKind::VirtualList(view), props)
            .unwrap();
        let pending = f.record();
        assert!(
            !f.ui
                .take_events()
                .iter()
                .any(|e| matches!(e, NodeEvent::Change(_))),
            "recording is not acceptance"
        );
        if cancellation == "focus" {
            f.ui.handle_text(&mut f.bridge, TextEvent::WindowFocusChange(false))
                .unwrap();
        }
        if cancellation == "clear" {
            f.bridge.clear();
            f.ui.clear_presented();
            assert_eq!(
                f.bridge.resolve(PresentationNotice::new(
                    pending.key(),
                    SurfaceEpoch(1),
                    PresentationOutcome::Accepted(pending.key().frame_id())
                )),
                NoticeResult::StaleOrForeign
            );
            let next = f.record();
            f.accept(&next);
        } else {
            f.accept(&pending);
        }
        let changes =
            f.ui.take_events()
                .into_iter()
                .filter_map(|e| match e {
                    NodeEvent::Change(c) => Some(c.node),
                    _ => None,
                })
                .collect::<Vec<_>>();
        if cancellation == "none" {
            assert_eq!(
                changes,
                [second],
                "accepted navigation updates the value with its focus"
            );
            assert_eq!(
                f.bridge.root().focused_widget(),
                Some(f.layouts()[&second].0)
            );
        } else {
            assert!(
                changes.is_empty(),
                "cancelled {cancellation} selection leaked"
            );
        }
        let release =
            f.ui.handle_text(&mut f.bridge, nav(A::Confirm, P::Released))
                .unwrap();
        assert!(
            !release
                .events
                .iter()
                .any(|e| matches!(e, NodeEvent::Change(_))),
            "old confirmation cannot activate the destination"
        );
    }
}
