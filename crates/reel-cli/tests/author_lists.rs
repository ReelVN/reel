//! Real source/ASB, accepted geometry and portable input for keyed author UI.
#[path = "support/author.rs"]
mod support;
use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};
use reel_player::aside_session::AsideSessionUi;
use reel_ui::{input::GamepadButton, stage::NodeKind};
use support::*;

#[test]
fn virtual_list_source_and_asb_layout_navigation_and_filter_share_one_stage() {
    let source = r#"
        fn main() {
            let data=model({items:[],even:false,selected:-1});
            let mut i=0; while i<1000 {data.items.append({id:i,expanded:false});i+=1;}
            stage.replace(stack(width:600,height:400) {
                scroll(width:400,height:180) {
                    list(|| {
                        let result=[];
                        for item in data.items {if !data.even || item.id%2==0 {result.append(item);}}
                        result
                    },key:|item|item.id,virtual:true,build:|row|column(gap:2) {
                        button(label:derive(||f"item {row.value.id}"),height:30,width:300,on_click:|e|{
                            data.selected=row.value.id;row.value.expanded=!row.value.expanded;
                        });
                        text("description",height:18+(row.value.id%3)*12);
                        text("expanded",height:40,visible:derive(||row.value.expanded));
                    });
                };
                button("filter",x:450,y:0,width:100,height:40,on_click:|e|{data.even=!data.even;});
                text(value:derive(||f"selected:{data.selected}"),y:240);
            });
            narrator("ready");
        }
    "#;
    let checked = reel_cli::check(source, "virtual.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for compiled in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            compiled,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let (mut bridge, root) = bridge(&driver);
        let mut frame = 0;
        let mut flush = |driver: &mut AsideSessionUi,
                         bridge: &mut reel_ui::bridge::PresentationBridge| {
            for _ in 0..5 {
                frame += 1;
                present(driver, bridge, root, frame);
                settle(driver);
            }
        };
        flush(&mut driver, &mut bridge);
        assert!(driver.adapter().stage().nodes().count() < 80);
        click_at(&mut driver, &mut bridge, 20., 15.);
        flush(&mut driver, &mut bridge);
        for _ in 0..25 {
            gamepad_press(&mut driver, &mut bridge, GamepadButton::DpadDown);
            flush(&mut driver, &mut bridge);
        }
        gamepad_press(&mut driver, &mut bridge, GamepadButton::South);
        flush(&mut driver, &mut bridge);
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("selected:25".into()))
        );
        assert!(driver.adapter().stage().nodes().count() < 100);
        click_at(&mut driver, &mut bridge, 470., 20.);
        flush(&mut driver, &mut bridge);
        let view = driver
            .adapter()
            .stage()
            .nodes()
            .find_map(|n| {
                if let NodeKind::VirtualList(v) = &n.kind {
                    Some(v)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(view.total_rows, 500);
        assert!(view.rows.len() < 30);
        driver.cancel().unwrap();
    }
}

#[test]
fn source_and_asb_reorder_preserves_focus_and_reads_the_replacement_record() {
    let source = r#"
    fn main() {
        let data = model({items: [{id: {}, name: "A"}, {id: [], name: "B"}], clicked: 0, latest: ""});
        let a = data.items[0].id; let b = data.items[1].id;
        stage.replace(column(width: 600, height: 400) {
            list(|| data.items, key: |item| item.id, build: |row| {
                button(label: derive(|| row.value.name), width: 180, height: 50, on_click: |e| {
                    data.latest = row.value.name;
                    data.clicked += 1;
                    if data.clicked == 1 { data.items = [{id: b, name: "Bee"}, {id: a, name: "Ay"}, {id: null, name: "C"}]; }
                    else if data.clicked == 2 { data.items.remove(row.index); }
                })
            });
            text(value: derive(|| f"{data.clicked}:{data.latest}"));
        });
        narrator("waiting");
    }"#;
    let checked = reel_cli::check(source, "list.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for compiled in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            compiled,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let id = driver
            .adapter()
            .stage()
            .nodes()
            .find(|n| matches!(&n.kind, NodeKind::Button {label: Some(s), ..} if &**s == "A"))
            .unwrap()
            .id;
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        click_at(&mut driver, &mut bridge, 30., 25.);
        let focused = bridge.root().focused_widget().unwrap();
        present(&mut driver, &mut bridge, root, 2);
        assert_eq!(bridge.root().focused_widget(), Some(focused));
        assert!(
            matches!(&driver.adapter().stage().node(id).unwrap().kind, NodeKind::Button { label: Some(s), .. } if &**s == "Ay")
        );
        gamepad_press(&mut driver, &mut bridge, GamepadButton::South);
        present(&mut driver, &mut bridge, root, 3);
        assert!(driver.adapter().stage().node(id).is_none());
        assert_ne!(bridge.root().focused_widget(), Some(focused));
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("2:Ay".into()))
        );
        assert!(driver.ui().session().view().interaction.is_some());
        driver.cancel().unwrap();
    }
}

