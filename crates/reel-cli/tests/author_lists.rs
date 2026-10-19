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
        reel_cli::decode_story(&artifact.bytes).unwrap(),
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
        reel_cli::decode_story(&artifact.bytes).unwrap(),
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
        reel_cli::decode_story(&artifact.bytes).unwrap(),
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
fn creator_ui_examples_initialize_from_source_and_asb() {
    for (name, source) in [
        (
            "author_fitted_text.aside",
            include_str!("../../../examples/author_fitted_text.aside"),
        ),
        (
            "author_tasks.aside",
            include_str!("../../../examples/author_tasks.aside"),
        ),
        (
            "author_class_models.aside",
            include_str!("../../../examples/author_class_models.aside"),
        ),
        (
            "author_random.aside",
            include_str!("../../../examples/author_random.aside"),
        ),
        (
            "author_list.aside",
            include_str!("../../../examples/author_list.aside"),
        ),
        (
            "author_narrative.aside",
            include_str!("../../../examples/author_narrative.aside"),
        ),
        (
            "author_reading.aside",
            include_str!("../../../examples/author_reading.aside"),
        ),
        (
            "author_text_input.aside",
            include_str!("../../../examples/author_text_input.aside"),
        ),
        (
            "author_timing.aside",
            include_str!("../../../examples/author_timing.aside"),
        ),
        (
            "author_keyboard.aside",
            include_str!("../../../examples/author_keyboard.aside"),
        ),
        (
            "match_values.aside",
            include_str!("../../../examples/match_values.aside"),
        ),
    ] {
        let checked = reel_cli::check(source, name).unwrap();
        let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
        for compiled in [
            reel_cli::into_story(checked, "main::main").unwrap(),
            reel_cli::decode_story(&artifact.bytes).unwrap(),
        ] {
            let mut driver = AsideSessionUi::from_compiled(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                compiled,
                "en",
                VmLimits::default(),
            )
            .unwrap();
            settle(&mut driver);
            assert!(driver.adapter().stage().root().is_some(), "{name}");
            driver.cancel().unwrap();
        }
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
        reel_cli::decode_story(&artifact.bytes).unwrap(),
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
            NodeKind::TextInput { value } if value.as_ref() == "你x")));
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
        reel_cli::decode_story(&artifact.bytes).unwrap(),
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
        include_str!("../../../examples/author_list_gallery.aside"),
        "natural-gallery.aside",
    )
    .unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for compiled in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_cli::decode_story(&artifact.bytes).unwrap(),
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
