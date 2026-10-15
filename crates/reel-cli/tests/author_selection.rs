//! Shared business values keep author-owned controls exclusive from source and ASB.
#[path = "support/author.rs"]
#[allow(dead_code)]
mod support;
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{VmLimits, source::compile_story},
};
use reel_player::aside_session::AsideSessionUi;
use reel_ui::{
    core::*,
    stage::{NodeId, NodeKind, ToggleAppearance},
};
use support::*;

const SOURCE: &str = r##"
fn main() {
    let data = model({enabled: false, language: "en", extra: "fr", modal: false});
    let count = text("changes", x: 0, y: 380);
    let changed = |event| {
        if event.target.value != event.value { throw "callback observed uncommitted value"; }
        count.x += 1;
    };
    let enabled = checkbox("Enabled", width: 220, height: 40, value: bind(data.enabled), on_change: changed);
    let chinese = radio("zh-CN", "Chinese", y: 60, width: 220, height: 40, value: bind(data.language), on_change: changed);
    let english = radio("e" + "n", "English", y: 110, width: 220, height: 40, value: bind(data.language), on_change: changed);
    let extra = radio(bind(data.extra), "Extra", y: 160, width: 220, height: 40, value: bind(data.language), on_change: changed);
    let disabled = radio("disabled", "Disabled", y: 210, width: 220, height: 40, enabled: false, value: bind(data.language), on_change: changed);
    let reset = button("Reset", x: 300, y: 0, width: 220, height: 40, on_click: |e| {
        data.enabled = false; data.language = "en"; data.extra = "de";
    });
    let custom = checkbox(value: bind(data.enabled), x: 300, y: 60, width: 220, height: 40, decorated: false, on_change: changed);
    custom.add(text("Custom", color: derive(|| if custom.value { "#00ff00" } else { "#ffffff" })));
    let retire = button("Remove", x: 300, y: 110, width: 220, height: 40, on_click: |e| { extra.remove(); });
    let overlay = stack(width: 640, height: 420, modal: true, visible: bind(data.modal)) {
        button("Close", width: 100, height: 40, on_click: |e| { data.modal = false; });
    };
    let open = button("Modal", x: 300, y: 160, width: 220, height: 40, on_click: |e| { data.modal = true; });
    let mut rejected = 0;
    try { checkbox(value: derive(|| data.enabled)); } catch e { rejected += 1; }
    try { radio("en", value: derive(|| data.language)); } catch e { rejected += 1; }
    try { enabled.value = 1; } catch e { rejected += 1; }
    if rejected != 3 { throw "writable value contract"; }
    stage.replace(stack(children: [enabled, chinese, english, extra, disabled, reset, custom, retire, open, count, overlay]));
    narrator("Control input must not advance this line.");
}
"##;

fn drivers(source: &str) -> Vec<AsideSessionUi> {
    let checked = reel_cli::check(source, "selection.aside").unwrap();
    let bytes = reel_cli::build(&checked, Some("main::main")).unwrap().bytes;
    [
        compile_story(source, "main", 1, "main").unwrap(),
        reel_cli::decode_story(&bytes).unwrap(),
    ]
    .into_iter()
    .map(|story| {
        let mut d = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut d);
        d
    })
    .collect()
}
fn radio(driver: &AsideSessionUi, label: &str) -> (NodeId, bool, u64) {
    driver
        .adapter()
        .stage()
        .nodes()
        .find_map(|n| match &n.kind {
            NodeKind::Radio {
                label: Some(name),
                selected,
                revision,
            } if name == label => Some((n.id, *selected, *revision)),
            _ => None,
        })
        .unwrap()
}
fn count(driver: &AsideSessionUi) -> f64 {
    driver
        .adapter()
        .stage()
        .nodes()
        .find(|n| n.kind == NodeKind::Text("changes".into()))
        .unwrap()
        .props
        .x
}
fn checked(driver: &AsideSessionUi) -> Vec<bool> {
    driver
        .adapter()
        .stage()
        .nodes()
        .filter_map(|n| match n.kind {
            NodeKind::Toggle {
                value,
                appearance: ToggleAppearance::Checkbox,
                ..
            } => Some(value),
            _ => None,
        })
        .collect()
}