#[test]
fn opaque_choice_ids_work_as_list_keys_through_source_and_asb() {
    let source = r#"fn main() {
        let choices = game.choice();
        stage.replace(list(|| choices.options, key: |option| option.id, build: |row|
            button(row.value.label, width: 200, height: 50,
                on_click: |event| choices.select(row.value.id))));
        let selected = choose([{key: "a", label: "A"}, {key: "b", label: "B"}]);
        stage.replace(text(selected));
        narrator("selected");
    }"#;
    let checked = reel_cli::check(source, "choices.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for compiled in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            compiled,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        click_at(&mut driver, &mut bridge, 20., 75.);
        let stage = driver.adapter().stage();
        assert_eq!(
            stage.node(stage.root().unwrap()).unwrap().kind,
            NodeKind::Text("b".into())
        );
        driver.cancel().unwrap();
    }
}

#[test]
fn keyboard_edges_repeat_and_cancel_while_focused_editors_keep_ime() {
    use reel_ui::core::{Ime, TextEvent, keyboard::*};
    let source = r#"fn main() {
        let data = model({log: "", name: ""});
        stage.replace(input(width: 600, height: 400, on_key: |e| {
            data.log += f"{e.code}:{e.phase};";
        }) {
            text(value: bind(data.log), x: 0, y: 0);
            text_input(value: bind(data.name), x: 0, y: 100, width: 240, height: 50);
            button("English", x: 0, y: 200, width: 240, height: 50,
                on_click: |event| { data.log += "button;"; });
        });
    }"#;
    let checked = reel_cli::check(source, "keys.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for compiled in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            compiled,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        let mut frame = 1;
        let mut send = |driver: &mut AsideSessionUi,
                        bridge: &mut reel_ui::bridge::PresentationBridge,
                        event| {
            driver.handle_text(bridge, event).unwrap();
            settle(driver);
            frame += 1;
            present(driver, bridge, root, frame);
        };
        for (code, key, state, repeat) in [
            (Code::KeyA, "a", KeyState::Down, false),
            (Code::KeyB, "b", KeyState::Down, false),
            (Code::KeyA, "a", KeyState::Down, true),
            (Code::KeyA, "a", KeyState::Up, false),
        ] {
            send(
                &mut driver,
                &mut bridge,
                TextEvent::Keyboard(KeyboardEvent {
                    code,
                    key: Key::Character(key.into()),
                    state,
                    repeat,
                    location: Location::Standard,
                    ..Default::default()
                }),
            );
        }
        send(
            &mut driver,
            &mut bridge,
            TextEvent::WindowFocusChange(false),
        );
        let expected = "KeyA:pressed;KeyB:pressed;KeyA:repeated;KeyA:released;KeyB:cancelled;";
        let log = |driver: &AsideSessionUi| {
            driver
                .adapter()
                .stage()
                .nodes()
                .find_map(|n| {
                    if let NodeKind::Text(s) = &n.kind {
                        Some(s.to_string())
                    } else {
                        None
                    }
                })
                .unwrap()
        };
        assert_eq!(log(&driver), expected);
        send(&mut driver, &mut bridge, TextEvent::WindowFocusChange(true));
        click_at(&mut driver, &mut bridge, 20., 220.);
        settle(&mut driver);
        frame += 1;
        present(&mut driver, &mut bridge, root, frame);
        let mut send = |driver: &mut AsideSessionUi,
                        bridge: &mut reel_ui::bridge::PresentationBridge,
                        event| {
            driver.handle_text(bridge, event).unwrap();
            settle(driver);
            frame += 1;
            present(driver, bridge, root, frame);
        };
        // The language button keeps focus. Its confirmation stays local, while
        // an unrelated typing key reaches the ancestor's author callback.
        for state in [KeyState::Down, KeyState::Up] {
            send(
                &mut driver,
                &mut bridge,
                TextEvent::Keyboard(KeyboardEvent {
                    code: Code::Enter,
                    key: Key::Named(NamedKey::Enter),
                    state,
                    ..Default::default()
                }),
            );
        }
        let expected = format!("{expected}button;button;");
        send(
            &mut driver,
            &mut bridge,
            TextEvent::Keyboard(KeyboardEvent {
                code: Code::KeyC,
                key: Key::Character("c".into()),
                state: KeyState::Down,
                location: Location::Standard,
                ..Default::default()
            }),
        );
        click_at(&mut driver, &mut bridge, 20., 120.);
        let expected = format!("{expected}KeyC:pressed;KeyC:cancelled;");
        send(
            &mut driver,
            &mut bridge,
            TextEvent::Ime(Ime::Preedit("ni".into(), Some((2, 2)))),
        );
        send(
            &mut driver,
            &mut bridge,
            TextEvent::Ime(Ime::Commit("你".into())),
        );
        send(
            &mut driver,
            &mut bridge,
            TextEvent::Keyboard(KeyboardEvent {
                code: Code::KeyX,
                key: Key::Character("x".into()),
                state: KeyState::Down,
                location: Location::Standard,
                ..Default::default()
            }),
        );
        assert_eq!(log(&driver), expected);
        assert!(driver.adapter().stage().nodes().any(|n| matches!(&n.kind,
            NodeKind::TextInput { value, .. } if value.as_ref() == "你x")));
        driver.cancel().unwrap();
    }
}

