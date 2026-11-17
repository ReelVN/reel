use super::*;
use reel_ui::core::{SelectionGroupId, TabNavigation};
use reel_ui::input::{ActionPhase, NavigationAction, NavigationEvent};
use reel_ui::stage::NodeSemantics;

fn navigation(f: &mut Fixture, action: NavigationAction) -> Vec<NodeEvent> {
    let mut events = Vec::new();
    for phase in [ActionPhase::Pressed, ActionPhase::Released] {
        events.extend(
            f.ui.handle_text(
                &mut f.bridge,
                TextEvent::Navigation(NavigationEvent {
                    action,
                    phase,
                    value: 1.,
                }),
            )
            .unwrap()
            .events,
        );
    }
    events
}
fn fixture(vertical: bool, manual: bool) -> (Fixture, [NodeId; 3]) {
    let mut f = Fixture::new(None);
    let mut stage = Stage::default();
    let mut options = Vec::new();
    for i in 0..3 {
        options.push(
            stage
                .create(
                    NodeKind::Radio {
                        label: Some(format!("Tab {i}")),
                        selected: i == 0,
                        group: Some(SelectionGroupId(1, 0)),
                        revision: 0,
                        tabs: Some(TabNavigation { vertical, manual }),
                    },
                    NodeProps {
                        semantics: NodeSemantics::Tab,
                        x: if vertical { 0. } else { i as f64 * 100. },
                        y: if vertical { i as f64 * 40. } else { 0. },
                        width: Some(90.),
                        height: Some(35.),
                        ..Default::default()
                    },
                    vec![],
                )
                .unwrap(),
        );
    }
    let ids: [NodeId; 3] = options.clone().try_into().unwrap();
    let root = stage
        .create(
            NodeKind::Stack,
            NodeProps {
                semantics: NodeSemantics::TabList,
                ..Default::default()
            },
            options,
        )
        .unwrap();
    stage.replace(root).unwrap();
    f.stage = stage;
    f.root = root;
    let frame = f.record();
    f.accept(&frame);
    let first = f.layouts()[&ids[0]].0;
    assert!(f.bridge.focus_on(Some(first)));
    (f, ids)
}

#[test]
fn native_tabs_use_accepted_focus_geometry_and_manual_or_automatic_activation() {
    for vertical in [false, true] {
        for manual in [false, true] {
            let (mut f, ids) = fixture(vertical, manual);
            let forward = if vertical {
                NavigationAction::Down
            } else {
                NavigationAction::Right
            };
            let events = navigation(&mut f, forward);
            let second = f.layouts()[&ids[1]].0;
            assert_eq!(f.bridge.root().focused_widget(), Some(second));
            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(e,NodeEvent::Change(change) if change.node==ids[1]))
                    .count(),
                usize::from(!manual)
            );
            if manual {
                let events = navigation(&mut f, NavigationAction::Confirm);
                assert!(
                    events
                        .iter()
                        .any(|e| matches!(e,NodeEvent::Change(change) if change.node==ids[1]))
                );
            }
            navigation(&mut f, forward);
            f.ui.take_events();
            navigation(&mut f, forward);
            assert_eq!(
                f.bridge.root().focused_widget(),
                Some(f.layouts()[&ids[0]].0)
            );
        }
    }
}

#[test]
fn native_tab_accessibility_exposes_selected_tabs_and_tab_list() {
    let (f, ids) = fixture(false, true);
    let properties = f.layouts();
    let tab = f.bridge.root().get_widget(properties[&ids[0]].0).unwrap();
    assert_eq!(tab.accessibility_role(), Role::Tab);
    let list = f.bridge.root().get_widget(properties[&f.root].0).unwrap();
    assert_eq!(list.accessibility_role(), Role::TabList);
}

#[test]
fn custom_manual_tabs_keep_native_focus_feedback_before_activation() {
    use reel_ui::imaging::record::{Command, Draw, Geometry};
    let (mut f, ids) = fixture(false, true);
    for id in ids {
        f.edit(id, |props| props.decorated = false);
        let content = f
            .stage
            .create(
                NodeKind::Stack,
                NodeProps {
                    width: Some(80.),
                    height: Some(25.),
                    background: Some([20, 40, 80, 255]),
                    ..Default::default()
                },
                vec![],
            )
            .unwrap();
        f.stage.add(id, content, 0).unwrap();
    }
    let custom = f.record();
    f.accept(&custom);
    f.ui.handle_text(&mut f.bridge, TextEvent::WindowFocusChange(true))
        .unwrap();
    let events = navigation(&mut f, NavigationAction::Right);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, NodeEvent::Change(_)))
    );
    assert_eq!(
        f.bridge.root().focused_widget(),
        Some(f.layouts()[&ids[1]].0)
    );
    let frame = f.record();
    let scene = frame.scene().unwrap();
    let rings=scene.commands().iter().filter(|command|match command {
        Command::Draw(id)=>matches!(scene.draw_op(*id),Draw::Stroke{shape:Geometry::RoundedRect(rect),stroke,transform,..}
            if stroke.width==2. && rect.rect()==Rect::new(2.,2.,88.,33.) && transform.translation()==Vec2::new(100.,0.)),
        _=>false,
    }).count();
    assert_eq!(
        rings, 1,
        "custom content retains a visible native focus ring without selecting"
    );
}