#[test]
fn selections_share_values_and_keep_identity_across_input_and_programmatic_changes() {
    for pointer in [PointerType::Mouse, PointerType::Touch] {
        for mut d in drivers(SOURCE) {
            let (mut b, root) = bridge(&d);
            present(&mut d, &mut b, root, 1);
            let interaction = d.ui().session().view().interaction.unwrap().id();
            let chinese = radio(&d, "Chinese").0;
            assert!(radio(&d, "English").1, "strings compare by contents");
            click_with_pointer(&mut d, &mut b, 50., 20., pointer);
            present(&mut d, &mut b, root, 2);
            assert_eq!(checked(&d), [true, true]);
            assert_eq!(count(&d), 1.);
            click_with_pointer(&mut d, &mut b, 50., 80., pointer);
            present(&mut d, &mut b, root, 3);
            assert!(radio(&d, "Chinese").1);
            assert!(!radio(&d, "English").1);
            assert_eq!(count(&d), 2.);
            click_with_pointer(&mut d, &mut b, 50., 80., pointer);
            present(&mut d, &mut b, root, 4);
            assert_eq!(count(&d), 2., "selecting again is silent");
            gamepad_press(&mut d, &mut b, reel_ui::input::GamepadButton::DpadDown);
            gamepad_press(&mut d, &mut b, reel_ui::input::GamepadButton::South);
            present(&mut d, &mut b, root, 5);
            assert!(radio(&d, "English").1);
            assert_eq!(count(&d), 3.);
            let target = b.root().focused_widget().unwrap().into();
            d.handle_access(
                &mut b,
                reel_ui::accesskit::ActionRequest {
                    action: reel_ui::accesskit::Action::Click,
                    target_tree: reel_ui::accesskit::TreeId::ROOT,
                    target_node: target,
                    data: None,
                },
            )
            .unwrap();
            settle(&mut d);
            assert_eq!(
                count(&d),
                3.,
                "accessible activation has the same exclusive behavior"
            );
            click_with_pointer(&mut d, &mut b, 50., 230., pointer);
            assert_eq!(count(&d), 3., "disabled radio cannot choose");
            click_with_pointer(&mut d, &mut b, 350., 20., pointer);
            click_with_pointer(&mut d, &mut b, 50., 180., pointer);
            assert_eq!(
                count(&d),
                3.,
                "old presentation cannot choose a replaced option"
            );
            present(&mut d, &mut b, root, 6);
            assert_eq!(checked(&d), [false, false]);
            assert_eq!(count(&d), 3., "programmatic resets are silent");
            assert_eq!(radio(&d, "Chinese").0, chinese);
            click_with_pointer(&mut d, &mut b, 50., 180., pointer);
            present(&mut d, &mut b, root, 7);
            assert!(radio(&d, "Extra").1, "changed option is selectable");
            assert!(!radio(&d, "English").1);
            click_with_pointer(&mut d, &mut b, 350., 80., pointer);
            present(&mut d, &mut b, root, 8);
            assert_eq!(
                checked(&d),
                [true, true],
                "custom children preserve checkbox behavior"
            );
            assert!(
                d.adapter()
                    .stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text("Custom".into())
                        && n.props.color == [0, 255, 0, 255])
            );
            click_with_pointer(&mut d, &mut b, 350., 180., pointer);
            present(&mut d, &mut b, root, 9);
            let before = count(&d);
            click_with_pointer(&mut d, &mut b, 50., 80., pointer);
            assert_eq!(count(&d), before, "modal owns input");
            click_with_pointer(&mut d, &mut b, 50., 20., pointer);
            present(&mut d, &mut b, root, 10);
            click_with_pointer(&mut d, &mut b, 350., 130., pointer);
            click_with_pointer(&mut d, &mut b, 50., 180., pointer);
            assert_eq!(count(&d), before, "retired option cannot choose");
            present(&mut d, &mut b, root, 11);
            assert_eq!(
                d.ui().session().view().interaction.unwrap().id(),
                interaction
            );
        }
    }
}