#[test]
fn virtual_scroll_to_source_and_asb_complete_only_after_real_layout_and_acceptance() {
    use reel_core::identity::{FrameId, SurfaceEpoch};
    use reel_ui::bridge::{PresentationNotice, PresentationOutcome};
    let source = r#"fn main(){
        let data=model({items:[],done:false});let mut i=0;while i<1000{data.items.append(i);i+=1;}
        let items=list(||data.items,key:|x|x,virtual:true,build:|row|column(gap:4){
            button(f"row {row.value}",height:30,width:300);
            text("description",height:20+row.value%3*15);
        });
        stage.replace(stack(width:600,height:400){
            scroll(width:400,height:180){items};
            button("jump",x:450,y:0,width:100,height:40,on_click:|e|{
                let row=await items.scroll_to(980,align:"center");row.opacity=0.75;data.done=true;
            });
            text(value:derive(||if data.done{"done"}else{"waiting"}),y:240);
        });narrator("ready");
    }"#;
    let checked = reel_cli::check(source, "position.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for compiled in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            compiled,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let (mut bridge, root) = bridge(&driver);
        for frame in 1..6 {
            present(&mut driver, &mut bridge, root, frame);
            settle(&mut driver);
        }
        click_at(&mut driver, &mut bridge, 470., 20.);
        for frame in 6..14 {
            driver.sync(&mut bridge, root).unwrap();
            let recording = bridge.record(FrameId(frame)).unwrap();
            bridge.resolve(PresentationNotice::new(
                recording.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Skipped,
            ));
            settle(&mut driver);
            assert!(
                !driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|n| n.kind == NodeKind::Text("done".into()))
            );
        }
        for frame in 14..20 {
            present(&mut driver, &mut bridge, root, frame);
            settle(&mut driver);
        }
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("done".into()))
        );
        let stage = driver.adapter().stage();
        let list = stage
            .nodes()
            .find(|n| matches!(n.kind, NodeKind::VirtualList(_)))
            .unwrap();
        let NodeKind::VirtualList(view) = &list.kind else {
            unreachable!()
        };
        let row = view.rows.iter().find(|r| r.index == 980).unwrap();
        assert_eq!(stage.node(row.node).unwrap().props.opacity, 0.75);
        assert!(stage.nodes().count() < 80);
        driver.cancel().unwrap();
    }
}

#[test]
fn natural_gallery_example_source_and_asb_measure_only_the_window() {
    let checked = reel_cli::check(
        include_str!("../../../examples/author_list_gallery/src/main.aside"),
        "natural-gallery.aside",
    )
    .unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for compiled in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            compiled,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let (mut bridge, root) = bridge(&driver);
        for frame in 1..5 {
            present(&mut driver, &mut bridge, root, frame);
            settle(&mut driver);
            let view = driver
                .adapter()
                .stage()
                .nodes()
                .find_map(|n| match &n.kind {
                    NodeKind::VirtualList(view) => Some(view),
                    _ => None,
                })
                .unwrap();
            assert!(view.total_height > 480.);
            assert!(view.rows.len() < 20);
            if frame < 3 {
                continue;
            }
            for row in view.rows.iter() {
                if let Some(next) = view.rows.iter().find(|next| next.index == row.index + 1) {
                    let expected = if row.index % 10 == 0 { 100. } else { 60. };
                    assert!((next.offset - row.offset - expected).abs() < 1e-6);
                }
            }
        }
        driver.cancel().unwrap();
    }
}

mod author_radio_navigation {
    //! Radio keyboard semantics execute through real source/ASB sessions and accepted frames.
    use super::support;
    use reel_engine::{
        Epoch, Session, SessionId,
        aside::{VmLimits, source::compile_story},
    };
    use reel_player::aside_session::AsideSessionUi;
    use reel_ui::{bridge::PresentationBridge, core::*, stage::NodeKind};
    use support::*;

    fn drivers(source: &str) -> Vec<AsideSessionUi> {
        let checked = reel_cli::check(source, "radio.aside").unwrap();
        let asb = reel_cli::build(&checked, Some("main::main")).unwrap().bytes;
        [
            compile_story(source, "main", 1, "main").unwrap(),
            reel_player::distribution::decode_story(&asb).unwrap(),
        ]
        .into_iter()
        .map(|story| {
            let mut driver = AsideSessionUi::from_compiled(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                story,
                "en",
                VmLimits::default(),
            )
            .unwrap();
            settle(&mut driver);
            driver
        })
        .collect()
    }

    fn press(
        d: &mut AsideSessionUi,
        b: &mut PresentationBridge,
        named: keyboard::NamedKey,
        shift: bool,
    ) {
        for state in [keyboard::KeyState::Down, keyboard::KeyState::Up] {
            d.handle_text(
                b,
                TextEvent::Keyboard(keyboard::KeyboardEvent {
                    state,
                    key: keyboard::Key::Named(named),
                    code: keyboard::Code::Unidentified,
                    location: keyboard::Location::Standard,
                    modifiers: if shift {
                        keyboard::Modifiers::SHIFT
                    } else {
                        keyboard::Modifiers::empty()
                    },
                    repeat: false,
                    is_composing: false,
                }),
            )
            .unwrap();
        }
        settle(d);
    }

    fn text(d: &AsideSessionUi, prefix: &str) -> String {
        d.adapter()
            .stage()
            .nodes()
            .find_map(|node| match &node.kind {
                NodeKind::Text(text) if text.as_ref().starts_with(prefix) => Some(text.to_string()),
                _ => None,
            })
            .unwrap()
    }

    const SOURCE: &str = r#"
fn main() {
    let state = model({value:"c", other:"y", changes:0});
    let changed = |e| {
        if e.target.value != e.value {throw "uncommitted selection";}
        state.changes += 1;
    };
    let before = button("Before", width:120,height:30);
    let a=radio("a","A",value:bind(state.value),on_change:changed,width:120,height:30,y:40);
    let b=radio("b","B",value:bind(state.value),enabled:false,width:120,height:30,y:80);
    let c=radio("c","C",value:bind(state.value),on_change:changed,width:120,height:30,y:120,decorated:false);
    let hidden=radio("hidden",value:bind(state.value),visible:false,width:120,height:30,y:160);
    let x=radio("x","X",value:bind(state.other),on_change:changed,width:120,height:30,x:180);
    let y=radio("y","Y",value:bind(state.other),on_change:changed,width:120,height:30,x:180,y:40);
    let after=button("Set C",x:350,width:120,height:30,on_click:||{state.value="c";});
    let disable=button("Disable C",x:350,y:40,width:120,height:30,on_click:||{c.enabled=false;});
    let status=text(derive(|| f"state:{state.value}:{state.other}:{state.changes}"),y:340);
    let focus=text(derive(|| if before.focused {"focus:before"} else if a.focused {"focus:a"}
        else if c.focused {"focus:c"} else if x.focused {"focus:x"} else if y.focused {"focus:y"}
        else if after.focused {"focus:after"} else if disable.focused {"focus:disable"} else {"focus:none"}),y:380);
    stage.replace(stack(children:[before,a,x,b,c,hidden,y,after,disable,status,focus]));
    narrator("ready");
}
"#;

    #[test]
    fn radio_groups_have_one_tab_stop_and_direction_selects_without_confirmation() {
        use keyboard::NamedKey::*;
        for mut d in drivers(SOURCE) {
            let (mut b, root) = bridge(&d);
            present(&mut d, &mut b, root, 1);
            for (key, shift, expected) in [
                (Tab, false, "before"),
                (Tab, false, "c"),
                (Tab, false, "y"),
                (Tab, true, "c"),
            ] {
                press(&mut d, &mut b, key, shift);
                assert_eq!(text(&d, "focus:"), format!("focus:{expected}"));
            }
            press(&mut d, &mut b, ArrowRight, false);
            assert_eq!(text(&d, "state:"), "state:a:y:1");
            assert_eq!(text(&d, "focus:"), "focus:a");
            present(&mut d, &mut b, root, 2);
            gamepad_press(&mut d, &mut b, reel_ui::input::GamepadButton::DpadUp);
            assert_eq!(text(&d, "state:"), "state:c:y:2");
            assert_eq!(text(&d, "focus:"), "focus:c");
            present(&mut d, &mut b, root, 3);
            press(&mut d, &mut b, Enter, false);
            assert_eq!(text(&d, "state:"), "state:c:y:2");
            click_at(&mut d, &mut b, 60., 55.);
            present(&mut d, &mut b, root, 4);
            click_at(&mut d, &mut b, 60., 55.);
            assert_eq!(text(&d, "state:"), "state:a:y:3");
            click_at(&mut d, &mut b, 400., 15.);
            assert_eq!(text(&d, "state:"), "state:c:y:3");
            assert_eq!(
                text(&d, "focus:"),
                "focus:after",
                "script writes do not steal focus"
            );
            present(&mut d, &mut b, root, 5);
            press(&mut d, &mut b, Tab, true);
            assert_eq!(text(&d, "focus:"), "focus:y");
            press(&mut d, &mut b, Tab, true);
            assert_eq!(text(&d, "focus:"), "focus:c");
            click_at(&mut d, &mut b, 400., 55.);
            present(&mut d, &mut b, root, 6);
            press(&mut d, &mut b, Tab, true);
            press(&mut d, &mut b, Tab, true);
            press(&mut d, &mut b, Tab, true);
            assert_eq!(
                text(&d, "focus:"),
                "focus:a",
                "disabled selection falls back without changing value"
            );
            assert_eq!(text(&d, "state:"), "state:c:y:3");
            press(&mut d, &mut b, ArrowDown, false);
            assert_eq!(text(&d, "state:"), "state:a:y:4");
        }
    }

    #[test]
    fn radio_group_keyboard_is_owned_by_control_and_modal_members_are_isolated() {
        use keyboard::NamedKey::*;
        let source = r#"
    fn main(){
        let state=model({value:1,open:false,keys:0,changes:0});
        let changed=||{state.changes+=1;};
        let outer=input(on_key:||{state.keys+=1;},width:600,height:400){
            radio(0,"zero",value:bind(state.value),width:120,height:30,on_change:changed);
            radio(1,"one",value:bind(state.value),y:40,width:120,height:30,on_change:changed);
            button("open",x:200,width:120,height:30,on_click:||{state.open=true;});
        };
        let first=radio(2,"two",value:bind(state.value),width:120,height:30,on_change:changed);
        let last=radio(3,"three",value:bind(state.value),y:40,width:120,height:30,on_change:changed);
        let modal=stack(modal:true,visible:bind(state.open),width:600,height:400){first;last;};
        stage.replace(stack{outer;modal;text(derive(||f"state:{state.value}:{state.keys}:{state.changes}"),y:380);});
        narrator("ready");
    }"#;
        for mut d in drivers(source) {
            let (mut b, root) = bridge(&d);
            present(&mut d, &mut b, root, 1);
            click_at(&mut d, &mut b, 50., 55.);
            press(&mut d, &mut b, ArrowRight, false);
            assert_eq!(
                text(&d, "state:"),
                "state:0:0:1",
                "ancestor on_key cannot take the radio arrow"
            );
            present(&mut d, &mut b, root, 2);
            click_at(&mut d, &mut b, 250., 15.);
            present(&mut d, &mut b, root, 3);
            press(&mut d, &mut b, Tab, false);
            press(&mut d, &mut b, ArrowLeft, false);
            assert_eq!(
                text(&d, "state:"),
                "state:3:0:2",
                "group wrapping stays inside the modal"
            );
            present(&mut d, &mut b, root, 4);
            press(&mut d, &mut b, ArrowRight, false);
            assert_eq!(text(&d, "state:"), "state:2:0:3");
        }
    }

    #[test]
    fn virtual_radio_tab_does_not_build_rows_and_direction_resumes_after_acceptance() {
        use keyboard::NamedKey::*;
        let source = r#"
    fn main(){
        let items=[];for i in 0..10000 {items.append(i);}
        let state=model({selected:0,changes:0});
        let rows=list(||items,key:|item|item,virtual:true,build:|row|
            radio(row.value,f"item {row.value}",value:bind(state.selected),width:180,height:40,
                on_change:||{state.changes+=1;}));
        let after=button("After",width:120,height:30,y:180);
        stage.replace(stack{scroll(width:220,height:120){rows;}after;
            text(derive(||f"state:{state.selected}:{state.changes}"),y:340);
            text(derive(||if after.focused {"focus:after"} else {"focus:list"}),y:380);
        });narrator("ready");
    }"#;
        for total in [1000, 10000] {
            for mut d in drivers(&source.replace("0..10000", &format!("0..{total}"))) {
                let (mut b, root) = bridge(&d);
                let mut frame = 0;
                let flush =
                    |d: &mut AsideSessionUi, b: &mut PresentationBridge, frame: &mut u64| {
                        for _ in 0..6 {
                            *frame += 1;
                            present(d, b, root, *frame);
                            settle(d);
                        }
                    };
                flush(&mut d, &mut b, &mut frame);
                let constructed = d.adapter().stage().next_identity().unwrap();
                let mounted = d.adapter().stage().nodes().count();
                assert!(mounted < 30);
                press(&mut d, &mut b, Tab, false);
                let start = std::time::Instant::now();
                press(&mut d, &mut b, Tab, false);
                let elapsed = start.elapsed();
                assert_eq!(text(&d, "focus:"), "focus:after");
                flush(&mut d, &mut b, &mut frame);
                assert_eq!(
                    d.adapter().stage().next_identity().unwrap(),
                    constructed,
                    "Tab must not call the row factory to search for an exit"
                );
                eprintln!(
                    "{total} radio items: mounted={mounted}, Tab input and VM drain={elapsed:?}, added nodes=0"
                );
                press(&mut d, &mut b, Tab, true);
                assert_eq!(text(&d, "focus:"), "focus:list");
                // Reach the accepted mounted edge without accepting a new frame.
                let mut waiting = None;
                for _ in 0..30 {
                    let before = text(&d, "state:");
                    press(&mut d, &mut b, ArrowDown, false);
                    if text(&d, "state:") == before {
                        assert_eq!(
                            text(&d, "state:"),
                            before,
                            "unpresented target must not select"
                        );
                        waiting = Some(before);
                        break;
                    }
                }
                let waiting = waiting.unwrap_or_else(|| {
                    panic!(
                        "navigation reaches a virtual gap: {} focus={:?}",
                        text(&d, "state:"),
                        b.root().focused_widget()
                    )
                });
                let selected: usize = waiting.split(':').nth(1).unwrap().parse().unwrap();
                flush(&mut d, &mut b, &mut frame);
                assert_eq!(
                    text(&d, "state:"),
                    format!("state:{}:{}", selected + 1, selected + 1)
                );
                for expected in selected + 2..=25 {
                    press(&mut d, &mut b, ArrowDown, false);
                    flush(&mut d, &mut b, &mut frame);
                    assert_eq!(text(&d, "state:"), format!("state:{expected}:{expected}"));
                }
                assert!(d.adapter().stage().nodes().count() < 40);
                let mut waiting = None;
                for _ in 0..30 {
                    let before = text(&d, "state:");
                    press(&mut d, &mut b, ArrowDown, false);
                    if text(&d, "state:") == before {
                        waiting = Some(before);
                        break;
                    }
                }
                let waiting = waiting.expect("second deferred boundary");
                press(&mut d, &mut b, Tab, false);
                flush(&mut d, &mut b, &mut frame);
                assert_eq!(
                    text(&d, "state:"),
                    waiting,
                    "Tab cancels the pending selection"
                );
                assert_eq!(text(&d, "focus:"), "focus:after");
            }
        }
    }

    #[test]
    fn virtual_radio_wraps_at_logical_ends_without_building_intermediate_rows() {
        use keyboard::NamedKey::*;
        let source = r#"
    fn main(){
        let items=[];for i in 0..10000 {items.append(i);}
        let state=model({selected:0,changes:0,ready:false});
        let rows=list(||items,key:|item|item,virtual:true,build:|row|
            radio(row.value,f"item {row.value}",value:bind(state.selected),width:180,height:40,
                on_change:||{state.changes+=1;}));
        let jump=button("Last",width:120,height:30,y:180,on_click:||{
            await rows.scroll_to(9999,align:"end");state.selected=9999;state.ready=true;
        });
        stage.replace(stack{scroll(width:220,height:120){rows;}jump;
            text(derive(||f"state:{state.selected}:{state.changes}:{state.ready}"),y:340);
            text(derive(||if jump.focused {"focus:jump"} else {"focus:list"}),y:380);
        });narrator("ready");
    }"#;
        for mut d in drivers(source) {
            let (mut b, root) = bridge(&d);
            let mut frame = 0;
            let flush = |d: &mut AsideSessionUi, b: &mut PresentationBridge, frame: &mut u64| {
                for _ in 0..12 {
                    *frame += 1;
                    present(d, b, root, *frame);
                    settle(d);
                }
            };
            flush(&mut d, &mut b, &mut frame);
            let initial = d.adapter().stage().next_identity().unwrap();
            click_at(&mut d, &mut b, 60., 195.);
            flush(&mut d, &mut b, &mut frame);
            assert_eq!(text(&d, "state:"), "state:9999:0:true");
            press(&mut d, &mut b, Tab, false);
            assert_eq!(text(&d, "focus:"), "focus:list");
            press(&mut d, &mut b, ArrowDown, false);
            assert_eq!(
                text(&d, "state:"),
                "state:9999:0:true",
                "wrap waits for the opposite accepted window"
            );
            flush(&mut d, &mut b, &mut frame);
            assert_eq!(text(&d, "state:"), "state:0:1:true");
            press(&mut d, &mut b, ArrowUp, false);
            assert_eq!(text(&d, "state:"), "state:0:1:true");
            flush(&mut d, &mut b, &mut frame);
            assert_eq!(text(&d, "state:"), "state:9999:2:true");
            assert!(
                d.adapter().stage().next_identity().unwrap().0 - initial.0 < 80,
                "only endpoint windows are constructed"
            );
            assert!(d.adapter().stage().nodes().count() < 40);
            press(&mut d, &mut b, ArrowDown, false);
            press(&mut d, &mut b, Tab, false);
            flush(&mut d, &mut b, &mut frame);
            assert_eq!(
                text(&d, "state:"),
                "state:9999:2:true",
                "leaving cancels an opposite-end request"
            );
            assert_eq!(text(&d, "focus:"), "focus:jump");
        }
    }

    #[test]
    fn radio_group_crosses_distinct_virtual_lists_at_their_logical_ends() {
        use keyboard::NamedKey::*;
        let source = r#"
    fn main(){
        let left_items=[];let right_items=[];
        for i in 0..1000 {left_items.append(i);right_items.append(i+1000);}
        let state=model({selected:0,changes:0});
        let make=|row|radio(row.value,f"item {row.value}",value:bind(state.selected),width:180,height:40,
            on_change:||{state.changes+=1;});
        let left=list(||left_items,key:|item|item,virtual:true,build:make);
        let right=list(||right_items,key:|item|item,virtual:true,build:make);
        let prepare=button("Prepare",width:120,height:30,y:180,on_click:||{
            await right.scroll_to(1500);await left.scroll_to(999,align:"end");state.selected=999;
        });
        let last=button("Last",width:120,height:30,y:220,on_click:||{
            await right.scroll_to(1999,align:"end");state.selected=1999;
        });
        stage.replace(stack{
            scroll(width:220,height:120){left;}
            scroll(x:250,width:220,height:120){right;}
            prepare;last;text(derive(||f"state:{state.selected}:{state.changes}"),y:340);
        });narrator("ready");
    }"#;
        for mut d in drivers(source) {
            let (mut b, root) = bridge(&d);
            let mut frame = 0;
            let flush = |d: &mut AsideSessionUi, b: &mut PresentationBridge, frame: &mut u64| {
                for _ in 0..16 {
                    *frame += 1;
                    present(d, b, root, *frame);
                    settle(d);
                }
            };
            flush(&mut d, &mut b, &mut frame);
            let initial = d.adapter().stage().next_identity().unwrap().0;
            click_at(&mut d, &mut b, 60., 195.);
            flush(&mut d, &mut b, &mut frame);
            assert_eq!(text(&d, "state:"), "state:999:0");
            press(&mut d, &mut b, Tab, true);
            press(&mut d, &mut b, ArrowDown, false);
            assert_eq!(text(&d, "state:"), "state:999:0");
            flush(&mut d, &mut b, &mut frame);
            assert_eq!(
                text(&d, "state:"),
                "state:1000:1",
                "enter the next list at its logical first row"
            );
            press(&mut d, &mut b, ArrowUp, false);
            flush(&mut d, &mut b, &mut frame);
            assert_eq!(text(&d, "state:"), "state:999:2");
            click_at(&mut d, &mut b, 60., 235.);
            flush(&mut d, &mut b, &mut frame);
            assert_eq!(text(&d, "state:"), "state:1999:2");
            press(&mut d, &mut b, Tab, false);
            press(&mut d, &mut b, ArrowDown, false);
            flush(&mut d, &mut b, &mut frame);
            assert_eq!(text(&d, "state:"), "state:0:3", "wrap across both lists");
            press(&mut d, &mut b, ArrowUp, false);
            flush(&mut d, &mut b, &mut frame);
            assert_eq!(text(&d, "state:"), "state:1999:4");
            assert!(d.adapter().stage().next_identity().unwrap().0 - initial < 150);
            assert!(d.adapter().stage().nodes().count() < 60);
        }
    }

    #[test]
    fn radio_discovers_its_group_beyond_another_virtual_lists_mounted_rows() {
        use keyboard::NamedKey::*;
        let source = r#"
    fn main(){
        let state=model({selected:0,other:-1,changes:0});
        let right_items=[];for i in 0..40 {right_items.append(i);}
        let left=list(||[0],key:|item|item,virtual:true,build:|row|
            radio(0,"left",value:bind(state.selected),width:180,height:40,
                on_change:||{state.changes+=1;}));
        let right=list(||right_items,key:|item|item,virtual:true,build:|row|{
            if row.value==32 {
                radio(32,"right",value:bind(state.selected),width:180,height:40,
                    on_change:||{state.changes+=1;})
            }else if row.value%2==0 {
                text("header",width:180,height:40)
            }else{
                radio(row.value,"another group",value:bind(state.other),width:180,height:40)
            }
        });
        stage.replace(stack{
            scroll(width:220,height:120){left;}
            scroll(x:260,width:220,height:120){right;}
            text(derive(||f"state:{state.selected}:{state.other}:{state.changes}"),y:340);
        });narrator("ready");
    }"#;
        for mut d in drivers(source) {
            let (mut b, root) = bridge(&d);
            let mut frame = 0;
            for _ in 0..6 {
                frame += 1;
                present(&mut d, &mut b, root, frame);
                settle(&mut d);
            }
            assert!(
                !d.adapter().stage().nodes().any(|node| matches!(&node.kind,
            NodeKind::Radio{label:Some(label),..} if label=="right")),
                "the target starts unconstructed"
            );
            press(&mut d, &mut b, Tab, false);
            press(&mut d, &mut b, ArrowDown, false);
            for _ in 0..120 {
                frame += 1;
                present(&mut d, &mut b, root, frame);
                settle(&mut d);
                if text(&d, "state:") == "state:32:-1:1" {
                    break;
                }
            }
            assert_eq!(
                text(&d, "state:"),
                "state:32:-1:1",
                "navigation discovers the group before wrapping to the left list"
            );
            assert!(
                d.adapter().stage().nodes().count() < 45,
                "only the current windows and pinned anchor stay mounted"
            );
            for (expected, changes) in [(0, 2), (32, 3)] {
                press(&mut d, &mut b, ArrowUp, false);
                for _ in 0..120 {
                    frame += 1;
                    present(&mut d, &mut b, root, frame);
                    settle(&mut d);
                    if text(&d, "state:") == format!("state:{expected}:-1:{changes}") {
                        break;
                    }
                }
                assert_eq!(
                    text(&d, "state:"),
                    format!("state:{expected}:-1:{changes}"),
                    "reverse traversal and wrapping use logical endpoints"
                );
            }
        }
    }
}
